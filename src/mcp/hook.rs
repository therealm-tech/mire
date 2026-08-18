//! Something that happens around a `tools/call`.
//!
//! A live MCP tool is the one thing `mire` does that has effects outside this
//! process, which makes it the one thing somebody else usually wants to know
//! about: an audit trail, a policy gate, a webhook that pages whoever owns the
//! server being poked. A hook is that — declared on the server in `mcp.yaml`,
//! fired [`Before`](HookPhase::Before) the call goes out, [`After`](HookPhase::After)
//! it comes back, or both.
//!
//! ```yaml
//! hooks:
//!   - name: audit
//!     on:
//!       - before
//!       - after
//!     action:
//!       kind: http
//!       url: https://audit.internal/tool-calls
//!       auth: keycloak-workload
//! ```
//!
//! # Why the action is tagged
//!
//! `kind: http` is the only one today and the only one worth having first: an
//! HTTP call is what every audit sink, policy service and chat webhook already
//! speaks. It is tagged anyway, because the alternative is a second shape bolted
//! onto a flat struct later, and a `kind:` added after the fact is a breaking
//! change to every file that already exists.
//!
//! # A hook can stop a call
//!
//! `on_error: fail` — the default — means a hook that could not be run, or whose
//! endpoint answered outside `2xx`, is a failure of the tool call it belongs to.
//! For a [`Before`](HookPhase::Before) hook that is a policy gate for free: the
//! `tools/call` never goes out. For an [`After`](HookPhase::After) hook it is a
//! report rather than an undo — the tool already ran, and nothing here can take
//! that back. `on_error: continue` records the failure and gets out of the way.
//!
//! The default is the loud one on purpose. A hook is something you asked for; a
//! harness that quietly skipped it would be answering a question you did not ask.
//!
//! # Which tools it covers
//!
//! `tools:` is a list of regexes, each matched against the whole name — see
//! [`NamePattern`] for why anchored. Empty is every tool. Compiled when
//! `mcp.yaml` loads, like everything else here.
//!
//! ```yaml
//!     tools:
//!       - write_.*
//!       - delete_file
//! ```
//!
//! # Which calls it has enough to fire on
//!
//! `when_defined:` names variables that must have been captured for the hook to
//! fire at all — plain names, not templates, because this asks whether a value
//! exists and the place to *use* it is `url:`, `body:` or `headers:`.
//!
//! ```yaml
//!     when_defined:
//!       - session
//!     action:
//!       kind: http
//!       url: https://audit.internal/sessions/{{ vars.session }}/tool-calls
//! ```
//!
//! It is the graceful counterpart to the loud undefined above, and the pairing
//! is the point: without it, that URL fails every call made before a session
//! exists; with it, the hook simply waits and starts firing once one does.
//!
//! **Not firing is not failing.** `on_error` does not apply, nothing is sent, no
//! credential is resolved, and the tool call proceeds untouched. The skip is
//! recorded all the same, naming the variable it waited for — see
//! [`HookRecord::skipped`]. A hook that quietly never ran and a hook that was
//! never declared must not look the same in a trace.
//!
//! Names are matched against what the run has captured and nothing else. There
//! is deliberately no check at load that some profile captures them: `mcp.yaml`
//! does not know which profiles will use the server, and a name nothing ever
//! fills shows up as a skip naming it rather than as a startup error about a
//! file that may be perfectly correct.
//!
//! # What it sends
//!
//! With no `body:`, the payload is the call itself — phase, server, tool,
//! arguments, and on the way back the result — as JSON. A `body:` template
//! replaces it and sees the same fields by name, plus `env` and `vars`:
//!
//! ```yaml
//!     body: '{"who": "{{ env.USER }}", "ran": "{{ tool }}"}'
//! ```
//!
//! `auth` is deliberately **not** in scope there. A credential belongs in a
//! header, where the redactor is; a body template that can reach the auth
//! registry is a credential one typo away from a webhook's access log.
//!
//! A hook's own `headers:` see `vars` too, beside the `env` and `auth` they
//! already had — see [`super::headers`]. Unlike a server's, they need no
//! `| default(...)` to be safe: a hook only renders around a call, so anything
//! captured before that call is there.
//!
//! # Where it sends it
//!
//! `url:` is ordinarily just a URL, parsed and checked when `mcp.yaml` loads. It
//! may also be a template, seeing exactly what `body:` sees — one context, so
//! there is no second table to remember:
//!
//! ```yaml
//!     url: https://audit.internal/sessions/{{ vars.session }}/tool-calls
//! ```
//!
//! `vars` is what earlier tool calls captured; see [`crate::vars`] for how a
//! value gets in there. A template that reads something undefined fails the hook
//! rather than sending the request somewhere with a hole in it, which is the
//! same rule the headers follow and for the same reason.
//!
//! **A templated URL is resolved before the credential is.** `allowed_hosts` is a
//! statement about where a credential may go, so it has to be checked against
//! the address the request will actually use, not against the one the file
//! happened to be written with. A template that renders to a host the provider
//! does not allow gets the refusal.
//!
//! # Attaching the run's files
//!
//! `files:` names uploads the same way `tools:` names tools, and what it names
//! goes out for real: the request becomes a `multipart/form-data` with the body
//! demoted to a `payload` part and one `file` part per attachment.
//!
//! ```yaml
//!     files:
//!       - .*\.pdf
//! ```
//!
//! Empty — the default — attaches **nothing**, which is the opposite of what an
//! empty `tools:` means. The asymmetry is deliberate: covering every tool is a
//! wide hook, while shipping every file somebody attached to a third address is
//! a leak, and the second must be asked for. `.*` asks for it.
//!
//! The same files reach a `body:` template as `uploads`, whole — `base64`,
//! `dataUrl`, `text`, the entries a model template gets from the same run. The
//! default payload describes them instead, by name and size: the bytes are
//! already going out as parts, and putting them in the body too would send every
//! file twice.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use minijinja::{Environment, UndefinedBehavior};
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderValue};
use reqwest::multipart::{Form, Part};
use reqwest::{Client, Method};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, warn};
use url::Url;

use super::auth::HookCredentials;
use super::headers::HeaderTemplates;
use super::{McpCredentials, McpError};
use crate::auth::AuthProvider;
use crate::redact::Redactor;
use crate::uploads::UploadRef;
use crate::vars::Captured;

/// Body templates render strictly, for the same reason header templates do: a
/// payload silently missing the field it was supposed to carry is a webhook that
/// looks like it works.
static ENVIRONMENT: std::sync::LazyLock<Environment<'static>> = std::sync::LazyLock::new(|| {
    let mut environment = Environment::new();
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    environment
});

/// When a hook fires, relative to the `tools/call` it is attached to.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum HookPhase {
    /// Before the request goes out. A failure here stops the call.
    Before,
    /// After the answer came back — including when it came back as a failure,
    /// because an audit trail that only records the calls that worked is not one.
    After,
}

