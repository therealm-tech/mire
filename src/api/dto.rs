//! Request and response shapes for the HTTP API.
//!
//! Deliberately separate from the domain types: what goes on the wire is a
//! contract with the UI, and the UI is not allowed to depend on the internals.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use url::Url;
use validator::Validate;

use crate::agent::{AgentInput, Trace, Turn};
use crate::auth::registry::AuthDescriptor;
use crate::exec::{CallEvent, CallInput, CallOutcome};
use crate::issue::LoadIssue;
use crate::message::Message;
use crate::model::loader::ModelSet;
use crate::model::{Model, ModelKind};
use crate::prompt::{Prompt, PromptRegistry};
use crate::redact::Secret;

/// One model, as listed.
// Four independent facts about an endpoint, each of which the UI acts on alone.
// The lint's advice — a state machine, or two-variant enums — would be a wire
// contract invented for the linter rather than for the reader.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelSummary {
    /// How the model is addressed: `name`, or `name@stage`. This is what
    /// `POST /api/call` takes.
    pub id: String,
    /// What the file called it. Two stages of one file share it, which is what
    /// lets the UI offer them as one model with a stage to pick.
    pub name: String,
    /// Stage this reading of the file belongs to, absent when it declares none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    /// `true` when a bare `name` means this reading — the file's
    /// `default_stage:`, and `true` for a file that declares no stages at all.
    /// The UI marks it, because that is the stage another file's `auth:` or a
    /// hand-written call reaches without saying so.
    pub is_default: bool,
    /// What the endpoint does.
    pub kind: ModelKind,
    /// `false` when the model takes no typed message, so the composer hides
    /// the box instead of holding **Send** back for one.
    pub has_prompt: bool,
    /// Where it points.
    pub url: Url,
    /// Auth provider the model defaults to, for the call to the model.
    pub auth: Option<String>,
    /// File it was read from.
    pub source: String,
    /// `false` when the model has no `decode:` block yet, so the UI can offer
    /// the assisted discovery flow instead of showing an empty result.
    pub has_decode: bool,
    /// `true` when the model declares `requires_upload:`, so the composer can
    /// say so before **Send** rather than after the `422` — the refusal is the
    /// server's either way.
    pub requires_upload: bool,
}

impl ModelSummary {
    /// Summarises one model. `is_default` is the set's answer rather than the
    /// model's: which stage a bare name means is a property of the file the
    /// stages came from, and a model on its own has never seen its siblings.
    #[must_use]
    pub fn new(model: &Model, is_default: bool) -> Self {
        Self {
            id: model.id(),
            name: model.name.clone(),
            stage: model.stage.clone(),
            is_default,
            kind: model.kind,
            has_prompt: model.has_prompt,
            url: model.url.clone(),
            auth: model.auth.clone(),
            source: model.source.display().to_string(),
            has_decode: !model.decode.is_empty(),
            requires_upload: model.requires_upload,
        }
    }
}

/// The models directory: what loaded, and what did not.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ModelsResponse {
    /// Models that parsed and validated.
    pub models: Vec<ModelSummary>,
    /// Files that did not, with the reason and position.
    pub issues: Vec<LoadIssue>,
}

impl ModelsResponse {
    /// The models directory as the UI reads it.
    #[must_use]
    pub fn new(set: &ModelSet) -> Self {
        Self {
            models: set
                .iter()
                .map(|model| ModelSummary::new(model, set.is_default(model)))
                .collect(),
            issues: set.issues().to_vec(),
        }
    }
}

/// Every prompt the UI can drop in the box.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PromptsResponse {
    /// Declared prompts, in the order the listing gives them — a library is
    /// a list somebody arranged, so it is not re-sorted on the way out.
    pub prompts: Vec<Prompt>,
    /// Entries of `prompts/` that did not load.
    pub issues: Vec<LoadIssue>,
}

impl From<&PromptRegistry> for PromptsResponse {
    fn from(registry: &PromptRegistry) -> Self {
        Self {
            prompts: registry.prompts().to_vec(),
            issues: registry.issues().to_vec(),
        }
    }
}

/// Every auth provider the UI can offer.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthResponse {
    /// Declared providers, plus the built-in anonymous one.
    pub providers: Vec<AuthDescriptor>,
    /// Entries of `auth/` that did not load. `anonymous` still works.
    pub issues: Vec<LoadIssue>,
}

