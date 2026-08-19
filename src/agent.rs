//! Agent mode: a `kind: chat` profile, run in a loop.
//!
//! Render, call, decode; if the stop condition is not met, answer the tool calls
//! with their simulated results, feed them back, and go round again. There is no
//! third payload format and no second profile — `POST /api/call` runs one turn of
//! exactly the same thing.
//!
//! # What is being checked
//!
//! Not that the model does useful work. That it emits tool calls matching the
//! schema it was given, and that it knows what to do with a result. The tools are
//! simulated: a fixed string, or a Rhai script that can at least look at the
//! arguments. Nothing is executed.
//!
//! # Never a silent loop
//!
//! Every way out is named. The one worth spelling out is
//! [`StopOutcome::PredicateNeverEvaluable`]: a backend that never emits a
//! `finish_reason` would let a profile stopping on `finish_reason_in` run to
//! `max_iterations` and look like a slow agent. It is not — the configured
//! predicate could never be evaluated even once, and that is what gets reported.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use jsonschema::Validator;
use rhai::Scope;
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::config::Config;
use crate::decode::Decoded;
use crate::exec::{CallInput, CallOutcome, ExecError, Runner};
use crate::mcp::{
    HookJournal, HookRecord, McpClient, McpCredentials, McpError, McpExchange, McpJournal,
    McpRegistry, McpTool, Revision,
};
use crate::message::{Message, Role, ToolCall};
use crate::profile::{AgentSpec, Profile, ProfileKind, StopWhen, ToolResponse, ToolSpec};
use crate::script::ScriptError;
use crate::uploads::UploadRef;
use crate::vars::Vars;

/// Turns allowed when the profile says nothing.
const DEFAULT_MAX_ITERATIONS: u32 = 10;

/// Wall-clock ceiling when the profile says nothing. Generous: a small model on
/// a CPU is slow, and a timeout that fires on a working agent is worse than one
/// that fires late.
const DEFAULT_MAX_DURATION: Duration = Duration::from_secs(300);

/// Why the loop stopped. Every variant is a complete explanation.
#[derive(Debug, Clone, Serialize, JsonSchema)]
// `rename_all` renames the *variants*; the struct-variant fields need
// `rename_all_fields` as well, or `at_turn` goes out snake_case.
#[serde(
    tag = "outcome",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum StopOutcome {
    /// A configured predicate held.
    Stopped {
        /// Which one.
        reason: StopReason,
    },
    /// The turn budget ran out with the predicates evaluable but never true.
    MaxIterations {
        /// The budget.
        limit: u32,
    },
    /// The time budget ran out.
    Deadline {
        /// How long the loop had run.
        after_ms: u64,
    },
    /// The model asked for the same thing twice — a loop, not progress. Only
    /// ever reported by a profile that set `stop_when.repeated_call`.
    RepeatedCall {
        /// Tool that was called again with identical arguments.
        tool: String,
        /// Turn it happened on.
        at_turn: u32,
    },
    /// The configured stop condition could never be evaluated, not even once.
    ///
    /// The loop is not slow — it is unfalsifiable. Almost always a backend that
    /// does not report `finish_reason` at all.
    PredicateNeverEvaluable {
        /// The predicate that never had anything to work with.
        predicate: &'static str,
        /// Turns that went by.
        turns: u32,
    },
}

/// Which predicate ended the loop.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(
    tag = "predicate",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum StopReason {
    /// The turn produced no tool call.
    NoToolCalls,
    /// `finish_reason` was one of the terminal values.
    FinishReason {
        /// The value that matched.
        value: String,
    },
}

/// What the loop decided at the end of a turn.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(
    tag = "decision",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Decision {
    /// Tool results were fed back and another turn follows.
    Continue {
        /// How many tools answered.
        tools: usize,
    },
    /// The loop ended here.
    Stop {
        /// Why.
        stop: StopOutcome,
    },
}

/// One simulated tool call: what the model asked for, and what it got back.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ToolInvocation {
    /// The call, as the model emitted it.
    pub call: ToolCall,
    /// Where the answer came from. `simulated` means nothing happened outside
    /// this process; `mcp` means it did.
    pub source: ToolSource,
    /// MCP server that ran it, when one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// Round trip to that server, in milliseconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// The tool ran and reported a problem (`isError`). Not a failure of `mire`,
    /// and not a failure of the loop — a result the model is meant to react to,
    /// exactly like a `4xx` from an endpoint under test.
    pub reported_error: bool,
    /// Why the arguments do not match the declared schema. Empty means they do —
    /// which is one of the two things agent mode exists to check.
    pub schema_errors: Vec<String>,
    /// What was fed back to the model.
    pub result: String,
    /// Set when the tool could not answer at all: unknown name, or a script that
    /// failed. The model still gets told, so it has a chance to recover.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// What this call put in the run's variables, per `agent.capture`.
    ///
    /// Reported per call rather than only as the run's final bag: a variable is
    /// a fact about one tool call, and a capture that quietly matched nothing is
    /// exactly what somebody staring at an empty `vars` needs to see.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub captured: crate::vars::Captured,
}

/// Where a tool's answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ToolSource {
    /// Declared in the profile. Nothing is executed.
    Simulated,
    /// Declared by an MCP server, and really called.
    Mcp,
}