impl HookPhase {
    /// The wire spelling, which is also what `mcp.yaml` writes.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Before => "before",
            Self::After => "after",
        }
    }
}

impl std::fmt::Display for HookPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What a hook failing means for the call it is attached to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OnError {
    /// The tool call fails too. The default: a hook is something you asked for.
    #[default]
    Fail,
    /// Recorded and stepped over.
    Continue,
}

/// A name-matching pattern, shared with the profile's own `tools:` lists.
///
/// Used here by `tools:` for tool names and by `files:` for upload names. One
/// type because it is one rule — the pattern has to match the whole name. See
/// [`crate::pattern`] for why that is not negotiable.
pub use crate::pattern::NamePattern;

/// One hook, compiled and ready to fire.
#[derive(Debug, Clone)]
pub struct Hook {
    /// Name, unique within its server. What the trace calls it.
    pub name: String,
    /// Phases it fires on. A set, so declaring one twice is not two calls.
    pub phases: BTreeSet<HookPhase>,
    /// Tools it applies to, as patterns. Empty — the default — is every tool the
    /// server offers.
    pub tools: Vec<NamePattern>,
    /// Variables that must have been captured for it to fire at all. Empty —
    /// the default — is no condition.
    ///
    /// Names, not templates: this asks whether a value exists, and the place to
    /// use it is `url:`, `body:` or `headers:`.
    pub when_defined: Vec<String>,
    /// What its failure does to the call.
    pub on_error: OnError,
    /// What it actually does.
    pub action: HookAction,
}

impl Hook {
    /// Whether this hook has anything to say about `tool` at `phase`.
    ///
    /// Only what the file says — [`waiting_for`](Self::waiting_for) is the half
    /// that depends on what the run has done so far.
    #[must_use]
    pub fn fires(&self, phase: HookPhase, tool: &str) -> bool {
        self.phases.contains(&phase) && NamePattern::any_matches(&self.tools, tool)
    }

    /// The first `when_defined:` variable this run has not captured yet.
    ///
    /// `Some` means the hook does not fire *this time*, which is a different
    /// thing from failing: a hook waiting for a session to be opened is a hook
    /// working as declared, and a run where the session never opens is a run
    /// where it correctly never fired.
    ///
    /// The first rather than all of them, because the answer to "why did my
    /// hook not fire" is one name, and a list would only be longer.
    #[must_use]
    pub fn waiting_for(&self, vars: &Captured) -> Option<&str> {
        self.when_defined
            .iter()
            .find(|name| !vars.contains_key(*name))
            .map(String::as_str)
    }

    /// Where it sends what it sends, as `mcp.yaml` wrote it.
    ///
    /// Its own URL, not the server's — which is the whole reason its credentials
    /// are resolved separately: an auth provider's `allowed_hosts` is a statement
    /// about where its credential may go, and a hook goes somewhere else.
    ///
    /// A [template](HookUrl::Template) is only an address once a call has been
    /// made; [`HookUrl::resolve`] is where it becomes one.
    #[must_use]
    pub const fn url(&self) -> &HookUrl {
        match &self.action {
            HookAction::Http(http) => &http.url,
        }
    }

    /// The provider it authenticates with, if it names one.
    #[must_use]
    pub fn auth(&self) -> Option<&str> {
        match &self.action {
            HookAction::Http(http) => http.auth.as_deref(),
        }
    }

    /// Providers its header templates read, beyond [`Self::auth`].
    pub fn header_providers(&self) -> impl Iterator<Item = &str> {
        match &self.action {
            HookAction::Http(http) => http.headers.providers(),
        }
    }

    /// The action's kind, as `mcp.yaml` spells it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match &self.action {
            HookAction::Http(_) => "http",
        }
    }
}

/// What a hook does when it fires.
///
/// One variant today. See the module docs for why it is an enum anyway.
#[derive(Debug, Clone)]
pub enum HookAction {
    /// Calls an HTTP endpoint.
    Http(HttpAction),
}

/// Where a hook sends what it sends.
///
/// Two variants rather than "a template that usually has no placeholders in it",
/// because the overwhelmingly common case deserves its load-time check: a URL
/// with a typo in its scheme should be a startup issue naming the hook, not a
/// string that renders beautifully and fails to parse on the first tool call.
#[derive(Debug, Clone)]
pub enum HookUrl {
    /// A URL, parsed when `mcp.yaml` loaded.
    Fixed(Url),
    /// A `MiniJinja` template, compiled when `mcp.yaml` loaded and rendered per
    /// firing. Only a template when it actually contains one.
    Template(String),
}

impl HookUrl {
    /// The address this hook will use for the call being rendered.
    ///
    /// # Errors
    ///
    /// The template's own message, or the parse failure of what it produced —
    /// with the rendered text quoted, since that is the part nobody can see by
    /// reading the file.
    fn resolve(&self, rendering: &Rendering<'_>) -> Result<Url, String> {
        match self {
            Self::Fixed(url) => Ok(url.clone()),
            Self::Template(template) => {
                let rendered = ENVIRONMENT
                    .render_str(template, rendering)
                    .map_err(|error| {
                        format!(
                            "the url template could not be rendered: {}",
                            explain(&error, template, rendering)
                        )
                    })?;
                Url::parse(&rendered).map_err(|error| {
                    format!("the url template produced `{rendered}`, which is not a URL: {error}")
                })
            }
        }
    }

    /// What `mcp.yaml` wrote, for the trace and the descriptor.
    #[must_use]
    pub fn source(&self) -> &str {
        match self {
            Self::Fixed(url) => url.as_str(),
            Self::Template(template) => template,
        }
    }

    /// Whether `source` needs rendering at all.
    ///
    /// The two delimiters `MiniJinja` acts on. A URL containing neither is a
    /// URL, and gets parsed as one.
    #[must_use]
    pub fn is_template(source: &str) -> bool {
        source.contains("{{") || source.contains("{%")
    }
}

/// An HTTP call, with the same credential machinery an MCP server gets.
#[derive(Debug, Clone)]
pub struct HttpAction {
    /// Where to call. A URL, or a template producing one per firing.
    pub url: HookUrl,
    /// How. `POST` unless told otherwise, because a hook carries a payload.
    pub method: Method,
    /// Auth provider, by registry name. Puts the credential where the provider
    /// says it goes.
    pub auth: Option<String>,
    /// Extra headers, rendered per request. `env` and `auth` are in scope,
    /// exactly as they are for a server's own headers.
    pub headers: HeaderTemplates,
    /// Body template. `None` sends the call as JSON — see [`Payload`].
    pub body: Option<String>,
    /// Which of the run's uploads to attach, by name.
    ///
    /// Empty — the default — is **none**, which is the opposite of what `tools:`
    /// empty means and deliberately so: a hook that shipped somebody's attached
    /// files to a third party unless told otherwise is a leak, not a default.
    /// `.*` asks for all of them.
    pub files: Vec<NamePattern>,
    /// Per-request timeout.
    pub timeout: Duration,
}