/// Every MCP server the agent loop may call.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpResponse {
    /// Declared servers.
    pub servers: Vec<crate::mcp::registry::McpDescriptor>,
    /// Every revision this build speaks, newest first.
    ///
    /// Here so that a client offering the choice does not have to keep its own
    /// copy of the list: what `mire` can speak is `mire`'s to say, and a UI that
    /// hard-codes it is a UI that offers a revision the server was never built
    /// with the day one is added or dropped.
    pub revisions: Vec<crate::mcp::Revision>,
    /// Entries of `mcp/` that did not load.
    pub issues: Vec<LoadIssue>,
}

/// What one server currently advertises.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpToolsResponse {
    /// Registry name of the server that was asked.
    pub server: String,
    /// The revision the exchange actually happened on, and how that was settled.
    ///
    /// Never omitted: a listing that does not say which protocol produced it is
    /// exactly the ambiguity this endpoint exists to remove.
    pub protocol: crate::mcp::Session,
    /// Its tools, as it describes them right now.
    pub tools: Vec<crate::mcp::McpTool>,
}

/// One file, as stored.
///
/// `name` is what the browser called it and `storedAs` is what it is actually
/// called on disk — they differ, always, because the stored name carries a
/// random prefix and has been reduced to one safe path segment. Showing the
/// first and writing the second is the whole point: the display name is the
/// user's, the file name is ours.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UploadResponse {
    /// The handle: the random prefix of the stored name, on its own.
    pub id: String,
    /// The name the browser sent, untouched.
    pub name: String,
    /// What it is called in the upload directory.
    pub stored_as: String,
    /// Where it landed, so a human can go and look at it.
    pub path: String,
    /// Size in bytes, as written.
    pub size: u64,
    /// Content type the browser claimed, when it claimed one.
    ///
    /// Unverified and unused: it is the client's word about its own file, kept
    /// because it is the only hint anybody has about what the bytes are.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
}

impl From<crate::uploads::StoredFile> for UploadResponse {
    fn from(file: crate::uploads::StoredFile) -> Self {
        Self {
            id: file.id,
            name: file.original_name,
            stored_as: file.stored_name,
            path: file.path.display().to_string(),
            size: file.size,
            content_type: file.content_type,
        }
    }
}

/// Naming an MCP server in the path.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct McpPath {
    /// Server id: `name`, or `name@stage`, as declared in `mcp/`.
    pub id: String,
}

/// Text to embed: one string, or several.
///
/// Both spellings exist in the wild for `input`, and typing `["ping"]` to embed
/// one sentence is the kind of friction this tool is supposed to remove. The
/// template always sees a list.
#[derive(Debug, Clone, Default)]
pub struct TextInput(Vec<String>);

impl From<TextInput> for Vec<String> {
    fn from(input: TextInput) -> Self {
        input.0
    }
}

impl<'de> Deserialize<'de> for TextInput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }

        Ok(Self(match OneOrMany::deserialize(deserializer)? {
            OneOrMany::One(text) => vec![text],
            OneOrMany::Many(texts) => texts,
        }))
    }
}

impl JsonSchema for TextInput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "TextInput".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "description": "Text to embed: a single string, or a list of strings.",
            "oneOf": [
                {"type": "string"},
                {"type": "array", "items": {"type": "string"}}
            ]
        })
    }
}

/// Naming a model in the path.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ModelPath {
    /// Model id: `name`, or `name@stage`. A bare name is the default stage.
    pub id: String,
}

/// Path parameters for the auth routes.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AuthPath {
    /// Auth provider id: `name`, or `name@stage`, as declared in `auth/`.
    pub id: String,
}