/// One turn: the whole exchange, plus what was decided about it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    /// 1-based turn number.
    pub index: u32,
    /// The request, the response and the decode — the same shape `POST /api/call`
    /// returns, credentials already masked.
    pub call: Box<CallOutcome>,
    /// Tools answered at the end of this turn.
    pub tools: Vec<ToolInvocation>,
    /// Every JSON-RPC round trip the tools above took, request and response.
    ///
    /// A [`ToolInvocation`] says what the tool was asked and what it answered;
    /// this says what actually went over the wire to get there, which is the only
    /// place a `401` from the server or a lost session is visible at all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<McpExchange>,
    /// Every hook that fired around the tools above.
    ///
    /// Its own list rather than an entry in `mcp`: a hook talks to a third party
    /// over plain HTTP, and filing a webhook's `POST` among the JSON-RPC methods
    /// would make the MCP traffic unreadable to make one number go up.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookRecord>,
    /// Continue, or stop and why.
    pub decision: Decision,
}

/// Everything one agent run produced.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Trace {
    /// Profile that ran.
    pub profile: String,
    /// Auth provider that ran.
    pub auth: String,
    /// What was said to the MCP servers before the first prompt was spent:
    /// discovery, the handshake, `tools/list`.
    ///
    /// Before any turn, because it happens before any turn — and because a run
    /// that died here has no turns at all, which is exactly when this is the
    /// whole story.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<McpExchange>,
    /// Every turn, in order.
    pub turns: Vec<Turn>,
    /// How it ended.
    pub stop: StopOutcome,
    /// Wall-clock time for the whole loop.
    pub duration_ms: u64,
}

/// What one agent run needs.
#[derive(Debug, Default)]
pub struct AgentInput {
    /// The single-turn input.
    pub call: CallInput,
    /// Turn budget, overriding the profile's.
    pub max_iterations: Option<u32>,
    /// Which of the declared MCP servers this run may reach.
    ///
    /// `None` is all of them: a server is declared once, in `mcp.yaml`, and every
    /// `kind: chat` profile is offered the lot. A list narrows that to the ones
    /// named — down to none at all, which is how you ask what the model does when
    /// the tool it wants is not there. A name `mcp.yaml` does not declare is
    /// [`McpError::UnknownServer`], because it is a typo rather than a server
    /// this run gets to invent.
    pub mcp_servers: Option<Vec<String>>,
    /// Revision to speak to every MCP server this run touches, overriding both
    /// the negotiation and any `protocol_version:` in `mcp.yaml`.
    ///
    /// `None` is the negotiation as it stands. Set it when the revision is what
    /// you are testing and you would rather not edit a file between two runs —
    /// the answer a server gives on `2025-03-26` is a different fact from the one
    /// it gives on `2026-07-28`, and both are worth a button.
    pub mcp_protocol: Option<Revision>,
}

/// Why an agent run could not be performed at all.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Agent mode is a way of running a chat profile, and only a chat profile.
    #[error("profile `{profile}` is `kind: embedding`; agent mode runs a `kind: chat` profile")]
    NotChat {
        /// The profile that was asked for.
        profile: String,
    },

    /// A turn failed. Whatever went wrong on turn one goes wrong on turn two.
    #[error(transparent)]
    Turn(#[from] ExecError),

    /// The run's MCP servers could not be resolved before the loop started.
    ///
    /// Only setup failures land here — a tool that fails *during* the loop is fed
    /// back to the model, because recovering from it is what is being tested.
    #[error(transparent)]
    Mcp(#[from] McpError),

    /// The profile's `agent.capture:` names a set `captures.yaml` does not
    /// declare.
    ///
    /// Stops the run rather than capturing less than the profile asked for: the
    /// variables that set was meant to fill are read by hooks and by server
    /// headers, and a run that goes ahead without them fails later, further away,
    /// in a rendered URL.
    #[error(
        "profile `{profile}` names the capture set `{set}`, which `captures.yaml` does not declare"
    )]
    UnknownCaptureSet {
        /// The profile that was run.
        profile: String,
        /// The set it asked for.
        set: String,
    },
}