/// One hook firing, as it happened.
///
/// Filed whatever the outcome, including a request that never left: a hook that
/// could not render its body is exactly the hook somebody needs to read about.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookRecord {
    /// Registry name of the server the hook is declared on.
    pub server: String,
    /// The hook's own name.
    pub hook: String,
    /// Which side of the call it fired on.
    pub phase: HookPhase,
    /// The tool whose call it fired around.
    pub tool: String,
    /// The action's kind: `http`.
    pub action: String,
    /// Where it went.
    pub url: String,
    /// The HTTP method it went out as.
    pub method: String,
    /// Request headers, masked.
    pub headers: BTreeMap<String, String>,
    /// The body it sent, masked. Empty when it never got that far.
    ///
    /// With files attached this is the `payload` part alone. The parts carrying
    /// the files are in [`files`](Self::files), by name and size: a trace is
    /// something a person reads, and 25 MB of base64 in it would cost the panel
    /// everything and tell nobody anything.
    pub request: String,
    /// The files it attached, described rather than repeated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<Attachment>,
    /// HTTP status, or `0` when the request never reached anybody.
    pub status: u16,
    /// The response body, masked.
    pub response: String,
    /// Round trip, in milliseconds.
    pub latency_ms: u64,
    /// Why it failed, when it did. A non-`2xx` answer counts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether this failure is what stopped the tool call.
    ///
    /// Distinguishes `on_error: fail` from `on_error: continue` after the fact,
    /// which is the difference between "the tool never ran" and "the tool ran and
    /// nobody was told".
    pub stopped_the_call: bool,
    /// The `when_defined:` variable that was not captured, when that is why
    /// nothing was sent.
    ///
    /// Filed rather than passed over in silence. A hook that did not fire is a
    /// question somebody will ask sooner or later, and "it was waiting for
    /// `session`" is the whole answer; a trace that simply omitted it would make
    /// a declared hook and a hook that never ran look identical. Not an error —
    /// `error` stays empty and `on_error` does not apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// One attached file, as the trace and the default payload describe it.
///
/// Everything except the bytes. The endpoint got those as a multipart part; a
/// reader of the trace wants to know which file went, not to scroll past it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// The upload's handle, as `POST /api/uploads` answered it.
    pub id: String,
    /// File name, sanitised. Also the part's `filename`.
    pub name: String,
    /// Size in bytes.
    pub size: u64,
    /// Media type the part went out as.
    pub content_type: String,
}

/// Where one run collects the hooks it fired.
///
/// A plain [`Mutex`] rather than tokio's, for the same reason [`super::McpJournal`]
/// is one: every critical section is a `push` with no `await` in it.
pub type HookJournal = Arc<Mutex<Vec<HookRecord>>>;

/// Validates a template without rendering it.
///
/// Called when `mcp.yaml` loads, so a syntax error names the hook and the field
/// at startup rather than on the first tool call twenty minutes into a run.
///
/// # Errors
///
/// The template's own message, for the load issue.
pub fn check_template(field: &str, template: &str) -> Result<(), String> {
    ENVIRONMENT
        .template_from_str(template)
        .map(|_| ())
        .map_err(|error| format!("`{field}`: {error}"))
}

/// Takes everything recorded so far, leaving the journal empty.
#[must_use]
pub fn drain(journal: &HookJournal) -> Vec<HookRecord> {
    journal
        .lock()
        .map(|mut entries| std::mem::take(&mut *entries))
        .unwrap_or_default()
}

/// What a hook is told about the call it fired around.
///
/// This is the default body verbatim, and the named variables a `body:` template
/// sees. `env` is added for the template and left out of the payload — a hook
/// that ships the whole process environment to a webhook by default is a leak
/// nobody asked for.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload<'a> {
    /// `before` or `after`.
    pub phase: HookPhase,
    /// Registry name of the MCP server.
    pub server: &'a str,
    /// The tool being called.
    pub tool: &'a str,
    /// The arguments the model produced, as they will be sent.
    pub arguments: &'a Value,
    /// What the call produced. `after` only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<ToolOutcome>,
}

/// The payload as one hook sends it: the call, plus what that hook attached.
///
/// Separate from [`Payload`] because the call is one thing and `files:` is per
/// hook — two hooks on the same `tools/call` can attach different files, and the
/// payload they share must not have to pick one of them.
#[derive(Debug, Serialize)]
struct Envelope<'a> {
    #[serde(flatten)]
    payload: &'a Payload<'a>,
    /// The files travelling alongside, described.
    ///
    /// Described and not embedded: the bytes are already going out as multipart
    /// parts, and a default payload that also carried them base64'd would send
    /// every file twice. A `body:` template that wants the bytes anyway has
    /// `uploads` for exactly that.
    #[serde(skip_serializing_if = "<[Attachment]>::is_empty")]
    files: &'a [Attachment],
}

/// The outcome of a `tools/call`, as an `after` hook sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolOutcome {
    /// The content blocks, flattened. Empty when the call failed outright.
    pub text: String,
    /// `structuredContent`, when the server sent it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured: Option<Value>,
    /// The server's own `isError`.
    pub is_error: bool,
    /// Round trip, in milliseconds.
    pub latency_ms: u64,
    /// Set when the call could not produce a result at all, with the reason.
    ///
    /// Different from `isError`: that is a tool reporting a problem, this is no
    /// tool having run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The payload plus what only a template gets to see.
///
/// Shared by `url:` and `body:` so the two cannot drift into two vocabularies
/// for the same call.
#[derive(Debug, Serialize)]
struct Rendering<'a> {
    #[serde(flatten)]
    envelope: &'a Envelope<'a>,
    /// The process environment, read fresh on every render.
    env: BTreeMap<String, String>,
    /// What the run's tool calls have captured so far.
    ///
    /// Here and not in the payload, like `env`: a hook that shipped a run's
    /// whole variable bag to a webhook by default is a decision nobody made.
    vars: &'a Captured,
    /// The attached files whole — `base64`, `dataUrl`, `text` and the rest, the
    /// same entries a model template gets from the same run.
    ///
    /// Only here, never in the payload: the bytes go out as parts, and a
    /// template asking for them in the body is asking on purpose.
    uploads: &'a [&'a UploadRef],
}