/// Start a browser login.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoginRequest {
    /// Where the identity provider should send the browser back.
    ///
    /// The UI computes this from `document.baseURI`, which is the only place the
    /// public URL is actually known: behind a Kubeflow notebook proxy, `mire`
    /// binds `127.0.0.1:8787` while the browser is at
    /// `https://kubeflow.example/notebook/<ns>/<name>/proxy/8787/`. Nothing in the
    /// process can derive the second from the first.
    ///
    /// Ignored when `--public-url` is set, which is the escape hatch for a proxy
    /// that rewrites paths. Whatever ends up here must still be registered with
    /// the identity provider — that check is the one that matters, and it is not
    /// ours.
    #[serde(default)]
    pub redirect_uri: Option<String>,

    /// OIDC `prompt`. Send `login` to make the identity provider ask again
    /// instead of silently reusing its own session.
    ///
    /// This is the way out of a stuck login: with an established SSO session the
    /// authorization endpoint redirects straight back, so a broken attempt
    /// repeats itself instantly and there is nothing to interact with.
    #[serde(default)]
    pub prompt: Option<String>,
}

/// Where to send the browser, and what will come back.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    /// The authorization endpoint, with the PKCE challenge and state attached.
    pub authorization_url: String,
    /// The callback that was resolved, echoed so a mismatch with what the
    /// identity provider has registered is one glance away.
    pub redirect_uri: String,
    /// Opaque, single-use. The UI does not need it; it makes the flow debuggable.
    pub state: String,
}

/// What a sign-out did.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogoutResponse {
    /// `false` when nobody was signed in to begin with.
    pub signed_out: bool,
}

/// What the identity provider sends back to the callback.
///
/// Every field is optional because both halves of RFC 6749 §4.1.2 land here: the
/// success pair (`code` + `state`) and the failure pair (`error` +
/// `error_description`).
#[derive(Debug, Default, Deserialize)]
pub struct CallbackQuery {
    /// The authorization code, on success.
    pub code: Option<String>,
    /// The state we minted when the login started.
    pub state: Option<String>,
    /// The error identifier, on refusal.
    pub error: Option<String>,
    /// Its human-readable companion.
    pub error_description: Option<String>,
}

/// One call.
#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[schemars(extend("example" = serde_json::json!({
    "model": "mistral-small",
    "auth": "anonymous",
    "prompt": "ping"
})))]
#[serde(rename_all = "camelCase")]
pub struct CallRequest {
    /// Model to run, as `name` or `name@stage`. A bare name is the model's
    /// default stage.
    #[validate(length(min = 1))]
    pub model: String,

    /// Auth provider, overriding the model's own. Omit to use the model's,
    /// which is the point of the three-mode replay.
    ///
    /// Takes a stage of its own — `keycloak-workload@preprod` — and it has
    /// nothing to do with the model's: a `prod` model authenticated by a
    /// `preprod` credential is a question somebody legitimately asks.
    #[serde(default)]
    pub auth: Option<String>,

    /// Shorthand for a single user message. Ignored when `messages` is given.
    #[serde(default)]
    pub prompt: Option<String>,

    /// Full conversation. `kind: chat`.
    #[serde(default)]
    pub messages: Vec<Message>,

    /// Text to embed. `kind: embedding`. A single string is accepted too.
    #[serde(default)]
    pub input: TextInput,

    /// Template knobs, reachable as `params.x`.
    #[serde(default)]
    pub params: Map<String, Value>,

    /// Ids of stored files to hand the template, from `POST /api/uploads`.
    ///
    /// They arrive as `uploads`, in this order, each carrying the file whole —
    /// `base64`, `dataUrl` and `text`. Attaching a file does nothing on its own:
    /// a template that never mentions `uploads` sends exactly what it always
    /// sent, which is the same rule `stream` follows.
    ///
    /// Ids rather than names, because a name is a path and a path is a way out
    /// of the upload directory.
    #[serde(default)]
    pub uploads: Vec<String>,

    /// Model identifier handed to the template, overriding whatever it bakes in.
    ///
    /// Reaches it as `model_id`, and names the model the *endpoint* serves — not
    /// the `model:` of this request, which is the entry in `models/` being run.
    #[serde(default)]
    pub model_id: Option<String>,

    /// Credential for a provider that declares no source. Never stored, never
    /// echoed back.
    #[serde(default)]
    pub token: Option<Secret>,

    /// Ask the endpoint to stream. `kind: chat`.
    ///
    /// Reaches the template as `stream`, so it only takes effect if the template
    /// passes it on — nothing here makes an endpoint chunk its answer.
    ///
    /// Off by default, and independent of which endpoint is asked: `POST
    /// /api/agent` streams every turn of the loop when it is on, and `POST
    /// /api/call/stream` forces it on because reading chunks is the whole of what
    /// that route does. `POST /api/call` reads a whole body, so asking it for a
    /// stream is asking it to parse an event stream as JSON — which it will
    /// report as a body it could not read, honestly and uselessly.
    #[serde(default)]
    pub stream: bool,