/// Something worth reporting before the run is over.
///
/// An enum rather than a second callback because there is an order to these and
/// a single stream is what preserves it: the setup traffic really did happen
/// before turn one, and a client rebuilding a timeline should not have to know
/// that on its own.
#[derive(Debug, Clone, Copy)]
pub enum AgentUpdate<'a> {
    /// What listing the tools cost, before the first prompt was spent.
    Setup(&'a [McpExchange]),
    /// A turn completed.
    Turn(&'a Turn),
}

/// Runs the loop, handing each update to `on_update` as it happens.
///
/// The callback is what makes the API able to stream: turns arrive as they
/// happen rather than after the run.
///
/// # Errors
///
/// Fails when the profile is not a chat profile, or when an exchange itself
/// fails. A model that misbehaves is a [`StopOutcome`], not an error.
pub async fn run(
    runner: &Runner,
    mut input: AgentInput,
    mut on_update: impl FnMut(AgentUpdate<'_>),
) -> Result<Trace, AgentError> {
    let Prepared {
        profile,
        spec,
        limit,
        deadline,
        tools,
        setup,
    } = prepare(runner, &mut input).await?;

    if !setup.is_empty() {
        on_update(AgentUpdate::Setup(&setup));
    }

    let started = Instant::now();
    let mut messages = input.call.messages.clone();
    let mut turns: Vec<Turn> = Vec::new();
    let mut seen_calls: HashSet<String> = HashSet::new();
    let mut predicate_ever_evaluable = spec.stop_when.no_tool_calls;
    let mut auth_name = String::new();

    let stop = loop {
        let index = u32::try_from(turns.len()).unwrap_or(u32::MAX) + 1;

        if let Some(outcome) = budget_exhausted(
            started,
            deadline,
            index,
            limit,
            predicate_ever_evaluable,
            &spec.stop_when,
        ) {
            break outcome;
        }

        let outcome = runner
            .call(CallInput {
                messages: messages.clone(),
                ..clone_input(&input.call)
            })
            .await?;
        auth_name.clone_from(&outcome.auth);

        let completion = completion_of(&outcome);

        if completion.finish_reason.is_some() && !spec.stop_when.finish_reason_in.is_empty() {
            predicate_ever_evaluable = true;
        }

        // Stop before spending a turn answering tools nobody will read.
        if let Some(reason) = should_stop(&spec.stop_when, &completion) {
            let turn = record(
                index,
                outcome,
                Vec::new(),
                &tools,
                Decision::Stop {
                    stop: StopOutcome::Stopped {
                        reason: reason.clone(),
                    },
                },
            );
            on_update(AgentUpdate::Turn(&turn));
            turns.push(turn);
            break StopOutcome::Stopped { reason };
        }

        // The model wants tools. Answer them, and — when the profile asked for
        // it — watch for it asking twice.
        let repeated = spec
            .stop_when
            .repeated_call
            .then(|| detect_repeat(&mut seen_calls, &completion.tool_calls))
            .flatten();

        let invocations = invoke_tools(&tools, &completion.tool_calls, index).await;
        let decision = decide(repeated.as_ref(), invocations.len(), index);

        let turn = record(index, outcome, invocations, &tools, decision);
        on_update(AgentUpdate::Turn(&turn));

        if let Some(tool) = repeated {
            warn!(profile = %profile.name, %tool, turn = index, "the model asked for the same thing twice");
            turns.push(turn);
            break StopOutcome::RepeatedCall {
                tool,
                at_turn: index,
            };
        }

        feed_back(&mut messages, &completion, &turn.tools);
        debug!(profile = %profile.name, turn = index, tools = turn.tools.len(), "continuing");
        turns.push(turn);
    };

    info!(
        profile = %profile.name,
        turns = turns.len(),
        outcome = ?std::mem::discriminant(&stop),
        duration_ms = elapsed_ms(started),
        "agent run finished"
    );

    Ok(Trace {
        profile: profile.name.clone(),
        auth: auth_name,
        setup,
        turns,
        stop,
        duration_ms: elapsed_ms(started),
    })
}

/// One turn, with everything it put on a wire attached.
///
/// The journals are drained *here*, after the tools have run, so a turn holds
/// exactly what it produced and the next one starts from nothing.
fn record(
    index: u32,
    call: CallOutcome,
    invocations: Vec<ToolInvocation>,
    tools: &Tools,
    decision: Decision,
) -> Turn {
    Turn {
        index,
        call: Box::new(call),
        tools: invocations,
        mcp: crate::mcp::drain(&tools.journal),
        hooks: crate::mcp::hook::drain(&tools.hooks),
        decision,
    }
}

/// What to do after a turn that asked for tools.
///
/// `repeated` is only ever `Some` under `stop_when.repeated_call`, where the
/// same tool with the same arguments twice is read as a loop rather than
/// progress — the only way that shows up as a finding instead of as a run that
/// merely took a while.
fn decide(repeated: Option<&String>, tools: usize, index: u32) -> Decision {
    repeated.map_or(Decision::Continue { tools }, |tool| Decision::Stop {
        stop: StopOutcome::RepeatedCall {
            tool: tool.clone(),
            at_turn: index,
        },
    })
}

/// Appends the assistant turn and one message per tool result, which is what
/// the next turn's template renders.
fn feed_back(
    messages: &mut Vec<Message>,
    completion: &crate::decode::Completion,
    tools: &[ToolInvocation],
) {
    messages.push(Message {
        role: Role::Assistant,
        content: completion.content.clone(),
        tool_calls: completion.tool_calls.clone(),
        tool_call_id: None,
    });
    for invocation in tools {
        messages.push(Message {
            role: Role::Tool,
            content: Some(invocation.result.clone()),
            tool_calls: Vec::new(),
            tool_call_id: invocation.call.id.clone(),
        });
    }
}

/// The budget checks that run before a turn is even attempted.
fn budget_exhausted(
    started: Instant,
    deadline: Duration,
    index: u32,
    limit: u32,
    predicate_ever_evaluable: bool,
    stop_when: &StopWhen,
) -> Option<StopOutcome> {
    if started.elapsed() >= deadline {
        return Some(StopOutcome::Deadline {
            after_ms: elapsed_ms(started),
        });
    }
    if index > limit {
        return Some(if predicate_ever_evaluable {
            StopOutcome::MaxIterations { limit }
        } else {
            StopOutcome::PredicateNeverEvaluable {
                predicate: describe_predicate(stop_when),
                turns: limit,
            }
        });
    }
    None
}

/// What the loop needs, resolved once.
struct Prepared {
    profile: std::sync::Arc<Profile>,
    spec: AgentSpec,
    limit: u32,
    deadline: Duration,
    tools: Tools,
    /// What listing the tools cost in MCP traffic, before the loop started.
    setup: Vec<McpExchange>,
}

/// Everything needed to answer a tool call, from either source.
struct Tools {
    profile: std::sync::Arc<Profile>,
    /// One entry per offered tool, simulated and live alike: a real server's
    /// `inputSchema` is checked exactly like a declared one.
    validators: Vec<(String, Option<Validator>)>,
    live: Vec<McpTool>,
    config: std::sync::Arc<Config>,
    /// This run's client per server, keyed by registry name.
    ///
    /// Built once, in [`prepare`], and reused for every call the loop makes.
    /// Looking one up again per tool call would be a second client with a second
    /// settled state — and on a run that chose its revision, a second handshake
    /// before every single tool.
    clients: BTreeMap<String, McpClient>,
    /// What this run's tool calls have captured, and the rules that fill it.
    ///
    /// Shared with every client the run built, so a variable a simulated tool
    /// set is one a live server's hook can read. Not drained per turn, unlike
    /// the journals beside it: carrying a value across turns is the point.
    vars: Arc<Vars>,
    /// This run's MCP traffic, drained into each turn as it completes.
    journal: McpJournal,
    /// Where the hooks that fired are collected, drained per turn beside it.
    hooks: HookJournal,
}

impl Tools {
    /// Every name the model may call, for the message when it invents one.
    fn known(&self) -> Vec<&str> {
        self.profile
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .chain(self.live.iter().map(|tool| tool.name.as_str()))
            .collect()
    }

    fn validator(&self, name: &str) -> Option<&Validator> {
        self.validators
            .iter()
            .find(|(candidate, _)| candidate == name)
            .and_then(|(_, validator)| validator.as_ref())
    }
}

/// The MCP servers a run will actually set up.
///
/// `None` is every server `mcp.yaml` declares — the file is the opt-in, and it
/// grants the lot to every `kind: chat` profile. `requested` narrows that to the
/// ones named. Shared by the API handler, which wants the refusal before it opens
/// a stream, and by [`prepare`], which is the one that acts on it: two readings of
/// the same rule are two readings that can disagree.
///
/// # Errors
///
/// Fails when `requested` names a server `mcp.yaml` does not declare.
pub fn selected_servers(
    registry: &McpRegistry,
    requested: Option<&[String]>,
) -> Result<Vec<String>, McpError> {
    let declared = registry.names();
    let Some(asked) = requested else {
        return Ok(declared);
    };

    for server in asked {
        if !declared.contains(server) {
            return Err(McpError::UnknownServer(server.clone()));
        }
    }
    // The registry's order, not the caller's: the setup traffic reads better when
    // a run always lists its servers the same way round.
    Ok(declared
        .into_iter()
        .filter(|server| asked.contains(server))
        .collect())
}

/// Resolves the profile, the budgets and the tools, or explains why the run
/// cannot happen.
///
/// Listing the MCP tools happens here, once, rather than per turn: a server that
/// is unreachable should stop the run before the first prompt is spent, and the
/// model must be offered the same tools on every turn.
async fn prepare(runner: &Runner, input: &mut AgentInput) -> Result<Prepared, AgentError> {
    let config = runner.config().snapshot();
    let profile = config
        .profiles
        .get(&input.call.profile)
        .ok_or_else(|| ExecError::UnknownProfile(input.call.profile.clone()))?
        .clone();

    if profile.kind != ProfileKind::Chat {
        return Err(AgentError::NotChat {
            profile: profile.name.clone(),
        });
    }

    // Recorded from the first probe: a run that cannot get past `initialize` has
    // no turns to hang the reason off, and the `?` below is where it ends.
    let journal: McpJournal = McpJournal::default();
    let hooks: HookJournal = HookJournal::default();
    let attachments: Arc<[UploadRef]> = Arc::from(input.call.uploads.clone());

    // Resolved before the clients are built, because each of them is handed the
    // bag its calls will fill. The `use:` entries are expanded here rather than
    // at load time, for the same reason an unknown server name is caught here: a
    // profile is read on its own, and the registry beside it is a separate file
    // with its own edits.
    let spec = profile.agent.clone().unwrap_or_else(default_spec);
    let rules = config.captures.resolve(&spec.capture).map_err(|missing| {
        AgentError::UnknownCaptureSet {
            profile: profile.name.clone(),
            set: missing.0,
        }
    })?;
    let vars = Vars::new(rules);

    let mut live = Vec::new();
    let mut clients = BTreeMap::new();
    let servers = selected_servers(&config.mcp, input.mcp_servers.as_deref())?;
    let declared = config.mcp.descriptors().len();
    if servers.len() != declared {
        // Worth a line of its own: a run offering the model fewer tools than
        // `mcp.yaml` declares is the first thing to check when it stops calling
        // one.
        info!(
            profile = %profile.name,
            declared,
            reaching = servers.len(),
            "MCP servers narrowed for this run"
        );
    }
    for server in &servers {
        // The client this run will use for everything it says to this server:
        // the revision it was told to speak, and the journal it is recorded in.
        let client = config
            .mcp
            .get(server)
            .ok_or_else(|| McpError::UnknownServer(server.clone()))?
            .speaking(input.mcp_protocol)
            .recording(journal.clone(), hooks.clone())
            // Shared across servers on purpose: a run has one set of variables,
            // however many servers it ends up talking to.
            .capturing(Arc::clone(&vars))
            // The same files the template gets, for a hook that asked for some.
            // Shared rather than copied per server: a run's uploads are one set
            // of bytes however many servers it ends up talking to.
            .carrying(Arc::clone(&attachments));
        let credentials = McpCredentials::resolve(&config.registry, client.server()).await?;
        let listed = client.list_tools(&credentials).await?;
        info!(profile = %profile.name, %server, tools = listed.len(), "MCP tools offered");
        live.extend(listed);
        clients.insert(server.clone(), client);
    }

    let validators = compile_validators(&profile.tools, &live);

    // The live tools go straight into the render context, so every turn offers
    // the model the same set.
    input.call.extra_tools = live
        .iter()
        // A simulated tool of the same name wins, so declaring it twice would
        // just confuse the model about which schema applies.
        .filter(|tool| !profile.tools.iter().any(|spec| spec.name == tool.name))
        .map(declare)
        .collect();

    Ok(Prepared {
        limit: input.max_iterations.unwrap_or(spec.max_iterations),
        deadline: spec
            .max_duration_ms
            .map_or(DEFAULT_MAX_DURATION, Duration::from_millis),
        setup: crate::mcp::drain(&journal),
        tools: Tools {
            profile: profile.clone(),
            validators,
            live,
            config,
            clients,
            vars,
            journal,
            hooks,
        },
        profile,
        spec,
    })
}

/// Looks up the auth provider an MCP server names.
/// One MCP tool, in the `OpenAI` function shape the templates already speak.
fn declare(tool: &McpTool) -> Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description.clone().unwrap_or_default(),
            "parameters": without_transport_keywords(&tool.input_schema),
        },
    })
}