/// Fires every hook that applies, in declaration order.
///
/// # Errors
///
/// The first failure of a hook whose `on_error` is `fail`. Later hooks are not
/// fired: a gate that said no has said no, and running the rest would be a
/// notification about a call that is not happening.
pub(super) async fn fire(
    http: &Client,
    hooks: &[Hook],
    payload: &Payload<'_>,
    uploads: &[UploadRef],
    vars: &Captured,
    credentials: &McpCredentials<'_>,
    file: impl Fn(HookRecord),
) -> Result<(), McpError> {
    for hook in hooks {
        if !hook.fires(payload.phase, payload.tool) {
            continue;
        }

        // Asked for by `when_defined:`, and answered before anything is built:
        // a hook waiting on a variable pays for no credential, renders no
        // template, and sends nothing. It is filed all the same — see
        // [`HookRecord::skipped`] for why a non-event still belongs in a trace.
        if let Some(missing) = hook.waiting_for(vars) {
            debug!(
                server = %payload.server,
                hook = %hook.name,
                phase = %payload.phase,
                tool = %payload.tool,
                %missing,
                "hook skipped: the variable it waits for is not captured yet"
            );
            file(skipped(hook, payload, missing));
            continue;
        }

        let (mut record, outcome) = run(http, hook, payload, uploads, vars, credentials).await;

        let Err(message) = outcome else {
            debug!(
                server = %payload.server,
                hook = %hook.name,
                phase = %payload.phase,
                tool = %payload.tool,
                "hook fired"
            );
            file(record);
            continue;
        };

        record.stopped_the_call = hook.on_error == OnError::Fail;
        record.error = Some(message.clone());
        warn!(
            server = %payload.server,
            hook = %hook.name,
            phase = %payload.phase,
            tool = %payload.tool,
            stopped_the_call = record.stopped_the_call,
            %message,
            "hook failed"
        );
        let stopped = record.stopped_the_call;
        file(record);

        if stopped {
            return Err(McpError::Hook {
                server: payload.server.to_owned(),
                hook: hook.name.clone(),
                phase: payload.phase,
                message,
            });
        }
    }

    Ok(())
}

/// The record for a hook that `when_defined:` held back.
///
/// Everything a firing would have said about *which* hook this was, and nothing
/// about a request: there was none. `url` is what the file says rather than a
/// rendered address, because rendering it is exactly what did not happen — and
/// on the hook this is written for, could not have.
fn skipped(hook: &Hook, payload: &Payload<'_>, missing: &str) -> HookRecord {
    let HookAction::Http(action) = &hook.action;
    HookRecord {
        server: payload.server.to_owned(),
        hook: hook.name.clone(),
        phase: payload.phase,
        tool: payload.tool.to_owned(),
        action: hook.kind().to_owned(),
        url: action.url.source().to_owned(),
        method: action.method.to_string(),
        headers: BTreeMap::new(),
        request: String::new(),
        files: Vec::new(),
        status: 0,
        response: String::new(),
        latency_ms: 0,
        error: None,
        stopped_the_call: false,
        skipped: Some(missing.to_owned()),
    }
}

/// One hook, run to completion. Always produces a record.
async fn run(
    http: &Client,
    hook: &Hook,
    payload: &Payload<'_>,
    uploads: &[UploadRef],
    vars: &Captured,
    credentials: &McpCredentials<'_>,
) -> (HookRecord, Result<(), String>) {
    let HookAction::Http(action) = &hook.action;

    let mut record = HookRecord {
        server: payload.server.to_owned(),
        hook: hook.name.clone(),
        phase: payload.phase,
        tool: payload.tool.to_owned(),
        action: hook.kind().to_owned(),
        // Replaced below by the address actually used. Until then it is what the
        // file says, so a hook that dies rendering its URL still says which one.
        url: action.url.source().to_owned(),
        method: action.method.to_string(),
        headers: BTreeMap::new(),
        request: String::new(),
        files: Vec::new(),
        status: 0,
        response: String::new(),
        latency_ms: 0,
        error: None,
        stopped_the_call: false,
        skipped: None,
    };

    let attached: Vec<&UploadRef> = select(&action.files, uploads);
    let described: Vec<Attachment> = attached.iter().map(|file| describe(file)).collect();
    let envelope = Envelope {
        payload,
        files: &described,
    };
    let rendering = Rendering {
        envelope: &envelope,
        env: std::env::vars().collect(),
        vars,
        uploads: &attached,
    };

    // Before the credential, and this order is not an accident: the provider
    // decides what to hand over by looking at where it is going, so the address
    // has to exist first. A template that renders to somewhere `allowed_hosts`
    // refuses is refused, which is the check doing its job.
    let url = match action.url.resolve(&rendering) {
        Ok(url) => url,
        Err(message) => return (record, Err(message)),
    };
    record.url = url.to_string();

    // Resolved here rather than with the server's, and only now: a credential
    // costs an exchange and can fail, and neither belongs to a hook that did not
    // fire. Against the *hook's* URL, so `allowed_hosts` means what it says.
    let resolved = match credentials.for_hook(hook, &url).await {
        Ok(resolved) => resolved,
        Err(error) => return (record, Err(error.to_string())),
    };

    let body = match body(action, &rendering) {
        Ok(body) => body,
        Err(message) => return (record, Err(message)),
    };

    // Files turn the whole thing into a `multipart/form-data`, with the body
    // demoted to a `payload` part beside them.
    let attachments = if attached.is_empty() {
        None
    } else {
        match form(body.clone(), &attached) {
            Ok(form) => Some(form),
            Err(message) => return (record, Err(message)),
        }
    };

    let (mut headers, scrub) = match authenticate(
        action,
        &url,
        &resolved,
        payload.server,
        vars,
        attachments.is_some(),
    )
    .await
    {
        Ok(pair) => pair,
        Err(message) => return (record, Err(message)),
    };

    let shown = settle_content_type(&mut headers, attachments.as_ref());

    // Filled in only now: the auth provider has just added its secret to the
    // redactor, and a record taken a line earlier would carry it in the clear.
    record.headers = scrub.headers(&shown);
    record.request = scrub.text(&body);
    record.files = described;

    let request = http
        .request(action.method.clone(), url)
        .headers(headers)
        .timeout(action.timeout);
    let request = match attachments {
        None => request.body(body),
        Some(form) => request.multipart(form),
    };

    let started = Instant::now();
    let sent = request.send().await;
    record.latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let response = match sent {
        Ok(response) => response,
        // The endpoint's own words, not `reqwest`'s: a hook that failed at the
        // address printed right above it has to say what it ran into there.
        Err(error) => return (record, Err(scrub.text(&crate::transport::explain(&error)))),
    };

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    record.status = status.as_u16();
    record.response = scrub.text(&text);

    if status.is_success() {
        (record, Ok(()))
    } else {
        let message = refused(status, &record.response);
        (record, Err(message))
    }
}

/// What a hook's endpoint saying no amounts to, in one line.
///
/// The body goes in the message, already scrubbed: a policy gate that refuses a
/// call usually says why, and the reason is the only half worth reading. When it
/// says nothing at all, say *that* — a message trailing off after a colon reads
/// like something was lost on the way here.
fn refused(status: reqwest::StatusCode, body: &str) -> String {
    let detail = snippet(body);
    if detail.is_empty() {
        format!("answered {status} with an empty body")
    } else {
        format!("answered {status}: {detail}")
    }
}