    /// Attach the full vectors to an embedding response.
    ///
    /// Off by default on purpose: the summaries (width, norm, sample, histogram)
    /// are what you actually read, and a page of 1024 floats is not.
    #[serde(default)]
    pub include_vectors: bool,

    /// Send the request this many times. `kind: embedding` only.
    ///
    /// Two or more enables the determinism check: the same input must produce the
    /// same vectors. Extra runs are discarded apart from that comparison.
    #[serde(default = "one")]
    #[validate(range(min = 1, max = 5))]
    pub repeat: u8,

    /// Largest absolute difference two runs may show and still count as
    /// deterministic.
    #[serde(default = "default_tolerance")]
    pub tolerance: f32,
}

fn one() -> u8 {
    1
}

fn default_tolerance() -> f32 {
    1e-6
}

impl From<CallRequest> for CallInput {
    fn from(request: CallRequest) -> Self {
        let messages = if request.messages.is_empty() {
            request.prompt.map(Message::user).into_iter().collect()
        } else {
            request.messages
        };

        Self {
            model: request.model,
            auth: request.auth,
            messages,
            input: request.input.into(),
            params: request.params,
            model_id: request.model_id,
            token: request.token,
            stream: request.stream,
            include_vectors: request.include_vectors,
            repeat: request.repeat,
            tolerance: request.tolerance,
            // Both filled in after this: the tools by the agent loop from the
            // model's MCP servers, the uploads by the handler, which is the
            // only layer allowed to read a directory.
            extra_tools: Vec::new(),
            uploads: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_is_shorthand_for_one_user_message() {
        let request: CallRequest =
            serde_json::from_value(serde_json::json!({"model": "p", "prompt": "ping"})).unwrap();
        let input: CallInput = request.into();

        assert_eq!(input.messages.len(), 1);
        assert_eq!(input.messages[0].content.as_deref(), Some("ping"));
    }

    #[test]
    fn explicit_messages_win_over_prompt() {
        let request: CallRequest = serde_json::from_value(serde_json::json!({
            "model": "p",
            "prompt": "ignored",
            "messages": [{"role": "system", "content": "kept"}],
        }))
        .unwrap();
        let input: CallInput = request.into();

        assert_eq!(input.messages.len(), 1);
        assert_eq!(input.messages[0].content.as_deref(), Some("kept"));
    }

    #[test]
    fn a_supplied_token_never_reappears_in_debug_output() {
        let request: CallRequest =
            serde_json::from_value(serde_json::json!({"model": "p", "token": "s3cr3t-value"}))
                .unwrap();
        assert!(!format!("{request:?}").contains("s3cr3t-value"));
    }

    #[test]
    fn a_multi_word_field_is_camel_case_on_the_wire() {
        let request: CallRequest =
            serde_json::from_value(serde_json::json!({"model": "p", "includeVectors": true}))
                .unwrap();
        assert!(request.include_vectors);
    }
}

/// One agent run.
#[derive(Debug, Deserialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
#[schemars(extend("example" = serde_json::json!({
    "model": "qwen3",
    "prompt": "What is the weather in Paris?",
    "maxTurns": 6
})))]
pub struct AgentRequest {
    /// Everything a single call needs. `includeVectors`, `repeat` and
    /// `tolerance` are ignored: they belong to `kind: embedding`, and agent mode
    /// runs a chat model.
    #[serde(flatten)]
    #[validate(nested)]
    pub call: CallRequest,

    /// Turn budget, overriding the model's `agent.default_max_turns`.
    #[serde(default)]
    #[validate(range(min = 1, max = 50))]
    pub max_turns: Option<u32>,

    /// Which of the model's MCP servers this run may reach.
    ///
    /// Omit it — the default — and the run reaches every server the model
    /// names, which is what the file says. A list narrows that to the ones named,
    /// `[]` included: a loop with no server set up offers the model the model's
    /// simulated `tools:` and nothing else, which is how you ask what it does
    /// when the tool it wants is not there, without editing the model.
    ///
    /// It only narrows. A server this model does not name is a `422`, not a
    /// server this run gets to add: `mcp:` is opt-in per model because it is
    /// the one thing here with effects outside the process, and a request is not
    /// where that opt-in is granted.
    #[serde(default)]
    pub mcp_servers: Option<Vec<String>>,