/// Strips MCP's own schema extensions before the schema reaches the model.
///
/// `x-mcp-header` says how a parameter is mirrored into an HTTP header. That is
/// the client's business and the server's; it is noise in a prompt, and prompt
/// noise is not free on a small model. JSON Schema tolerates unknown keywords,
/// which is exactly why nothing would have complained.
fn without_transport_keywords(schema: &Value) -> Value {
    match schema {
        Value::Object(fields) => Value::Object(
            fields
                .iter()
                .filter(|(key, _)| key.as_str() != "x-mcp-header")
                .map(|(key, value)| (key.clone(), without_transport_keywords(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_transport_keywords).collect()),
        other => other.clone(),
    }
}

/// The decoded completion of a turn, or an empty one when the endpoint did not
/// answer with a chat completion at all.
///
/// A turn with nothing decodable is not a crash: it is a model that answered
/// something this profile cannot read, and the loop should stop on it rather
/// than pretend.
fn completion_of(outcome: &CallOutcome) -> crate::decode::Completion {
    match outcome.response.decoded.as_ref() {
        Some(Decoded::Completion(completion)) => completion.clone(),
        _ => crate::decode::Completion::default(),
    }
}

/// Records this turn's calls and reports the first one already seen.
///
/// Keyed on name *and* arguments, so asking about two different cities is
/// progress and asking about the same one twice is a loop.
fn detect_repeat(seen: &mut HashSet<String>, calls: &[ToolCall]) -> Option<String> {
    let mut repeated = None;
    for call in calls {
        if !seen.insert(fingerprint(call)) && repeated.is_none() {
            repeated = Some(call.name.clone());
        }
    }
    repeated
}

fn default_spec() -> AgentSpec {
    AgentSpec {
        stop_when: StopWhen::default(),
        max_iterations: DEFAULT_MAX_ITERATIONS,
        max_duration_ms: None,
        capture: Vec::new(),
    }
}

/// `CallInput` is not `Clone` because it holds a credential; the loop needs the
/// same input every turn, so it is rebuilt field by field on purpose.
fn clone_input(input: &CallInput) -> CallInput {
    CallInput {
        profile: input.profile.clone(),
        auth: input.auth.clone(),
        messages: Vec::new(),
        input: input.input.clone(),
        params: input.params.clone(),
        model: input.model.clone(),
        token: input.token.clone(),
        include_vectors: false,
        repeat: 1,
        tolerance: input.tolerance,
        extra_tools: input.extra_tools.clone(),
        // Every turn, because every turn re-renders the whole body from the
        // template: a file the first turn carried is a file the second one has
        // to carry again, or the model loses sight of it mid-run.
        uploads: input.uploads.clone(),
        // The loop reads tool calls out of a decoded answer, and a streamed
        // answer does not reassemble them. Agent mode calls whole.
        stream: false,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Names the configured predicate, for the report that says it never fired.
fn describe_predicate(stop_when: &StopWhen) -> &'static str {
    if stop_when.finish_reason_in.is_empty() {
        "stop_when"
    } else {
        "stop_when.finish_reason_in"
    }
}

/// Applies the configured predicates. Combined with OR: the first that holds wins.
fn should_stop(stop_when: &StopWhen, completion: &crate::decode::Completion) -> Option<StopReason> {
    if let Some(reason) = &completion.finish_reason
        && stop_when
            .finish_reason_in
            .iter()
            .any(|value| value == reason)
    {
        return Some(StopReason::FinishReason {
            value: reason.clone(),
        });
    }
    if stop_when.no_tool_calls && completion.tool_calls.is_empty() {
        return Some(StopReason::NoToolCalls);
    }
    None
}

/// A call's identity, for spotting the model asking for the same thing twice.
fn fingerprint(call: &ToolCall) -> String {
    format!(
        "{}({})",
        call.name,
        serde_json::to_string(&call.arguments).unwrap_or_default()
    )
}

/// Compiles every offered tool's argument schema once per run, from both sources.
fn compile_validators(tools: &[ToolSpec], live: &[McpTool]) -> Vec<(String, Option<Validator>)> {
    let declared = tools.iter().map(|tool| (tool.name.clone(), &tool.schema));
    let served = live
        .iter()
        .map(|tool| (tool.name.clone(), &tool.input_schema));

    declared
        .chain(served)
        .map(|(name, schema)| {
            let validator = jsonschema::validator_for(schema).ok();
            if validator.is_none() {
                warn!(tool = %name, "the tool's argument schema is not usable, arguments will not be checked");
            }
            (name, validator)
        })
        .collect()
}

/// Answers every tool call the model made, from whichever source declared it.
///
/// A simulated tool wins over a live one of the same name: that is how you stub
/// exactly one tool of an otherwise real server.
async fn invoke_tools(tools: &Tools, calls: &[ToolCall], turn: u32) -> Vec<ToolInvocation> {
    let mut invocations = Vec::with_capacity(calls.len());

    for call in calls {
        // Arguments that do not match are reported *and still answered*: the
        // model gets a chance to correct itself, and a real server gets to have
        // its own opinion about them.
        let schema_errors = tools
            .validator(&call.name)
            .map(|validator| {
                validator
                    .iter_errors(&call.arguments)
                    .map(|error| format!("{}: {error}", error.instance_path()))
                    .collect()
            })
            .unwrap_or_default();

        if let Some(spec) = tools.profile.tools.iter().find(|t| t.name == call.name) {
            invocations.push(simulated(&tools.vars, spec, call, turn, schema_errors));
        } else if let Some(tool) = tools.live.iter().find(|t| t.name == call.name) {
            invocations.push(live(tools, tool, call, schema_errors).await);
        } else {
            let message = format!(
                "no tool named `{}` is declared; this profile offers {:?}",
                call.name,
                tools.known()
            );
            invocations.push(ToolInvocation {
                call: call.clone(),
                source: ToolSource::Simulated,
                server: None,
                latency_ms: None,
                reported_error: false,
                schema_errors,
                result: format!("{{\"error\": {}}}", json_string(&message)),
                error: Some(message),
                captured: crate::vars::Captured::new(),
            });
        }
    }

    invocations
}

/// A tool the profile declares. Nothing leaves this process.
fn simulated(
    vars: &Vars,
    spec: &ToolSpec,
    call: &ToolCall,
    turn: u32,
    schema_errors: Vec<String>,
) -> ToolInvocation {
    let (result, error) = match answer(spec, call, turn) {
        Ok(result) => (result, None),
        Err(failure) => {
            let message = failure.to_string();
            (
                format!("{{\"error\": {}}}", json_string(&message)),
                Some(message),
            )
        }
    };

    // A simulated tool captures on exactly the same terms as a live one: what a
    // variable is worth does not depend on which of the two answered, and a
    // stubbed `create_session` is how somebody tries a capture rule out before
    // pointing it at a server. Nothing to read from a tool that failed, though —
    // that result is our error message, not the tool's answer.
    let captured = if error.is_none() {
        vars.capture(&call.name, None, &result)
    } else {
        crate::vars::Captured::new()
    };

    ToolInvocation {
        call: call.clone(),
        source: ToolSource::Simulated,
        server: None,
        latency_ms: None,
        reported_error: false,
        schema_errors,
        result,
        error,
        captured,
    }
}

/// A tool on a real MCP server. This one has effects.
async fn live(
    tools: &Tools,
    tool: &McpTool,
    call: &ToolCall,
    schema_errors: Vec<String>,
) -> ToolInvocation {
    let mut invocation = ToolInvocation {
        call: call.clone(),
        source: ToolSource::Mcp,
        server: Some(tool.server.clone()),
        latency_ms: None,
        reported_error: false,
        schema_errors,
        result: String::new(),
        error: None,
        captured: crate::vars::Captured::new(),
    };

    let outcome = async {
        let client = tools
            .clients
            .get(&tool.server)
            .ok_or_else(|| McpError::UnknownServer(tool.server.clone()))?;
        let credentials = McpCredentials::resolve(&tools.config.registry, client.server()).await?;
        client.call_tool(tool, &call.arguments, &credentials).await
    }
    .await;

    match outcome {
        Ok(result) => {
            invocation.latency_ms = Some(result.latency_ms);
            // The server's own `isError` is a result, not a failure: reacting to
            // it is exactly what agent mode is checking the model can do.
            invocation.reported_error = result.is_error;
            // Captured by the client, before its `after` hooks fired; carried up
            // here so the turn can report which call set what.
            invocation.captured = result.captured;
            invocation.result = result.text;
        }
        Err(failure) => {
            let message = failure.to_string();
            warn!(tool = %call.name, server = %tool.server, %message, "MCP call failed");
            invocation.result = format!("{{\"error\": {}}}", json_string(&message));
            invocation.error = Some(message);
        }
    }

    invocation
}

/// Produces one tool's result.
fn answer(spec: &ToolSpec, call: &ToolCall, turn: u32) -> Result<String, ScriptError> {
    match spec.answer() {
        Some(ToolResponse::Static(response)) => Ok(response.to_owned()),
        Some(ToolResponse::Script(script)) => {
            let mut scope = Scope::new();
            scope.push_dynamic("arguments", crate::script::to_dynamic(&call.arguments)?);
            scope.push("name", call.name.clone());
            scope.push("turn", i64::from(turn));

            let returned = script.run(&mut scope)?;
            if returned.is_string() {
                return returned
                    .into_string()
                    .map_err(|found| ScriptError::WrongShape {
                        found: found.to_owned(),
                        expected: "a string, a map or an array",
                    });
            }
            let value: Value = crate::script::from_dynamic(&returned, "a map or an array")?;
            Ok(value.to_string())
        }
        // Validation rejects this at load; reaching it means a profile arrived
        // some other way.
        None => Err(ScriptError::WrongShape {
            found: "nothing".to_owned(),
            expected: "a `response` or a `script`",
        }),
    }
}

fn json_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::Completion;

    fn completion(finish: Option<&str>, tools: &[&str]) -> Completion {
        Completion {
            content: None,
            tool_calls: tools
                .iter()
                .map(|name| ToolCall {
                    id: None,
                    name: (*name).to_owned(),
                    arguments: serde_json::json!({}),
                    arguments_as_text: false,
                })
                .collect(),
            finish_reason: finish.map(str::to_owned),
            usage: None,
        }
    }

    #[test]
    fn the_default_is_to_stop_when_there_are_no_tool_calls() {
        let stop_when = StopWhen::default();

        assert!(matches!(
            should_stop(&stop_when, &completion(None, &[])),
            Some(StopReason::NoToolCalls)
        ));
        assert!(should_stop(&stop_when, &completion(None, &["get_weather"])).is_none());
        // Repeat watching is opt-in: a model re-reading a tool is often working.
        assert!(!stop_when.repeated_call);
    }

    #[test]
    fn a_terminal_finish_reason_stops_the_loop_even_with_tool_calls_pending() {
        let stop_when = StopWhen {
            no_tool_calls: false,
            finish_reason_in: vec!["stop".to_owned(), "end_turn".to_owned()],
            ..StopWhen::default()
        };

        let stopped = should_stop(&stop_when, &completion(Some("end_turn"), &["get_weather"]));
        assert!(matches!(
            stopped,
            Some(StopReason::FinishReason { value }) if value == "end_turn"
        ));
        // A reason outside the list is not terminal.
        assert!(should_stop(&stop_when, &completion(Some("length"), &[])).is_none());
    }

    #[test]
    fn the_predicate_name_is_reported_when_it_never_had_anything_to_evaluate() {
        let only_finish_reason = StopWhen {
            no_tool_calls: false,
            finish_reason_in: vec!["stop".to_owned()],
            ..StopWhen::default()
        };
        assert_eq!(
            describe_predicate(&only_finish_reason),
            "stop_when.finish_reason_in"
        );
    }

    #[test]
    fn identical_calls_share_a_fingerprint_and_different_arguments_do_not() {
        let paris = ToolCall {
            id: Some("a".to_owned()),
            name: "get_weather".to_owned(),
            arguments: serde_json::json!({"city": "Paris"}),
            arguments_as_text: false,
        };
        let paris_again = ToolCall {
            // A different id is still the same request.
            id: Some("b".to_owned()),
            ..paris.clone()
        };
        let lyon = ToolCall {
            arguments: serde_json::json!({"city": "Lyon"}),
            ..paris.clone()
        };

        assert_eq!(fingerprint(&paris), fingerprint(&paris_again));
        assert_ne!(fingerprint(&paris), fingerprint(&lyon));
    }

    fn weather_profile(response: &str) -> Profile {
        let yaml = format!(
            r"
name: agent
kind: chat
url: https://models.internal/v1
request:
  template: '{{}}'
tools:
  - name: get_weather
    schema:
      type: object
      properties:
        city:
          type: string
      required:
        - city
    {response}
"
        );
        serde_yaml_ng::from_str(&yaml).unwrap()
    }

    /// The dispatch context for a profile with no MCP servers.
    fn simulated_only(profile: Profile) -> Tools {
        capturing(profile, Vars::none())
    }

    /// The same, with a bag the profile's capture rules fill.
    fn capturing(profile: Profile, vars: std::sync::Arc<Vars>) -> Tools {
        let profile = std::sync::Arc::new(profile);
        Tools {
            validators: compile_validators(&profile.tools, &[]),
            live: Vec::new(),
            config: std::sync::Arc::new(Config::default()),
            clients: BTreeMap::new(),
            vars,
            journal: McpJournal::default(),
            hooks: HookJournal::default(),
            profile,
        }
    }

    #[test]
    fn a_declared_tool_carries_no_transport_keywords() {
        let tool = McpTool {
            name: "execute_sql".to_owned(),
            title: None,
            description: Some("Runs SQL".to_owned()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "region": {"type": "string", "x-mcp-header": "Region"},
                    "query": {"type": "string"},
                },
            }),
            annotations: None,
            server: "db".to_owned(),
        };

        let declared = declare(&tool);
        let rendered = declared.to_string();
        // The mirroring is still honoured on the wire — it just does not belong
        // in a prompt.
        assert!(!rendered.contains("x-mcp-header"), "{rendered}");
        assert_eq!(declared["function"]["name"], "execute_sql");
        assert_eq!(
            declared["function"]["parameters"]["properties"]["region"]["type"],
            "string"
        );
    }

    #[tokio::test]
    async fn arguments_are_checked_against_the_declared_schema() {
        let tools = simulated_only(weather_profile(r#"response: '{"temp": 21}'"#));

        let good = ToolCall {
            id: None,
            name: "get_weather".to_owned(),
            arguments: serde_json::json!({"city": "Paris"}),
            arguments_as_text: false,
        };
        let bad = ToolCall {
            arguments: serde_json::json!({"town": 12}),
            ..good.clone()
        };

        let invocations = invoke_tools(&tools, &[good, bad], 1).await;
        assert_eq!(invocations[0].source, ToolSource::Simulated);
        assert!(invocations[0].schema_errors.is_empty());
        assert_eq!(invocations[0].result, r#"{"temp": 21}"#);

        assert!(!invocations[1].schema_errors.is_empty());
        // The model still gets an answer, so it has a chance to correct itself.
        assert!(invocations[1].error.is_none());
    }

    #[tokio::test]
    async fn a_tool_the_profile_never_declared_is_reported_and_answered() {
        let tools = simulated_only(weather_profile(r#"response: '{"temp": 21}'"#));

        let invocations = invoke_tools(
            &tools,
            &[ToolCall {
                id: None,
                name: "launch_missiles".to_owned(),
                arguments: serde_json::json!({}),
                arguments_as_text: false,
            }],
            1,
        )
        .await;

        assert!(
            invocations[0]
                .error
                .as_ref()
                .unwrap()
                .contains("launch_missiles")
        );
        assert!(invocations[0].result.contains("error"));
    }

    #[tokio::test]
    async fn a_tool_can_answer_from_a_script() {
        let tools = simulated_only(weather_profile(
            "script: '`{\"city\": \"${arguments.city}\", \"turn\": ${turn}}`'",
        ));

        let invocations = invoke_tools(
            &tools,
            &[ToolCall {
                id: None,
                name: "get_weather".to_owned(),
                arguments: serde_json::json!({"city": "Lyon"}),
                arguments_as_text: false,
            }],
            3,
        )
        .await;

        assert_eq!(invocations[0].result, r#"{"city": "Lyon", "turn": 3}"#);
        assert!(invocations[0].error.is_none());
    }

    /// A bag filled by one rule on `get_weather`.
    fn weather_vars(name: &str, paths: &[&str]) -> std::sync::Arc<Vars> {
        Vars::new(vec![crate::capture::CaptureRule {
            tools: vec![crate::pattern::NamePattern::compile("get_weather").expect("pattern")],
            vars: BTreeMap::from([(
                name.to_owned(),
                paths
                    .iter()
                    .map(|path| path.parse().expect("path"))
                    .collect(),
            )]),
        }])
    }

    fn weather_call() -> ToolCall {
        ToolCall {
            id: None,
            name: "get_weather".to_owned(),
            arguments: serde_json::json!({"city": "Lyon"}),
            arguments_as_text: false,
        }
    }

    #[tokio::test]
    async fn a_simulated_tool_fills_the_run_variables_it_was_asked_for() {
        let vars = weather_vars("temp", &["$.temp"]);
        let tools = capturing(
            weather_profile(r#"response: '{"temp": 21}'"#),
            std::sync::Arc::clone(&vars),
        );

        let invocations = invoke_tools(&tools, &[weather_call()], 1).await;

        // Reported on the call that set it, and readable from the run's bag.
        assert_eq!(
            invocations[0].captured.get("temp"),
            Some(&serde_json::json!(21))
        );
        assert_eq!(vars.snapshot().get("temp"), Some(&serde_json::json!(21)));
    }

    #[tokio::test]
    async fn a_capture_that_matches_nothing_leaves_the_call_saying_so() {
        let vars = weather_vars("temp", &["$.temperature"]);
        let tools = capturing(
            weather_profile(r#"response: '{"temp": 21}'"#),
            std::sync::Arc::clone(&vars),
        );

        let invocations = invoke_tools(&tools, &[weather_call()], 1).await;

        // Empty rather than absent-and-unexplained: a `vars` nobody filled is a
        // fact to read in the trace, not a mystery in a rendered URL.
        assert!(invocations[0].captured.is_empty());
        assert!(vars.snapshot().is_empty());
    }

    #[tokio::test]
    async fn a_tool_that_could_not_answer_captures_nothing() {
        let vars = weather_vars("error", &["$.error"]);
        let tools = capturing(
            weather_profile("script: 'arguments.no_such_method()'"),
            std::sync::Arc::clone(&vars),
        );

        let invocations = invoke_tools(&tools, &[weather_call()], 1).await;

        // The result is our error message, not the tool's answer, and capturing
        // from it would put `mire`'s own words in a variable.
        assert!(invocations[0].error.is_some());
        assert!(invocations[0].captured.is_empty());
        assert!(vars.snapshot().is_empty());
    }

    #[tokio::test]
    async fn a_tool_answering_something_that_is_not_json_captures_nothing() {
        let vars = weather_vars("temp", &["$.temp"]);
        let tools = capturing(
            weather_profile("response: 'it is sunny in Lyon'"),
            std::sync::Arc::clone(&vars),
        );

        let invocations = invoke_tools(&tools, &[weather_call()], 1).await;

        assert!(invocations[0].error.is_none());
        assert!(invocations[0].captured.is_empty());
    }

    #[tokio::test]
    async fn a_profile_with_no_capture_rules_reports_nothing_extra() {
        let tools = simulated_only(weather_profile(r#"response: '{"temp": 21}'"#));

        let invocations = invoke_tools(&tools, &[weather_call()], 1).await;

        assert!(invocations[0].captured.is_empty());
    }

    #[tokio::test]
    async fn a_failing_tool_script_answers_with_the_error_rather_than_nothing() {
        let tools = simulated_only(weather_profile("script: 'arguments.no_such_method()'"));

        let invocations = invoke_tools(
            &tools,
            &[ToolCall {
                id: None,
                name: "get_weather".to_owned(),
                arguments: serde_json::json!({"city": "Lyon"}),
                arguments_as_text: false,
            }],
            1,
        )
        .await;

        assert!(invocations[0].error.is_some());
        assert!(invocations[0].result.contains("error"));
    }
}