/// The run's uploads this hook asked for, in the order the run attached them.
///
/// No patterns is none of them. See [`HttpAction::files`] for why that is the
/// opposite of what `tools:` empty means.
fn select<'a>(patterns: &[NamePattern], uploads: &'a [UploadRef]) -> Vec<&'a UploadRef> {
    if patterns.is_empty() {
        return Vec::new();
    }
    uploads
        .iter()
        .filter(|upload| patterns.iter().any(|pattern| pattern.matches(&upload.name)))
        .collect()
}

/// One upload, as the trace and the payload describe it. Everything but bytes.
fn describe(upload: &UploadRef) -> Attachment {
    Attachment {
        id: upload.id.clone(),
        name: upload.name.clone(),
        size: upload.size,
        content_type: mime_of(upload).to_owned(),
    }
}

/// The media type a file's part goes out as.
///
/// The extension's guess, made by the upload store. `application/octet-stream`
/// when the extension gave nothing away, which is what a part with no better
/// idea is supposed to say.
fn mime_of(upload: &UploadRef) -> &str {
    upload
        .content_type
        .as_deref()
        .unwrap_or("application/octet-stream")
}

/// The multipart form: the `payload` part, then one `file` part per attachment.
///
/// Every file uses the same part name. A form with one field repeated is what
/// every server-side upload handler already reads, and naming each part after
/// its file would make the endpoint guess field names it cannot know in advance.
fn form(body: String, attached: &[&UploadRef]) -> Result<Form, String> {
    let payload = Part::text(body)
        .mime_str("application/json")
        .map_err(|error| format!("the payload part could not be built: {error}"))?;
    let mut form = Form::new().part("payload", payload);

    for upload in attached {
        // Decoded rather than re-read off the disk: the run already holds the
        // file, and going back to the filesystem would be a second answer to a
        // question already answered — one that can differ if the file moved.
        let bytes = BASE64
            .decode(&upload.base64)
            .map_err(|error| format!("`{}` could not be decoded: {error}", upload.name))?;
        let part = Part::bytes(bytes)
            .file_name(upload.name.clone())
            .mime_str(mime_of(upload))
            .map_err(|error| format!("`{}` could not be attached: {error}", upload.name))?;
        form = form.part("file", part);
    }

    Ok(form)
}

/// The body to send: the template's, or the call itself as JSON.
fn body(action: &HttpAction, rendering: &Rendering<'_>) -> Result<String, String> {
    let Some(template) = &action.body else {
        return serde_json::to_string(rendering.envelope)
            .map_err(|error| format!("the payload could not be serialised: {error}"));
    };

    ENVIRONMENT
        .render_str(template, rendering)
        .map_err(|error| {
            format!(
                "the body template could not be rendered: {}",
                explain(&error, template, rendering)
            )
        })
}

/// The headers to send, and a redactor holding every secret among them.
async fn authenticate(
    action: &HttpAction,
    url: &Url,
    credentials: &HookCredentials<'_>,
    server: &str,
    vars: &Captured,
    multipart: bool,
) -> Result<(HeaderMap, Redactor), String> {
    let mut headers = HeaderMap::new();
    // The default body is JSON. A template that sends something else says so with
    // a `content-type:` header of its own, which lands after this and wins.
    //
    // Skipped entirely when files are going out: that body's type is settled by
    // the multipart encoder, boundary and all, and nothing written here could be
    // right about it.
    if !multipart {
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    }

    let mut scrub = Redactor::new();
    let rendered = action
        .headers
        .render(server, credentials.named(), vars)
        .map_err(|error| error.to_string())?;

    for (name, value) in rendered {
        let mut header = HeaderValue::from_str(value.expose())
            .map_err(|_| format!("header `{name}`: the rendered value cannot go in one"))?;
        header.set_sensitive(true);
        scrub.add(&value);
        headers.insert(name, header);
    }

    // Last, so a named provider wins over a hand-written header of the same name
    // rather than being quietly overwritten by one — the same order a server's
    // own headers go out in.
    if let Some(provider) = credentials.provider() {
        let from_auth = provider
            .apply(&mut headers, url, None)
            .await
            .map_err(|error| error.to_string())?;
        scrub.merge(&from_auth);
    }

    Ok((headers, scrub))
}

/// Leaves `headers` carrying the right `content-type`, and returns the headers
/// as the trace should show them.
///
/// Whatever `content-type` survived has to go before `.multipart()` adds the
/// real one. `reqwest` *appends* rather than replaces, so leaving one here would
/// send two — and a server reading the first would be told `json` about a body
/// that is not. Put back for the record only, boundary included, so the trace
/// says what actually went out.
fn settle_content_type(headers: &mut HeaderMap, form: Option<&Form>) -> BTreeMap<String, String> {
    let Some(form) = form else {
        return readable(headers);
    };

    headers.remove(CONTENT_TYPE);
    let mut shown = readable(headers);
    shown.insert(
        CONTENT_TYPE.as_str().to_owned(),
        format!("multipart/form-data; boundary={}", form.boundary()),
    );
    shown
}

/// Headers as text, for the record. A value that is not UTF-8 is named rather
/// than dropped: it was still sent.
fn readable(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value
                    .to_str()
                    .map_or_else(|_| "<not text>".to_owned(), str::to_owned),
            )
        })
        .collect()
}

/// The innermost message, plus the name of whatever was undefined.
///
/// `MiniJinja` says "undefined value" without saying *which*, which on a URL
/// reading three variables leaves you guessing. The template is scanned for the
/// lookups it makes — `env` and `vars`, the two roots that can be absent — and
/// the ones that resolved to nothing are named. Exactly what a header template
/// already does, and [`super::headers::lookups`] is the same scanner, so the two
/// cannot find different names for the same template.
///
/// Only **names** are ever emitted. Echoing the template back would be more
/// direct and is exactly the wrong thing: a `body:` may hold a literal
/// credential, and an error message is a place secrets go to be logged forever.
fn explain(error: &minijinja::Error, template: &str, rendering: &Rendering<'_>) -> String {
    let message = root(error);

    let mut missing: Vec<&str> = super::headers::lookups(template, "env")
        .into_iter()
        .filter(|name| !rendering.env.contains_key(*name))
        .collect();
    missing.extend(
        super::headers::lookups(template, "vars")
            .into_iter()
            .filter(|name| !rendering.vars.contains_key(*name)),
    );

    match missing.as_slice() {
        [] => message,
        [one] => format!("{message} — `{one}` is not set"),
        many => format!("{message} — none of {many:?} are set"),
    }
}