    /// Revision to speak to every MCP server this run touches.
    ///
    /// Omit it — the default — and each server settles its own the way it always
    /// did: `protocol_version:` from `mcp/` when it has one, the negotiation
    /// otherwise. Naming one here overrides both, for this run only, which is
    /// what makes "does my endpoint still work on `2025-03-26`?" a question you
    /// answer by asking rather than by editing a file.
    #[serde(default)]
    pub mcp_protocol: Option<crate::mcp::Revision>,
}

impl From<AgentRequest> for AgentInput {
    fn from(request: AgentRequest) -> Self {
        Self {
            call: request.call.into(),
            max_turns: request.max_turns,
            mcp_servers: request.mcp_servers,
            mcp_protocol: request.mcp_protocol,
        }
    }
}

/// What `POST /api/call/stream` streams, one per server-sent event.
///
/// The two live events carry only what cannot wait: the head, and the text. The
/// `done` event is the same [`CallOutcome`] the non-streaming endpoint returns,
/// so a client can ignore the deltas entirely and still get the full answer —
/// which is what makes this endpoint a superset of `POST /api/call` rather than
/// a separate thing to support.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum StreamEvent {
    /// The response head arrived, long before the body. A `401` shows up here.
    Open {
        /// HTTP status.
        status: u16,
        /// Response headers, masked.
        headers: BTreeMap<String, String>,
    },
    /// A chunk carried text. Masked, like everything else that leaves the process.
    Delta {
        /// The text of this chunk alone, not the aggregate.
        text: String,
    },
    /// The stream closed. Carries the whole outcome, deltas already aggregated.
    Done(Box<CallOutcome>),
    /// The call could not be made or the stream broke before anything arrived.
    Failed {
        /// Stable identifier.
        code: String,
        /// What went wrong.
        message: String,
    },
}

impl StreamEvent {
    /// The server-sent event name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Open { .. } => "open",
            Self::Delta { .. } => "delta",
            Self::Done(_) => "done",
            Self::Failed { .. } => "failed",
        }
    }
}

impl From<CallEvent> for StreamEvent {
    fn from(event: CallEvent) -> Self {
        match event {
            CallEvent::Open { status, headers } => Self::Open { status, headers },
            CallEvent::Delta { text } => Self::Delta { text },
        }
    }
}

/// What `POST /api/agent` streams, one per server-sent event.
///
/// The event name (`turn`, `done`, `failed`) is what a client dispatches on; the
/// payload is this.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum AgentEvent {
    /// What was said to the MCP servers before the loop began: discovery, the
    /// handshake, `tools/list`. Sent only when there was any.
    Setup {
        /// The exchanges, in the order they happened.
        mcp: Vec<crate::mcp::McpExchange>,
    },
    /// A chunk of the turn in flight carried text. Only when the request asked
    /// the run to `stream`.
    ///
    /// It names its turn, because a loop writes several answers one after
    /// another and a client aggregating deltas has nothing else to tell them
    /// apart. Every one of them is in the `turn` event that follows anyway —
    /// these are the live copy, not the only one.
    Delta {
        /// The turn being written, counting from one.
        turn: u32,
        /// The text of this chunk alone, not the aggregate.
        text: String,
    },
    /// A turn completed. Sent as it happens, not at the end.
    Turn(Box<Turn>),
    /// The loop ended. Carries the whole trace, so a client that missed events
    /// still gets everything.
    Done(Box<Trace>),
    /// The run could not continue. Not the same as a loop that stopped badly —
    /// that is a [`StopOutcome`] inside `done`.
    Failed {
        /// Stable identifier.
        code: String,
        /// What went wrong.
        message: String,
    },
}

impl AgentEvent {
    /// The server-sent event name.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Setup { .. } => "setup",
            Self::Delta { .. } => "delta",
            Self::Turn(_) => "turn",
            Self::Done(_) => "done",
            Self::Failed { .. } => "failed",
        }
    }
}