/// `MiniJinja` wraps the useful message; this is the innermost one.
fn root(error: &minijinja::Error) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        message = inner.to_string();
        source = inner.source();
    }
    message
}

/// Enough of a body to recognise it, in an error message that has to stay short.
fn snippet(text: &str) -> String {
    const LIMIT: usize = 200;
    let trimmed = text.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_owned();
    }
    let cut: String = trimmed.chars().take(LIMIT).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hook(name: &str, phases: &[HookPhase], tools: &[&str]) -> Hook {
        Hook {
            name: name.to_owned(),
            phases: phases.iter().copied().collect(),
            tools: tools
                .iter()
                .map(|tool| NamePattern::compile(tool).expect("pattern"))
                .collect(),
            when_defined: Vec::new(),
            on_error: OnError::Fail,
            action: HookAction::Http(HttpAction {
                url: HookUrl::Fixed("https://audit.internal/events".parse().expect("url")),
                method: Method::POST,
                auth: None,
                headers: HeaderTemplates::default(),
                body: None,
                files: Vec::new(),
                timeout: Duration::from_secs(5),
            }),
        }
    }

    /// A hook that waits for `waits_for` before firing.
    fn waiting(name: &str, waits_for: &[&str]) -> Hook {
        Hook {
            when_defined: waits_for.iter().map(|v| (*v).to_owned()).collect(),
            ..hook(name, &[HookPhase::After], &[])
        }
    }

    /// The body one hook would send, with nothing attached and nothing captured.
    fn rendered(action: &HttpAction, payload: &Payload<'_>) -> Result<String, String> {
        with_files(action, payload, &[])
    }

    /// The body one hook would send, with `attached` going out beside it.
    fn with_files(
        action: &HttpAction,
        payload: &Payload<'_>,
        attached: &[&UploadRef],
    ) -> Result<String, String> {
        with_vars(action, payload, attached, &Captured::new())
            .map(|(body, _)| body)
            .map_err(|(message, _)| message)
    }

    /// The body and the URL one hook would produce, given what a run captured.
    ///
    /// Both at once because they render from one context, and a test that could
    /// only see one of them could not catch the two drifting apart.
    fn with_vars(
        action: &HttpAction,
        payload: &Payload<'_>,
        attached: &[&UploadRef],
        vars: &Captured,
    ) -> Result<(String, Url), (String, Option<Url>)> {
        let described: Vec<Attachment> = attached.iter().map(|file| describe(file)).collect();
        let envelope = Envelope {
            payload,
            files: &described,
        };
        let rendering = Rendering {
            envelope: &envelope,
            env: std::env::vars().collect(),
            vars,
            uploads: attached,
        };
        let url = action
            .url
            .resolve(&rendering)
            .map_err(|message| (message, None))?;
        let body = body(action, &rendering).map_err(|message| (message, Some(url.clone())))?;
        Ok((body, url))
    }

    /// One upload, as a run would have it: bytes already read and encoded.
    fn upload(name: &str, content_type: Option<&str>, bytes: &[u8]) -> UploadRef {
        UploadRef {
            id: "aB3dE5gH7jK9".to_owned(),
            name: name.to_owned(),
            stored_as: format!("aB3dE5gH7jK9-{name}"),
            path: format!("/uploads/aB3dE5gH7jK9-{name}"),
            size: bytes.len() as u64,
            content_type: content_type.map(str::to_owned),
            base64: BASE64.encode(bytes),
            data_url: format!(
                "data:{};base64,{}",
                content_type.unwrap_or("application/octet-stream"),
                BASE64.encode(bytes)
            ),
            text: String::from_utf8(bytes.to_vec()).ok(),
        }
    }

    #[test]
    fn a_hook_with_no_tool_list_applies_to_every_tool() {
        let hook = hook("audit", &[HookPhase::Before], &[]);
        assert!(hook.fires(HookPhase::Before, "read_file"));
        assert!(hook.fires(HookPhase::Before, "rm_rf"));
        // Declaring `before` is not declaring `after`.
        assert!(!hook.fires(HookPhase::After, "read_file"));
    }

    #[test]
    fn a_tool_list_narrows_it_to_those_tools() {
        let hook = hook(
            "gate",
            &[HookPhase::Before, HookPhase::After],
            &["write_file"],
        );
        assert!(hook.fires(HookPhase::Before, "write_file"));
        assert!(hook.fires(HookPhase::After, "write_file"));
        assert!(!hook.fires(HookPhase::Before, "read_file"));
    }

    #[test]
    fn a_pattern_covers_the_tools_it_describes() {
        let hook = hook("gate", &[HookPhase::Before], &["write_.*", "delete_file"]);
        assert!(hook.fires(HookPhase::Before, "write_file"));
        assert!(hook.fires(HookPhase::Before, "write_anything_at_all"));
        assert!(hook.fires(HookPhase::Before, "delete_file"));
        assert!(!hook.fires(HookPhase::Before, "read_file"));
    }

    #[test]
    fn a_pattern_matches_the_whole_name_or_not_at_all() {
        // The point of anchoring: a gate on `write_file` is a gate on that tool,
        // not on every tool whose name happens to contain it. A server offering
        // both would otherwise have had one of them silently join the gate the
        // day patterns landed.
        let narrow = hook("gate", &[HookPhase::Before], &["write_file"]);
        assert!(narrow.fires(HookPhase::Before, "write_file"));
        assert!(!narrow.fires(HookPhase::Before, "overwrite_file"));
        assert!(!narrow.fires(HookPhase::Before, "write_file_backup"));

        // And asking for the wide version gets it.
        let wide = hook("gate", &[HookPhase::Before], &[".*write_file.*"]);
        assert!(wide.fires(HookPhase::Before, "overwrite_file"));
    }

    #[test]
    fn an_alternation_is_anchored_as_a_whole_rather_than_by_its_first_branch() {
        // `^write|write_file$` would be the naive anchoring and would answer
        // this wrong; the group is what makes both branches whole names.
        let hook = hook("gate", &[HookPhase::Before], &["write|write_file"]);
        assert!(hook.fires(HookPhase::Before, "write"));
        assert!(hook.fires(HookPhase::Before, "write_file"));
        assert!(!hook.fires(HookPhase::Before, "write_file_now"));
    }

    #[test]
    fn the_default_body_is_the_call_itself() {
        let arguments = json!({"path": "/etc/passwd"});
        let payload = Payload {
            phase: HookPhase::Before,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(action) = &hook("audit", &[HookPhase::Before], &[]).action;

        let body: Value = serde_json::from_str(&rendered(action, &payload).expect("body")).unwrap();
        assert_eq!(body["phase"], "before");
        assert_eq!(body["tool"], "read_file");
        assert_eq!(body["arguments"]["path"], "/etc/passwd");
        // `result` is an `after` field, and a `before` hook must not imply one.
        assert!(body.get("result").is_none());
    }

    #[test]
    fn an_after_hook_carries_what_the_call_produced() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: Some(ToolOutcome {
                text: "root:x:0:0".to_owned(),
                structured: None,
                is_error: true,
                latency_ms: 12,
                error: None,
            }),
        };
        let HookAction::Http(action) = &hook("audit", &[HookPhase::After], &[]).action;

        let body: Value = serde_json::from_str(&rendered(action, &payload).expect("body")).unwrap();
        assert_eq!(body["result"]["isError"], true);
        assert_eq!(body["result"]["latencyMs"], 12);
    }

    #[test]
    fn a_body_template_sees_the_call_by_name() {
        let arguments = json!({"path": "/tmp/x"});
        let payload = Payload {
            phase: HookPhase::Before,
            server: "files",
            tool: "write_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::Before], &[]).action;
        action.body = Some("{{ tool }} on {{ server }} with {{ arguments.path }}".to_owned());

        assert_eq!(
            rendered(&action, &payload).expect("render"),
            "write_file on files with /tmp/x"
        );
    }

    #[test]
    fn a_body_template_that_reads_something_undefined_is_an_error() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::Before,
            server: "files",
            tool: "write_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::Before], &[]).action;
        // A payload silently missing the field it was meant to carry is a webhook
        // that looks like it works.
        action.body = Some("{{ env.MIRE_DEFINITELY_NOT_SET }}".to_owned());

        let message = rendered(&action, &payload).expect_err("undefined");
        assert!(message.contains("undefined"), "{message}");
    }

    #[test]
    fn a_body_template_reads_what_the_run_captured() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::After], &[]).action;
        action.body = Some(r#"{"session": "{{ vars.session }}"}"#.to_owned());
        let vars = Captured::from([("session".to_owned(), json!("abc-123"))]);

        let (body, _) = with_vars(&action, &payload, &[], &vars).expect("render");

        assert_eq!(body, r#"{"session": "abc-123"}"#);
    }

    #[test]
    fn a_url_template_is_rendered_from_the_same_variables() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::After], &[]).action;
        action.url =
            HookUrl::Template("https://audit.internal/sessions/{{ vars.session }}".to_owned());
        let vars = Captured::from([("session".to_owned(), json!("abc-123"))]);

        let (_, url) = with_vars(&action, &payload, &[], &vars).expect("render");

        assert_eq!(url.as_str(), "https://audit.internal/sessions/abc-123");
    }

    #[test]
    fn a_url_template_also_sees_the_call_it_fired_around() {
        // One context for `url:` and `body:`, so anything the payload carries is
        // addressable — not a second, smaller vocabulary to look up.
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::Before,
            server: "files",
            tool: "write_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::Before], &[]).action;
        action.url = HookUrl::Template("https://audit.internal/{{ server }}/{{ tool }}".to_owned());

        let (_, url) = with_vars(&action, &payload, &[], &Captured::new()).expect("render");

        assert_eq!(url.as_str(), "https://audit.internal/files/write_file");
    }

    #[test]
    fn a_url_template_reading_a_variable_nobody_captured_is_an_error() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::After], &[]).action;
        action.url =
            HookUrl::Template("https://audit.internal/sessions/{{ vars.session }}".to_owned());

        // Rendering it loosely would send the request to `/sessions/`, which is a
        // different endpoint that may well answer `200`.
        let (message, _) =
            with_vars(&action, &payload, &[], &Captured::new()).expect_err("undefined");

        assert!(message.contains("undefined"), "{message}");
        assert!(message.contains("url"), "{message}");
    }

    #[test]
    fn a_url_template_that_renders_to_something_that_is_not_a_url_says_what_it_produced() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::After], &[]).action;
        action.url = HookUrl::Template("{{ vars.wherever }}".to_owned());
        let vars = Captured::from([("wherever".to_owned(), json!("not a url"))]);

        let (message, _) = with_vars(&action, &payload, &[], &vars).expect_err("not a url");

        // The rendered text, because that is the part nobody can see by reading
        // the file.
        assert!(message.contains("not a url"), "{message}");
    }

    #[test]
    fn a_fixed_url_is_used_as_written_whatever_the_run_captured() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(action) = hook("audit", &[HookPhase::After], &[]).action;
        let vars = Captured::from([("session".to_owned(), json!("abc-123"))]);

        let (_, url) = with_vars(&action, &payload, &[], &vars).expect("render");

        assert_eq!(url.as_str(), "https://audit.internal/events");
    }

    #[test]
    fn a_url_is_only_a_template_when_it_contains_one() {
        assert!(!HookUrl::is_template("https://audit.internal/events"));
        assert!(HookUrl::is_template("https://audit.internal/{{ vars.id }}"));
        assert!(HookUrl::is_template(
            "https://audit.internal/{% if vars.id %}x{% endif %}"
        ));
    }

    #[test]
    fn a_hook_with_no_condition_fires_whatever_the_run_captured() {
        let hook = hook("audit", &[HookPhase::After], &[]);

        assert!(hook.waiting_for(&Captured::new()).is_none());
    }

    #[test]
    fn a_hook_waits_for_the_variable_it_names() {
        let hook = waiting("audit", &["session"]);

        assert_eq!(hook.waiting_for(&Captured::new()), Some("session"));

        let captured = Captured::from([("session".to_owned(), json!("abc-123"))]);
        assert!(hook.waiting_for(&captured).is_none());
    }

    #[test]
    fn every_named_variable_has_to_be_there() {
        let hook = waiting("audit", &["session", "job"]);
        let half = Captured::from([("session".to_owned(), json!("abc-123"))]);

        // Named in declaration order, so "why did it not fire" has one answer.
        assert_eq!(hook.waiting_for(&half), Some("job"));

        let both = Captured::from([
            ("session".to_owned(), json!("abc-123")),
            ("job".to_owned(), json!(7)),
        ]);
        assert!(hook.waiting_for(&both).is_none());
    }

    #[test]
    fn a_variable_captured_as_null_still_counts_as_captured() {
        // Presence, not truthiness: a path that resolved to `null` resolved, and
        // second-guessing that here would be a different rule from the one the
        // capture applied.
        let hook = waiting("audit", &["session"]);
        let captured = Captured::from([("session".to_owned(), Value::Null)]);

        assert!(hook.waiting_for(&captured).is_none());
    }

    #[test]
    fn waiting_is_decided_apart_from_the_phase_and_the_tool() {
        // Two questions, two methods: `fires` is what the file says, and this is
        // what the run has done. A hook can be right about the tool and still
        // have nothing to address itself with.
        let hook = waiting("audit", &["session"]);

        assert!(hook.fires(HookPhase::After, "read_file"));
        assert_eq!(hook.waiting_for(&Captured::new()), Some("session"));
    }

    #[test]
    fn a_skipped_hook_is_recorded_as_a_non_event_rather_than_a_failure() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let hook = waiting("audit", &["session"]);

        let record = skipped(&hook, &payload, "session");

        assert_eq!(record.skipped.as_deref(), Some("session"));
        // Not a failure, and nothing went out.
        assert!(record.error.is_none());
        assert!(!record.stopped_the_call);
        assert_eq!(record.status, 0);
        assert!(record.request.is_empty());
        // Enough to know which hook this was.
        assert_eq!(record.hook, "audit");
        assert_eq!(record.tool, "read_file");
        assert_eq!(record.url, "https://audit.internal/events");
    }

    #[test]
    fn an_undefined_variable_is_named_rather_than_left_as_undefined_value() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::After], &[]).action;
        action.url = HookUrl::Template(
            "https://audit.internal/{{ vars.session }}/{{ vars.job }}".to_owned(),
        );

        let (message, _) =
            with_vars(&action, &payload, &[], &Captured::new()).expect_err("undefined");

        // "undefined value" on a URL reading two variables leaves you guessing.
        assert!(message.contains("session"), "{message}");
        assert!(message.contains("job"), "{message}");
    }

    #[test]
    fn a_body_template_names_what_it_could_not_read_either() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::After], &[]).action;
        action.body = Some(r#"{"session": "{{ vars.session }}"}"#.to_owned());

        let (message, _) =
            with_vars(&action, &payload, &[], &Captured::new()).expect_err("undefined");

        assert!(message.contains("session"), "{message}");
    }

    #[test]
    fn an_error_names_the_variable_and_never_the_template() {
        // A `body:` may hold a literal credential. An error message is exactly
        // where one must not end up — the same rule the headers follow.
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::After], &[]).action;
        action.body = Some(r#"{"key": "hunter2-{{ vars.suffix }}"}"#.to_owned());

        let (message, _) =
            with_vars(&action, &payload, &[], &Captured::new()).expect_err("undefined");

        assert!(message.contains("suffix"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }

    #[test]
    fn the_default_payload_says_nothing_about_the_variables() {
        // `vars` is for a template that asked for it. A hook shipping a run's
        // whole variable bag to a third party by default is a decision nobody
        // made — the same rule `env` follows.
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let HookAction::Http(action) = hook("audit", &[HookPhase::After], &[]).action;
        let vars = Captured::from([("session".to_owned(), json!("abc-123"))]);

        let (body, _) = with_vars(&action, &payload, &[], &vars).expect("render");

        assert!(!body.contains("abc-123"), "{body}");
        assert!(!body.contains("vars"), "{body}");
    }

    #[test]
    fn no_files_pattern_attaches_nothing_however_much_the_run_carried() {
        // The asymmetry with `tools:` is the point: empty there is everything,
        // empty here is nothing. A hook that shipped somebody's attachments to a
        // third party unless told otherwise would be a leak, not a default.
        let uploads = vec![upload("report.pdf", Some("application/pdf"), b"%PDF-1.7")];
        assert!(select(&[], &uploads).is_empty());
    }

    #[test]
    fn a_files_pattern_picks_the_uploads_it_names() {
        let uploads = vec![
            upload("report.pdf", Some("application/pdf"), b"%PDF"),
            upload("notes.txt", Some("text/plain"), b"ping"),
            upload("report.pdf.bak", None, b"old"),
        ];
        let patterns = vec![NamePattern::compile(r".*\.pdf").expect("pattern")];

        let picked = select(&patterns, &uploads);
        // Anchored, so the backup is not a `.pdf` however much it looks like one.
        assert_eq!(picked.len(), 1);
        assert_eq!(picked[0].name, "report.pdf");
    }

    #[test]
    fn the_default_payload_describes_the_files_rather_than_repeating_them() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::Before,
            server: "files",
            tool: "write_file",
            arguments: &arguments,
            result: None,
        };
        let attached = upload("report.pdf", Some("application/pdf"), b"%PDF-1.7");
        let HookAction::Http(action) = &hook("audit", &[HookPhase::Before], &[]).action;

        let sent: Value =
            serde_json::from_str(&with_files(action, &payload, &[&attached]).expect("body"))
                .unwrap();
        assert_eq!(sent["files"][0]["name"], "report.pdf");
        assert_eq!(sent["files"][0]["size"], 8);
        assert_eq!(sent["files"][0]["contentType"], "application/pdf");
        // The bytes went out as a part. Sending them here too would be sending
        // every file twice.
        assert!(sent["files"][0].get("base64").is_none());
    }

    #[test]
    fn a_body_template_can_reach_the_bytes_when_it_asks_for_them() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::Before,
            server: "files",
            tool: "write_file",
            arguments: &arguments,
            result: None,
        };
        let attached = upload("notes.txt", Some("text/plain"), b"ping");
        let HookAction::Http(mut action) = hook("audit", &[HookPhase::Before], &[]).action;
        action.body = Some("{{ uploads[0].name }}:{{ uploads[0].text }}".to_owned());

        assert_eq!(
            with_files(&action, &payload, &[&attached]).expect("render"),
            "notes.txt:ping"
        );
    }

    #[test]
    fn an_upload_with_no_guessable_type_still_goes_out_as_something() {
        // `application/octet-stream` is what a part with no better idea is
        // supposed to say, and a part with no type at all is one an endpoint has
        // to guess about.
        let unknown = upload("blob.whatever", None, b"\x00\x01");
        assert_eq!(mime_of(&unknown), "application/octet-stream");
        assert_eq!(describe(&unknown).content_type, "application/octet-stream");
    }

    #[test]
    fn a_form_carries_the_payload_beside_every_file() {
        let attached = [
            upload("a.txt", Some("text/plain"), b"one"),
            upload("b.png", Some("image/png"), &[0x89, b'P']),
        ];
        let refs: Vec<&UploadRef> = attached.iter().collect();

        let built = form("{\"phase\":\"before\"}".to_owned(), &refs).expect("form");
        // `Form` does not hand its parts back, so the boundary is what there is
        // to assert on: it exists, which means the two files and the payload
        // encoded without anybody complaining about a media type.
        assert!(!built.boundary().is_empty());
    }

    #[test]
    fn a_refusal_with_nothing_to_say_still_reads_as_a_sentence() {
        assert_eq!(
            refused(reqwest::StatusCode::FORBIDDEN, "   "),
            "answered 403 Forbidden with an empty body"
        );
        assert_eq!(
            refused(reqwest::StatusCode::FORBIDDEN, "not on my watch"),
            "answered 403 Forbidden: not on my watch"
        );
    }

    #[test]
    fn a_long_response_is_cut_rather_than_pasted_into_the_error() {
        let long = "x".repeat(500);
        let cut = snippet(&long);
        assert!(cut.ends_with('…'), "{cut}");
        assert_eq!(cut.chars().count(), 201);
        assert_eq!(snippet("  short  "), "short");
    }
}
