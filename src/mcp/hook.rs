//! Something that happens around a `tools/call`.
//!
//! A live MCP tool is the one thing `mire` does that has effects outside this
//! process, which makes it the one thing somebody else usually wants to know
//! about: an audit trail, a policy gate, an upload endpoint that has to be
//! handed the inputs before a task runs. A hook is that — declared on the server in
//! `mcp.yaml`, fired [`Before`](HookPhase::Before) the call goes out,
//! [`After`](HookPhase::After) it comes back, or both.
//!
//! ```yaml
//! hooks:
//!   - name: audit
//!     on:
//!       - before
//!       - after
//!     actions:
//!       - http:
//!           url: https://audit.internal/tool-calls
//!           auth: keycloak-workload
//!           json:
//!             tool: '{{ tool }}'
//!             arguments: '{{ arguments }}'
//! ```
//!
//! # Why a hook has several actions
//!
//! Because one event is usually two calls to two different people. The file goes
//! to the API that is about to run it, the line goes to the audit sink; both
//! belong to the same firing, both want their own address, credential and body.
//! `actions:` is a list, they go out in the order written, and each produces its
//! own [`HookRecord`].
//!
//! # Why the action is tagged
//!
//! `- http:` is the only kind today and the only one worth having first: an HTTP
//! call is what every audit sink, policy service and upload endpoint already
//! speaks. The kind is the key it is written under anyway, because the
//! alternative is a second shape bolted onto a flat struct later, and a `kind:`
//! added after the fact is a breaking change to every file that already exists.
//!
//! # A hook can stop a call
//!
//! `on_error: fail` — the default — means a hook that could not be run, or whose
//! endpoint answered outside `2xx`, is a failure of the tool call it belongs to.
//! For a [`Before`](HookPhase::Before) hook that is a policy gate for free: the
//! `tools/call` never goes out, and neither do the hook's remaining actions. For
//! an [`After`](HookPhase::After) hook it is a report rather than an undo — the
//! tool already ran, and nothing here can take that back. `on_error: continue`
//! records the failure, moves on to the next action, and gets out of the way.
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
//! `if:` is a `MiniJinja` expression asked once per firing. True and the hook
//! fires; false and it sits the call out. It sees exactly what `url:`, `json:`
//! and `headers:` see — `phase`, `tool`, `arguments`, `result`, `env`, `vars`,
//! `uploads` — because a condition about a call and a body about the same call
//! should not need two vocabularies.
//!
//! ```yaml
//!     if: '{{ vars.session is defined }}'
//!     actions:
//!       - http:
//!           url: https://audit.internal/sessions/{{ vars.session }}/tool-calls
//! ```
//!
//! It is the graceful counterpart to the loud undefined above, and the pairing
//! is the point: without it, that URL fails every call made before a session
//! exists; with it, the hook simply waits and starts firing once one does.
//!
//! Being an expression rather than a list of names is what makes the rest
//! reachable — the audit that only cares about writes that actually worked, the
//! gate that only asks about the big files:
//!
//! ```yaml
//!     if: '{{ result.is_error == false }}'
//!     if: '{{ arguments.size > 1048576 }}'
//!     if: '{{ vars.session is defined and env.STAGE == "prod" }}'
//! ```
//!
//! **Undefined is false here, not an error** — the one place in a hook where it
//! is. Everywhere else a template names something absent, the request would go
//! out with a hole in it, so it fails loudly; a condition is *asking* whether
//! something is there, and it has to be able to hear "no" without falling over.
//! Lookups chain, so `{{ vars.job.id }}` on a run with no `job` is false rather
//! than a failure about `id`. A condition that cannot be evaluated at all — an
//! unknown filter, a call to something that is not callable — is still a hook
//! failure, and `on_error` decides what that does to the call.
//!
//! **Not firing is not failing.** `on_error` does not apply, nothing is sent, no
//! credential is resolved, and the tool call proceeds untouched. The skip is
//! recorded all the same, once per action, quoting the condition that came back
//! false — see [`HookRecord::skipped`]. A hook that quietly never ran and a hook
//! that was never declared must not look the same in a trace.
//!
//! The expression is compiled when `mcp.yaml` loads, so a typo in its syntax is
//! a startup issue naming the hook. What it *reads* is checked against nothing:
//! a variable may be captured by this server, by another one the run happens to
//! reach, or by nobody — so a condition naming a variable nothing ever captures
//! shows up as a skip quoting it, run after run, rather than as a startup error
//! about a file that may be perfectly correct.
//!
//! # What it sends
//!
//! Whatever the file says, and nothing otherwise. An action declaring neither
//! `json:` nor `multipart:` sends **no body at all** — see [`HookBody`] for why
//! that is the only defensible default; the names a template can reach are the
//! ones listed under *What it sends* below.
//!
//! `json:` is a document, not a string: written as YAML, sent as JSON, with every
//! string in it a template.
//!
//! ```yaml
//!           json:
//!             who: '{{ env.USER }}'
//!             ran: '{{ tool }}'
//!             arguments: '{{ arguments }}'
//!             attempt: 1
//! ```
//!
//! A string that is one `{{ … }}` and nothing else keeps the type of what it
//! names, so `arguments` above is the arguments *object* rather than a quoted
//! rendering of one; text around the expression makes it a string again, because
//! that is what interpolation is for. `{{ call }}` is the whole call in one
//! line, for the audit sink that wants exactly that:
//!
//! ```yaml
//!           json: '{{ call }}'
//! ```
//!
//! `auth` is deliberately **not** in scope there. A credential belongs in a
//! header, where the redactor is; a body that can reach the auth registry is a
//! credential one typo away from a webhook's access log.
//!
//! A hook's own `headers:` see `vars` too, beside the `env` and `auth` they
//! already had — see [`super::headers`]. Unlike a server's, they need no
//! `| default(...)` to be safe: a hook only renders around a call, so anything
//! captured before that call is there.
//!
//! # Where it sends it
//!
//! `url:` is ordinarily just a URL, parsed and checked when `mcp.yaml` loads. It
//! may also be a template, seeing exactly what `json:` sees — one context, so
//! there is no second table to remember:
//!
//! ```yaml
//!           url: https://audit.internal/sessions/{{ vars.session }}/tool-calls
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
//! # Sending the run's files
//!
//! `multipart:` is one entry per form field, each naming uploads of the run. The
//! request becomes a `multipart/form-data` carrying exactly those, under exactly
//! those field names — which is what an upload endpoint asking for `file` is
//! actually asking for.
//!
//! ```yaml
//!           multipart:
//!             file: '{{ uploads[0].path }}'
//! ```
//!
//! A field can carry several files — write a list, or an expression producing
//! one — and they go out as several parts under the same name, which is what
//! every server-side upload handler already reads:
//!
//! ```yaml
//!           multipart:
//!             file: '{{ uploads }}'
//! ```
//!
//! Each entry names **one** upload: the object itself, or a string matching its
//! `path`, `name` or `id`. A field that names something the run is not carrying
//! fails the hook, and so does one that resolves to nothing at all. Sending a
//! form with a part missing is how an endpoint ends up
//! explaining our own configuration back to us, in the form of a `422`.
//!
//! `json:` and `multipart:` are mutually exclusive, refused together at load: a
//! request is one body.
//!
//! The files reach a `json:` template as `uploads`, whole — `base64`, `dataUrl`,
//! `text`, the entries a model template gets from the same run — for the webhook
//! that wants the bytes inline instead. The trace never repeats them: it names
//! the field, the file and its size, because 25 MB of base64 in a panel costs
//! everything and tells nobody anything.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use minijinja::value::{Value as Rendered, ValueKind};
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

/// `if:` renders leniently, for the opposite reason — and it is the only place
/// in a hook that does.
///
/// Strictness above protects a request that is about to go out: a body missing
/// the field it promised is worse than no request at all. A condition is the
/// question of whether that request should exist, and `{{ vars.session is
/// defined }}` has to be answerable on the run where it is not. Chainable, so
/// `{{ vars.job.id }}` on a run carrying no `job` is false rather than a failure
/// about `id` — the question was about the job, and the answer is no.
static CONDITIONS: std::sync::LazyLock<Environment<'static>> = std::sync::LazyLock::new(|| {
    let mut environment = Environment::new();
    environment.set_undefined_behavior(UndefinedBehavior::Chainable);
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
/// One type because it is one rule — the pattern has to match the whole name.
/// See [`crate::pattern`] for why that is not negotiable.
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
    /// What has to hold for it to fire at all. `None` — the default — is no
    /// condition, and a hook that fires on every call it covers.
    pub condition: Option<HookCondition>,
    /// What its failure does to the call.
    pub on_error: OnError,
    /// What it actually does, in declaration order.
    ///
    /// A list, because one event is usually two calls to two different people:
    /// the file goes to the API that is about to run it, the line goes to the
    /// audit sink. Each action produces its own record, and the first failure
    /// decides what happens to the rest — see [`Self::on_error`].
    pub actions: Vec<HookAction>,
}

impl Hook {
    /// Whether this hook has anything to say about `tool` at `phase`.
    ///
    /// Only what the file says — `if:` is the half that depends on the call
    /// itself and on what the run has done so far, and it is asked separately
    /// for exactly that reason: a hook can be right about the tool and still
    /// have nothing to address itself with.
    #[must_use]
    pub fn fires(&self, phase: HookPhase, tool: &str) -> bool {
        self.phases.contains(&phase) && NamePattern::any_matches(&self.tools, tool)
    }
}

/// A hook's `if:`, compiled when `mcp.yaml` loaded.
///
/// One expression, evaluated per firing against the same context every other
/// template of the hook sees. Kept as text rather than as a compiled `MiniJinja`
/// expression, which would borrow its environment — and the source is wanted
/// anyway: it is what the trace quotes when the answer is no, and what
/// `GET /api/mcp` advertises before a run even starts.
#[derive(Debug, Clone)]
pub struct HookCondition {
    /// What `mcp.yaml` wrote, `{{ … }}` and all.
    source: String,
    /// The expression alone, with the delimiters — when there were any —
    /// removed. What actually gets compiled.
    expression: String,
}

impl HookCondition {
    /// Compiles `source`, or says why it is not a condition.
    ///
    /// Both spellings are accepted, and they mean the same thing: `{{ vars.x }}`
    /// is how every other template in this file is written, and a bare `vars.x`
    /// is what an expression looks like once you stop rendering it. What is
    /// refused is the third shape — text with an expression somewhere in it, or
    /// a `{% … %}` block. Those are *templates*, they produce a string, and a
    /// string that is not empty is truthy, so `{% if vars.x %}yes{% endif %}`
    /// would be a condition that holds on every call including the ones it was
    /// written to exclude.
    ///
    /// # Errors
    ///
    /// The shape refusal, or `MiniJinja`'s own message for an expression that
    /// does not parse — at load, so it names the hook rather than surfacing on
    /// the first tool call twenty minutes into a run.
    pub fn compile(source: &str) -> Result<Self, String> {
        let trimmed = source.trim();
        if trimmed.is_empty() {
            return Err("is empty; leave it out to fire on every call".to_owned());
        }

        let expression = match lone_expression(trimmed) {
            Some(inner) => inner.trim(),
            None if trimmed.contains("{{") || trimmed.contains("{%") => {
                return Err(
                    "must be one `{{ … }}` expression, not a template around one: a template \
                     produces text, and text that is not empty is true on every call"
                        .to_owned(),
                );
            }
            None => trimmed,
        };

        CONDITIONS
            .compile_expression(expression)
            .map_err(|error| root(&error))?;

        Ok(Self {
            source: trimmed.to_owned(),
            expression: expression.to_owned(),
        })
    }

    /// What `mcp.yaml` wrote, for the trace and the descriptor.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Whether this firing should happen.
    ///
    /// Truthiness is `MiniJinja`'s own, so an empty string, `0`, an empty list
    /// and an absent variable are all no — and a captured `null` is no as well.
    /// That last one is a deliberate departure from what `when_defined:` used to
    /// say: a list of names could only ask about presence, an expression is read
    /// as a question about a value, and `null` is not much of a value. Ask for
    /// presence when presence is the point — `{{ vars.session is defined }}`
    /// says so in as many words.
    ///
    /// # Errors
    ///
    /// `MiniJinja`'s message for an expression that could not be evaluated —
    /// which, undefined being false here, means a genuine mistake: an unknown
    /// filter, a call to something that is not callable.
    fn holds(&self, rendering: &Rendering<'_>) -> Result<bool, String> {
        CONDITIONS
            .compile_expression(&self.expression)
            .and_then(|compiled| compiled.eval(rendering))
            .map(|value| value.is_true())
            .map_err(|error| {
                format!(
                    "the `if` condition could not be evaluated: {}",
                    root(&error)
                )
            })
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

impl HookAction {
    /// Where it sends what it sends, as `mcp.yaml` wrote it.
    ///
    /// Its own URL, not the server's — which is the whole reason its credentials
    /// are resolved separately: an auth provider's `allowed_hosts` is a statement
    /// about where its credential may go, and a hook goes somewhere else. Per
    /// action, and not per hook, for exactly the same reason: two actions of one
    /// hook are two addresses, and one of them may be somewhere the other's
    /// credential must never reach.
    ///
    /// A [template](HookUrl::Template) is only an address once a call has been
    /// made; [`HookUrl::resolve`] is where it becomes one.
    #[must_use]
    pub const fn url(&self) -> &HookUrl {
        match self {
            Self::Http(http) => &http.url,
        }
    }

    /// The provider it authenticates with, if it names one.
    #[must_use]
    pub fn auth(&self) -> Option<&str> {
        match self {
            Self::Http(http) => http.auth.as_deref(),
        }
    }

    /// Providers its header templates read, beyond [`Self::auth`].
    pub fn header_providers(&self) -> impl Iterator<Item = &str> {
        match self {
            Self::Http(http) => http.headers.providers(),
        }
    }

    /// The kind, as `mcp.yaml` spells it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Http(_) => "http",
        }
    }
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
    /// What the request carries, or `None` for no body at all.
    ///
    /// `None` is the honest default. A hook that posted a payload nobody wrote
    /// is a request the endpoint never agreed to read, and the one thing worse
    /// than no body is one somebody has to reverse-engineer from a `422`. The
    /// call itself is still one line away — `json: "{{ call }}"` — for the audit
    /// sink that does want exactly that.
    pub body: Option<HookBody>,
    /// Per-request timeout.
    pub timeout: Duration,
}

/// What a hook's request carries.
///
/// Two shapes, because they answer two questions. [`Json`](Self::Json) is "tell
/// somebody about this call", and the endpoint being told has a schema of its
/// own — so the file writes that schema out, field by field, instead of hoping a
/// fixed payload happens to fit. [`Multipart`](Self::Multipart) is "send
/// somebody these bytes", which is the shape every upload endpoint already
/// reads.
///
/// Mutually exclusive, and `mcp.yaml` refuses both together at load rather than
/// picking one: a request cannot be a JSON document and a form at the same time,
/// and a file declaring both was written expecting something else to happen.
#[derive(Debug, Clone)]
pub enum HookBody {
    /// A JSON document, as written, with every string a template.
    ///
    /// The tree is walked on each firing: strings render, everything else goes
    /// out as it stands. A string that is one `{{ … }}` and nothing else keeps
    /// the expression's own type, so `"{{ arguments }}"` is the arguments object
    /// rather than a quoted rendering of one.
    Json(Value),
    /// A `multipart/form-data`, one entry per field.
    Multipart(Vec<PartSpec>),
}

/// One field of a multipart body, and the uploads it carries.
///
/// `sources` is a list because a field can carry several files: they go out as
/// several parts under the same name, which is what every server-side upload
/// handler already reads. Naming each part after its file would make the
/// endpoint guess field names it cannot know in advance.
#[derive(Debug, Clone)]
pub struct PartSpec {
    /// The form field, as `mcp.yaml` named it.
    pub field: String,
    /// Templates, each naming uploads of the run.
    pub sources: Vec<String>,
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
    /// Which of the hook's actions this was, counting from one.
    ///
    /// A hook is allowed several, and two of them can differ only by what they
    /// send. Without this, a trace of a hook that both uploads and audits is two
    /// cards with the same title and no way to say which is which.
    pub step: u32,
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
    /// The body it sent, masked.
    ///
    /// Empty for a hook that sent files, and for one that sent nothing at all:
    /// there is no text to show in either case, and the parts that did go out
    /// are in [`files`](Self::files), by field, name and size. A trace is
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
    /// The `if:` condition that came back false, when that is why nothing was
    /// sent.
    ///
    /// Filed rather than passed over in silence. A hook that did not fire is a
    /// question somebody will ask sooner or later, and the condition quoted back
    /// is the whole answer; a trace that simply omitted it would make a declared
    /// hook and a hook that never ran look identical. Not an error — `error`
    /// stays empty and `on_error` does not apply. A condition that could not be
    /// *evaluated* is the other case, and it fills `error` instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped: Option<String>,
}

/// One attached file, as the trace describes it.
///
/// Everything except the bytes. The endpoint got those as a multipart part; a
/// reader of the trace wants to know which file went, not to scroll past it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Attachment {
    /// The form field it went out under, as `mcp.yaml` named it.
    ///
    /// A multipart with three fields makes three different statements to the
    /// endpoint, and a trace listing only file names would say which bytes went
    /// without saying what they were sent *as*.
    pub field: String,
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
/// The named variables every template of a hook sees, and what `{{ call }}`
/// renders to. `env`, `vars` and `uploads` sit beside it rather than in it — a
/// hook that shipped the whole process environment to a webhook because somebody
/// wrote `{{ call }}` would be a leak nobody asked for.
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
/// Shared by `url:`, `headers:`, `json:` and `multipart:`, so the four cannot
/// drift into four vocabularies for the same call.
#[derive(Debug, Serialize)]
struct Rendering<'a> {
    #[serde(flatten)]
    payload: &'a Payload<'a>,
    /// The same fields again, under one name.
    ///
    /// So that `json: "{{ call }}"` is the whole call in one line: the entire
    /// configuration of an audit sink, and the thing `mire` used to send whether
    /// anybody had asked for it or not.
    call: &'a Payload<'a>,
    /// The process environment, read fresh on every firing.
    ///
    /// Once per `tools/call` rather than once per template: a rotated token is
    /// picked up without a restart either way, and every template of a firing
    /// reading the same environment is the weaker promise worth keeping — an
    /// `if:` that said yes and a `url:` that then disagreed about `env.STAGE`
    /// would be a request nobody asked for.
    env: BTreeMap<String, String>,
    /// What the run's tool calls have captured so far.
    ///
    /// Here and not in the payload, like `env`: a hook that shipped a run's
    /// whole variable bag to a webhook by default is a decision nobody made.
    vars: &'a Captured,
    /// Every file the run is carrying, whole — `base64`, `dataUrl`, `text` and
    /// the rest, the same entries a model template gets from the same run.
    ///
    /// All of them, not a selection: `multipart:` is where a hook says which
    /// ones leave, and it says so by naming them.
    uploads: &'a [UploadRef],
}

/// Fires every hook that applies, in declaration order, and every action of
/// each in the order the file wrote them.
///
/// # Errors
///
/// The first failure of a hook whose `on_error` is `fail`. Nothing after it
/// runs — neither the hook's remaining actions nor the hooks behind it: a gate
/// that said no has said no, and the rest would be notifications about a call
/// that is not happening. `on_error: continue` records the failure and moves on
/// to the next action, which is the same rule applied one level down.
pub(super) async fn fire(
    http: &Client,
    hooks: &[Hook],
    payload: &Payload<'_>,
    uploads: &[UploadRef],
    vars: &Captured,
    credentials: &McpCredentials<'_>,
    file: impl Fn(HookRecord),
) -> Result<(), McpError> {
    // Most tool calls have no hook on them at all, and the context below reads
    // the whole process environment. Asked first, so the ordinary call pays for
    // nothing.
    if !hooks
        .iter()
        .any(|hook| hook.fires(payload.phase, payload.tool))
    {
        return Ok(());
    }

    // Built once, and shared by the condition and by every action behind it:
    // one firing is one set of facts about one call, and two actions of a hook
    // disagreeing about what `env` said would be a bug nobody could reproduce.
    let rendering = Rendering {
        payload,
        call: payload,
        env: std::env::vars().collect(),
        vars,
        uploads,
    };

    for hook in hooks {
        if !hook.fires(payload.phase, payload.tool) {
            continue;
        }

        // Asked by `if:`, and answered before anything is built: a hook whose
        // condition says no pays for no credential, renders no template, and
        // sends nothing.
        match qualifies(hook, payload, &rendering, &file) {
            Ok(true) => {}
            Ok(false) => continue,
            // The condition could not be asked at all, which is a broken file
            // rather than a call that did not qualify. Its records are already
            // filed; what is left is what `on_error` says it means.
            Err(message) => {
                if hook.on_error == OnError::Fail {
                    return Err(McpError::Hook {
                        server: payload.server.to_owned(),
                        hook: hook.name.clone(),
                        phase: payload.phase,
                        message,
                    });
                }
                continue;
            }
        }

        for (step, action) in hook.actions.iter().enumerate() {
            let firing = Firing { hook, action, step };
            let (mut record, outcome) = run(http, firing, &rendering, credentials).await;

            let Err(message) = outcome else {
                debug!(
                    server = %payload.server,
                    hook = %hook.name,
                    action = record.step,
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
                action = record.step,
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
    }

    Ok(())
}

/// Whether `if:` lets this hook fire, with the trail filed when it does not.
///
/// Both no-answers leave a record per action rather than a silence — see
/// [`HookRecord::skipped`] for why a non-event belongs in a trace, and once per
/// action because each names its own address. They are not the same answer,
/// though: a condition that came back false is the hook working as declared,
/// and one that could not be evaluated is a mistake in `mcp.yaml`.
///
/// # Errors
///
/// The condition's own failure, once its records are filed. Whether that stops
/// the tool call is `on_error`'s to say, and the caller's to act on.
fn qualifies(
    hook: &Hook,
    payload: &Payload<'_>,
    rendering: &Rendering<'_>,
    file: &impl Fn(HookRecord),
) -> Result<bool, String> {
    let Some(condition) = hook.condition.as_ref() else {
        return Ok(true);
    };

    match condition.holds(rendering) {
        Ok(true) => Ok(true),
        Ok(false) => {
            debug!(
                server = %payload.server,
                hook = %hook.name,
                phase = %payload.phase,
                tool = %payload.tool,
                condition = condition.source(),
                "hook skipped: its `if` condition does not hold for this call"
            );
            for (step, action) in hook.actions.iter().enumerate() {
                let mut record = unfired(hook, action, step, payload);
                record.skipped = Some(condition.source().to_owned());
                file(record);
            }
            Ok(false)
        }
        Err(message) => {
            let stopped = hook.on_error == OnError::Fail;
            warn!(
                server = %payload.server,
                hook = %hook.name,
                phase = %payload.phase,
                tool = %payload.tool,
                stopped_the_call = stopped,
                %message,
                "hook failed"
            );
            for (step, action) in hook.actions.iter().enumerate() {
                let mut record = unfired(hook, action, step, payload);
                record.error = Some(message.clone());
                record.stopped_the_call = stopped;
                file(record);
            }
            Err(message)
        }
    }
}

/// Which action of its hook this is, as a person counts.
fn step_of(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

/// The record for an action that never got as far as a request.
///
/// Both things `if:` can do to an action end here — the condition that came back
/// false, and the one that could not be evaluated at all. Everything a firing
/// would have said about *which* action this was, and nothing about a request:
/// there was none. `url` is what the file says rather than a rendered address,
/// because rendering it is exactly what did not happen — and on the hooks this
/// is written for, could not have.
///
/// The caller fills in which of the two it was: `skipped` for the answer that
/// was no, `error` and `stopped_the_call` for the question that broke.
fn unfired(hook: &Hook, action: &HookAction, step: usize, payload: &Payload<'_>) -> HookRecord {
    let HookAction::Http(http) = action;
    HookRecord {
        server: payload.server.to_owned(),
        hook: hook.name.clone(),
        step: step_of(step),
        phase: payload.phase,
        tool: payload.tool.to_owned(),
        action: action.kind().to_owned(),
        url: http.url.source().to_owned(),
        method: http.method.to_string(),
        headers: BTreeMap::new(),
        request: String::new(),
        files: Vec::new(),
        status: 0,
        response: String::new(),
        latency_ms: 0,
        error: None,
        stopped_the_call: false,
        skipped: None,
    }
}

/// Which action of which hook is going out, for the record it will produce.
///
/// Three fields that only ever travel together: a record has to name the hook,
/// the action within it, and where that action sits in the list.
struct Firing<'a> {
    hook: &'a Hook,
    action: &'a HookAction,
    step: usize,
}

/// One action of one hook, run to completion. Always produces a record.
async fn run(
    http: &Client,
    firing: Firing<'_>,
    rendering: &Rendering<'_>,
    credentials: &McpCredentials<'_>,
) -> (HookRecord, Result<(), String>) {
    let Firing { hook, action, step } = firing;
    let HookAction::Http(http_action) = action;
    let payload = rendering.payload;

    let mut record = HookRecord {
        server: payload.server.to_owned(),
        hook: hook.name.clone(),
        step: step_of(step),
        phase: payload.phase,
        tool: payload.tool.to_owned(),
        action: action.kind().to_owned(),
        // Replaced below by the address actually used. Until then it is what the
        // file says, so an action that dies rendering its URL still says which
        // one.
        url: http_action.url.source().to_owned(),
        method: http_action.method.to_string(),
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

    // Before the credential, and this order is not an accident: the provider
    // decides what to hand over by looking at where it is going, so the address
    // has to exist first. A template that renders to somewhere `allowed_hosts`
    // refuses is refused, which is the check doing its job.
    let url = match http_action.url.resolve(rendering) {
        Ok(url) => url,
        Err(message) => return (record, Err(message)),
    };
    record.url = url.to_string();

    // Resolved here rather than with the server's, and only now: a credential
    // costs an exchange and can fail, and neither belongs to an action that did
    // not fire. Against the *action's* URL, so `allowed_hosts` means what it
    // says.
    let resolved = match credentials.for_action(action, &url).await {
        Ok(resolved) => resolved,
        Err(error) => return (record, Err(error.to_string())),
    };

    let sent = match render_body(http_action, rendering) {
        Ok(sent) => sent,
        Err(message) => return (record, Err(message)),
    };

    let multipart = match &sent {
        Sent::Files(attached) => match form(attached) {
            Ok(form) => Some(form),
            Err(message) => return (record, Err(message)),
        },
        Sent::Nothing | Sent::Json(_) => None,
    };

    let (mut headers, scrub) = match authenticate(
        http_action,
        &url,
        &resolved,
        payload.server,
        rendering.vars,
        sent.is_json(),
    )
    .await
    {
        Ok(pair) => pair,
        Err(message) => return (record, Err(message)),
    };

    let shown = settle_content_type(&mut headers, multipart.as_ref());

    // Filled in only now: the auth provider has just added its secret to the
    // redactor, and a record taken a line earlier would carry it in the clear.
    record.headers = scrub.headers(&shown);
    match &sent {
        Sent::Nothing => {}
        Sent::Json(text) => record.request = scrub.text(text),
        Sent::Files(attached) => record.files = attached.iter().map(describe).collect(),
    }

    let request = http
        .request(http_action.method.clone(), url)
        .headers(headers)
        .timeout(http_action.timeout);
    let request = match (multipart, sent) {
        (Some(form), _) => request.multipart(form),
        (None, Sent::Json(text)) => request.body(text),
        // No `json:`, no `multipart:`, no body — and no `content-length` guess
        // on the endpoint's part about what the silence meant.
        (None, Sent::Nothing | Sent::Files(_)) => request,
    };

    let started = Instant::now();
    let sent = request.send().await;
    record.latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

    let response = match sent {
        Ok(response) => response,
        // The endpoint's own words, not `reqwest`'s: an action that failed at the
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

/// What one firing puts on the wire.
enum Sent<'a> {
    /// Nothing at all: the action declares neither `json:` nor `multipart:`.
    Nothing,
    /// A JSON document, serialised.
    Json(String),
    /// Files, each under the field that named it.
    Files(Vec<Attached<'a>>),
}

impl Sent<'_> {
    /// Whether `content-type: application/json` is the truth about this body.
    const fn is_json(&self) -> bool {
        matches!(self, Self::Json(_))
    }
}

/// One upload on its way out, under the field that named it.
struct Attached<'a> {
    field: String,
    upload: &'a UploadRef,
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

/// The body an action sends, rendered: whatever `json:` or `multipart:` asked
/// for, and nothing when it asked for neither.
fn render_body<'a>(action: &HttpAction, rendering: &Rendering<'a>) -> Result<Sent<'a>, String> {
    match &action.body {
        None => Ok(Sent::Nothing),
        Some(HookBody::Json(node)) => serde_json::to_string(&render_json(node, rendering)?)
            .map(Sent::Json)
            .map_err(|error| format!("the rendered body could not be serialised: {error}")),
        Some(HookBody::Multipart(parts)) => attach(parts, rendering).map(Sent::Files),
    }
}

/// A JSON body, rendered node by node.
///
/// Strings render; numbers, booleans, `null` and the shape of the document
/// itself go out as `mcp.yaml` wrote them. The document is the endpoint's
/// schema, written down — which is the whole reason this is a tree and not a
/// string somebody has to keep valid by hand.
fn render_json(node: &Value, rendering: &Rendering<'_>) -> Result<Value, String> {
    match node {
        Value::String(text) => render_value(text, rendering),
        Value::Array(items) => {
            let mut rendered = Vec::with_capacity(items.len());
            for item in items {
                rendered.push(render_json(item, rendering)?);
            }
            Ok(Value::Array(rendered))
        }
        Value::Object(fields) => {
            let mut rendered = serde_json::Map::with_capacity(fields.len());
            for (name, value) in fields {
                rendered.insert(name.clone(), render_json(value, rendering)?);
            }
            Ok(Value::Object(rendered))
        }
        other => Ok(other.clone()),
    }
}

/// One string of a JSON body, rendered with its type kept.
///
/// A string that is one `{{ … }}` and nothing else is *evaluated* rather than
/// rendered, so an expression naming an object stays an object. Anything with
/// text around it is a string, because that is what interpolation is for.
///
/// The rule earns its paragraph: without it, `"{{ arguments }}"` reaches the
/// endpoint as a quoted rendering of a map. That parses as JSON, survives
/// review, and is wrong — the field is a string where a schema promised an
/// object, and the endpoint is the one that finds out.
fn render_value(text: &str, rendering: &Rendering<'_>) -> Result<Value, String> {
    let Some(expression) = lone_expression(text) else {
        return ENVIRONMENT
            .render_str(text, rendering)
            .map(Value::String)
            .map_err(|error| {
                format!(
                    "the body could not be rendered: {}",
                    explain(&error, text, rendering)
                )
            });
    };

    serde_json::to_value(evaluate(expression, text, rendering)?)
        .map_err(|error| format!("the body could not be serialised: {error}"))
}

/// The expression a string is made of, when it is made of nothing else.
fn lone_expression(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let inner = trimmed.strip_prefix("{{")?.strip_suffix("}}")?;
    (!inner.contains("{{") && !inner.contains("}}")).then_some(inner)
}

/// One expression, evaluated to a value rather than rendered to text.
fn evaluate(
    expression: &str,
    template: &str,
    rendering: &Rendering<'_>,
) -> Result<Rendered, String> {
    let value = ENVIRONMENT
        .compile_expression(expression)
        .map_err(|error| {
            format!(
                "the template could not be compiled: {}",
                explain(&error, template, rendering)
            )
        })?
        .eval(rendering)
        .map_err(|error| {
            format!(
                "the template could not be rendered: {}",
                explain(&error, template, rendering)
            )
        })?;

    // Strict undefined behaviour catches this while *rendering* a template, and
    // says nothing while evaluating one: an expression naming something that is
    // not there evaluates happily to undefined, which serialises to `null`. A
    // field silently `null` is the webhook that looks like it works — the exact
    // failure the strict setting exists to prevent — so the same rule is applied
    // here by hand.
    if value.is_undefined() {
        return Err(format!(
            "the template could not be rendered: {}",
            name_the_missing("undefined value".to_owned(), template, rendering)
        ));
    }

    Ok(value)
}

/// The parts of a multipart body, resolved against what the run is carrying.
fn attach<'a>(parts: &[PartSpec], rendering: &Rendering<'a>) -> Result<Vec<Attached<'a>>, String> {
    let mut attached: Vec<Attached<'a>> = Vec::new();

    for part in parts {
        let before = attached.len();

        for source in &part.sources {
            let value =
                match lone_expression(source) {
                    Some(expression) => evaluate(expression, source, rendering)?,
                    None => Rendered::from(ENVIRONMENT.render_str(source, rendering).map_err(
                        |error| format!("`{}`: {}", part.field, explain(&error, source, rendering)),
                    )?),
                };

            for upload in resolve(&value, rendering.uploads, &part.field)? {
                attached.push(Attached {
                    field: part.field.clone(),
                    upload,
                });
            }
        }

        // A field that named nothing is the failure this whole shape exists to
        // make loud. A multipart missing the one part the endpoint asked for
        // goes out looking perfectly well-formed and comes back a `422` about a
        // field nobody in the file ever mentioned.
        if attached.len() == before {
            return Err(format!(
                "`{}` named no upload ({})",
                part.field,
                carrying(rendering.uploads)
            ));
        }
    }

    Ok(attached)
}

/// The uploads one rendered value names.
///
/// Three forms, because a template can sensibly produce three things: an upload
/// whole (`{{ uploads[0] }}`), a list of them (`{{ uploads }}`), or a string
/// naming one — its `path`, its `name` or its `id`. Anything else is an error
/// rather than a part quietly left out of the form.
fn resolve<'a>(
    value: &Rendered,
    uploads: &'a [UploadRef],
    field: &str,
) -> Result<Vec<&'a UploadRef>, String> {
    if let Some(text) = value.as_str() {
        return named(text, uploads, field).map(|upload| vec![upload]);
    }

    if value.kind() == ValueKind::Seq {
        let items = value
            .try_iter()
            .map_err(|error| format!("`{field}`: {}", root(&error)))?;
        let mut found = Vec::new();
        for item in items {
            found.extend(resolve(&item, uploads, field)?);
        }
        return Ok(found);
    }

    // An upload as the context carries it. Any of the three identifying fields
    // will do, and `path` is the one a file is most likely to have written.
    for attribute in ["path", "id", "name"] {
        if let Ok(inner) = value.get_attr(attribute)
            && let Some(text) = inner.as_str()
        {
            return named(text, uploads, field).map(|upload| vec![upload]);
        }
    }

    Err(format!(
        "`{field}`: that is not a file, and not the name of one"
    ))
}

/// The upload a path, name or id points at.
fn named<'a>(text: &str, uploads: &'a [UploadRef], field: &str) -> Result<&'a UploadRef, String> {
    uploads
        .iter()
        .find(|upload| upload.path == text || upload.name == text || upload.id == text)
        .ok_or_else(|| {
            format!(
                "`{field}`: `{text}` is not a file this run is carrying ({})",
                carrying(uploads)
            )
        })
}

/// What the run has to offer, for the error saying it had none of it.
fn carrying(uploads: &[UploadRef]) -> String {
    if uploads.is_empty() {
        return "nothing was attached to this run".to_owned();
    }
    let names: Vec<&str> = uploads.iter().map(|upload| upload.name.as_str()).collect();
    format!("it is carrying {}", names.join(", "))
}

/// One upload on its way out, as the trace describes it. Everything but bytes.
fn describe(attached: &Attached<'_>) -> Attachment {
    Attachment {
        field: attached.field.clone(),
        id: attached.upload.id.clone(),
        name: attached.upload.name.clone(),
        size: attached.upload.size,
        content_type: mime_of(attached.upload).to_owned(),
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

/// The multipart form: one part per file, under the field that named it.
fn form(attached: &[Attached<'_>]) -> Result<Form, String> {
    let mut form = Form::new();

    for one in attached {
        // Decoded rather than re-read off the disk: the run already holds the
        // file, and going back to the filesystem would be a second answer to a
        // question already answered — one that can differ if the file moved.
        let bytes = BASE64
            .decode(&one.upload.base64)
            .map_err(|error| format!("`{}` could not be decoded: {error}", one.upload.name))?;
        let part = Part::bytes(bytes)
            .file_name(one.upload.name.clone())
            .mime_str(mime_of(one.upload))
            .map_err(|error| format!("`{}` could not be attached: {error}", one.upload.name))?;
        form = form.part(one.field.clone(), part);
    }

    Ok(form)
}

/// The headers to send, and a redactor holding every secret among them.
async fn authenticate(
    action: &HttpAction,
    url: &Url,
    credentials: &HookCredentials<'_>,
    server: &str,
    vars: &Captured,
    json: bool,
) -> Result<(HeaderMap, Redactor), String> {
    let mut headers = HeaderMap::new();
    // Only for a `json:` body, and only as a starting point: a `content-type:`
    // of the action's own lands after this and wins. A multipart's type is
    // settled by the encoder, boundary and all, and nothing written here could
    // be right about it — and a request with no body has no type to declare.
    if json {
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
/// direct and is exactly the wrong thing: a `json:` field may hold a literal
/// credential, and an error message is a place secrets go to be logged forever.
fn explain(error: &minijinja::Error, template: &str, rendering: &Rendering<'_>) -> String {
    name_the_missing(root(error), template, rendering)
}

/// The same, for a failure `MiniJinja` did not report as an error.
///
/// Split out for [`evaluate`], which has to raise "undefined" itself: the naming
/// is the useful half, and it must read identically whichever side found the
/// problem.
fn name_the_missing(message: String, template: &str, rendering: &Rendering<'_>) -> String {
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

    fn action() -> HttpAction {
        HttpAction {
            url: HookUrl::Fixed("https://audit.internal/events".parse().expect("url")),
            method: Method::POST,
            auth: None,
            headers: HeaderTemplates::default(),
            body: None,
            timeout: Duration::from_secs(5),
        }
    }

    fn hook(name: &str, phases: &[HookPhase], tools: &[&str]) -> Hook {
        Hook {
            name: name.to_owned(),
            phases: phases.iter().copied().collect(),
            tools: tools
                .iter()
                .map(|tool| NamePattern::compile(tool).expect("pattern"))
                .collect(),
            condition: None,
            on_error: OnError::Fail,
            actions: vec![HookAction::Http(action())],
        }
    }

    /// A hook fired under `condition`.
    fn conditional(name: &str, condition: &str) -> Hook {
        Hook {
            condition: Some(HookCondition::compile(condition).expect("condition")),
            ..hook(name, &[HookPhase::After], &[])
        }
    }

    /// Whether `condition` holds for an `after` call carrying `vars`.
    fn holds(condition: &str, vars: &Captured) -> Result<bool, String> {
        let arguments = json!({ "path": "/etc/hosts", "size": 2048 });
        let result = ToolOutcome {
            text: "ok".to_owned(),
            structured: None,
            is_error: false,
            latency_ms: 3,
            error: None,
        };
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: Some(result),
        };
        let rendering = Rendering {
            payload: &payload,
            call: &payload,
            env: std::env::vars().collect(),
            vars,
            uploads: &[],
        };

        HookCondition::compile(condition)
            .expect("condition")
            .holds(&rendering)
    }

    /// An action sending the JSON document `body`.
    fn sending(body: Value) -> HttpAction {
        HttpAction {
            body: Some(HookBody::Json(body)),
            ..action()
        }
    }

    /// An action sending `field`, filled by `sources`.
    fn attaching(field: &str, sources: &[&str]) -> HttpAction {
        HttpAction {
            body: Some(HookBody::Multipart(vec![PartSpec {
                field: field.to_owned(),
                sources: sources.iter().map(|s| (*s).to_owned()).collect(),
            }])),
            ..action()
        }
    }

    /// What one firing put on the wire, in terms a test can assert on.
    #[derive(Debug)]
    struct Fired {
        /// The JSON body, or empty when there was none.
        body: String,
        /// The parts, as `field:name`, in the order they go out.
        parts: Vec<String>,
        /// Where it was all going.
        url: Url,
    }

    /// One action, rendered against a call — body and URL together.
    ///
    /// Both at once because they render from one context, and a test that could
    /// only see one of them could not catch the two drifting apart.
    fn fire_once(
        action: &HttpAction,
        payload: &Payload<'_>,
        uploads: &[UploadRef],
        vars: &Captured,
    ) -> Result<Fired, (String, Option<Url>)> {
        let rendering = Rendering {
            payload,
            call: payload,
            env: std::env::vars().collect(),
            vars,
            uploads,
        };
        let url = action
            .url
            .resolve(&rendering)
            .map_err(|message| (message, None))?;
        let sent =
            render_body(action, &rendering).map_err(|message| (message, Some(url.clone())))?;

        let (body, parts) = match &sent {
            Sent::Nothing => (String::new(), Vec::new()),
            Sent::Json(text) => (text.clone(), Vec::new()),
            Sent::Files(attached) => (
                String::new(),
                attached
                    .iter()
                    .map(|one| format!("{}:{}", one.field, one.upload.name))
                    .collect(),
            ),
        };

        Ok(Fired { body, parts, url })
    }

    /// The body one action would send, with nothing attached and nothing
    /// captured.
    fn rendered(action: &HttpAction, payload: &Payload<'_>) -> Result<String, String> {
        fire_once(action, payload, &[], &Captured::new())
            .map(|fired| fired.body)
            .map_err(|(message, _)| message)
    }

    /// The same, parsed, for a test that reads fields rather than text.
    fn document(action: &HttpAction, payload: &Payload<'_>) -> Value {
        serde_json::from_str(&rendered(action, payload).expect("body")).expect("json")
    }

    /// A `before` call, which is the shape most of these need.
    fn calling<'a>(tool: &'a str, arguments: &'a Value) -> Payload<'a> {
        Payload {
            phase: HookPhase::Before,
            server: "files",
            tool,
            arguments,
            result: None,
        }
    }

    /// One upload, as a run would have it: bytes already read and encoded.
    fn upload(name: &str, content_type: Option<&str>, bytes: &[u8]) -> UploadRef {
        UploadRef {
            id: format!("id-{name}"),
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
    fn an_action_with_neither_json_nor_multipart_sends_no_body_at_all() {
        // The whole reason this shape exists. A payload nobody wrote is a
        // request an endpoint never agreed to read, and it used to go out on
        // every hook that had not been told otherwise.
        let arguments = json!({"path": "/etc/passwd"});

        assert_eq!(
            rendered(&action(), &calling("read_file", &arguments)).expect("body"),
            ""
        );
    }

    #[test]
    fn the_call_itself_is_one_variable_away() {
        // And the audit sink that did want it says so in one line.
        let arguments = json!({"path": "/etc/passwd"});
        let action = sending(json!("{{ call }}"));

        let body = document(&action, &calling("read_file", &arguments));

        assert_eq!(body["phase"], "before");
        assert_eq!(body["server"], "files");
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

        let body = document(&sending(json!("{{ call }}")), &payload);

        assert_eq!(body["result"]["isError"], true);
        assert_eq!(body["result"]["latencyMs"], 12);
    }

    #[test]
    fn a_json_body_sees_the_call_by_name() {
        let arguments = json!({"path": "/tmp/x"});
        let action = sending(json!({
            "ran": "{{ tool }}",
            "on": "{{ server }}",
            "with": "{{ arguments.path }}",
        }));

        let body = document(&action, &calling("write_file", &arguments));

        assert_eq!(body["ran"], "write_file");
        assert_eq!(body["on"], "files");
        assert_eq!(body["with"], "/tmp/x");
    }

    #[test]
    fn an_expression_on_its_own_keeps_the_type_it_names() {
        // The rule the whole shape turns on. `"{{ arguments }}"` rendered as text
        // reaches the endpoint as a quoted rendering of a map: valid JSON, wrong
        // type, and the endpoint is the one that finds out.
        let arguments = json!({"path": "/tmp/x", "retries": 2});
        let vars = Captured::from([("attempt".to_owned(), json!(3))]);
        let action = sending(json!({
            "arguments": "{{ arguments }}",
            "attempt": "{{ vars.attempt }}",
            "retries": "{{ arguments.retries }}",
        }));

        let fired =
            fire_once(&action, &calling("write_file", &arguments), &[], &vars).expect("render");
        let body: Value = serde_json::from_str(&fired.body).expect("json");

        assert_eq!(body["arguments"], json!({"path": "/tmp/x", "retries": 2}));
        assert_eq!(body["attempt"], json!(3));
        assert_eq!(body["retries"], json!(2));
    }

    #[test]
    fn text_around_an_expression_makes_it_a_string_again() {
        // Which is what interpolation is for, and the only unambiguous line to
        // draw: one expression is a value, an expression in a sentence is a
        // sentence.
        let arguments = json!({"retries": 2});
        let action = sending(json!({"note": "tried {{ arguments.retries }} times"}));

        let body = document(&action, &calling("write_file", &arguments));

        assert_eq!(body["note"], "tried 2 times");
    }

    #[test]
    fn the_document_goes_out_in_the_shape_the_file_wrote_it() {
        // Numbers stay numbers, booleans stay booleans, and nesting survives:
        // the document is the endpoint's schema written down.
        let arguments = json!({});
        let action = sending(json!({
            "tags": ["audit", "{{ server }}"],
            "nested": {"deep": {"tool": "{{ tool }}"}},
            "count": 3,
            "enabled": true,
            "nothing": null,
        }));

        let body = document(&action, &calling("read_file", &arguments));

        assert_eq!(body["tags"], json!(["audit", "files"]));
        assert_eq!(body["nested"]["deep"]["tool"], "read_file");
        assert_eq!(body["count"], json!(3));
        assert_eq!(body["enabled"], json!(true));
        assert!(body["nothing"].is_null());
    }

    #[test]
    fn a_json_body_that_reads_something_undefined_is_an_error() {
        let arguments = json!({});
        // A payload silently missing the field it was meant to carry is a webhook
        // that looks like it works.
        let action = sending(json!({"who": "{{ env.MIRE_DEFINITELY_NOT_SET }}"}));

        let message = rendered(&action, &calling("write_file", &arguments)).expect_err("undefined");

        assert!(message.contains("undefined"), "{message}");
    }

    #[test]
    fn a_json_body_reads_what_the_run_captured() {
        let arguments = json!({});
        let vars = Captured::from([("session".to_owned(), json!("abc-123"))]);
        let action = sending(json!({"session": "{{ vars.session }}"}));

        let fired =
            fire_once(&action, &calling("read_file", &arguments), &[], &vars).expect("render");

        assert_eq!(fired.body, r#"{"session":"abc-123"}"#);
    }

    #[test]
    fn the_call_carries_neither_the_environment_nor_the_variables() {
        // `vars` and `env` are for a template that asked for them. A hook
        // shipping a run's whole variable bag to a third party because somebody
        // wrote `{{ call }}` is a decision nobody made.
        let arguments = json!({});
        let vars = Captured::from([("session".to_owned(), json!("abc-123"))]);

        let fired = fire_once(
            &sending(json!("{{ call }}")),
            &calling("read_file", &arguments),
            &[],
            &vars,
        )
        .expect("render");

        assert!(!fired.body.contains("abc-123"), "{}", fired.body);
        assert!(!fired.body.contains("vars"), "{}", fired.body);
        assert!(!fired.body.contains("env"), "{}", fired.body);
    }

    #[test]
    fn a_multipart_field_sends_the_upload_it_names() {
        let uploads = vec![upload("report.pdf", Some("application/pdf"), b"%PDF-1.7")];
        let arguments = json!({});
        let action = attaching("file", &["{{ uploads[0].path }}"]);

        let fired = fire_once(
            &action,
            &calling("run_task", &arguments),
            &uploads,
            &Captured::new(),
        )
        .expect("render");

        assert_eq!(fired.parts, vec!["file:report.pdf"]);
        // A multipart is not a JSON body with files stapled to it: there is no
        // payload part, because nobody asked for one.
        assert!(fired.body.is_empty());
    }

    #[test]
    fn an_upload_answers_to_its_path_its_name_or_its_id() {
        let uploads = vec![upload("report.pdf", Some("application/pdf"), b"%PDF")];
        let arguments = json!({});

        for source in [
            "/uploads/aB3dE5gH7jK9-report.pdf",
            "report.pdf",
            "id-report.pdf",
        ] {
            let fired = fire_once(
                &attaching("file", &[source]),
                &calling("run_task", &arguments),
                &uploads,
                &Captured::new(),
            )
            .expect("render");
            assert_eq!(fired.parts, vec!["file:report.pdf"], "{source}");
        }
    }

    #[test]
    fn one_field_can_carry_several_files() {
        // Several parts under one name, which is what every upload handler on
        // the other side already reads.
        let uploads = vec![
            upload("a.txt", Some("text/plain"), b"one"),
            upload("b.txt", Some("text/plain"), b"two"),
        ];
        let arguments = json!({});
        let action = attaching("file", &["{{ uploads[0].path }}", "{{ uploads[1].path }}"]);

        let fired = fire_once(
            &action,
            &calling("run_task", &arguments),
            &uploads,
            &Captured::new(),
        )
        .expect("render");

        assert_eq!(fired.parts, vec!["file:a.txt", "file:b.txt"]);
    }

    #[test]
    fn an_expression_naming_every_upload_sends_every_upload() {
        // The list falls out of the typing rule rather than out of a second
        // syntax: `{{ uploads }}` is a list, so it is several parts.
        let uploads = vec![
            upload("a.txt", Some("text/plain"), b"one"),
            upload("b.txt", Some("text/plain"), b"two"),
        ];
        let arguments = json!({});

        let fired = fire_once(
            &attaching("file", &["{{ uploads }}"]),
            &calling("run_task", &arguments),
            &uploads,
            &Captured::new(),
        )
        .expect("render");

        assert_eq!(fired.parts, vec!["file:a.txt", "file:b.txt"]);
    }

    #[test]
    fn a_field_can_be_filled_from_what_an_earlier_call_captured() {
        let uploads = vec![upload("input.csv", Some("text/csv"), b"a,b")];
        let arguments = json!({});
        let vars = Captured::from([("input".to_owned(), json!("input.csv"))]);

        let fired = fire_once(
            &attaching("file", &["{{ vars.input }}"]),
            &calling("run_task", &arguments),
            &uploads,
            &vars,
        )
        .expect("render");

        assert_eq!(fired.parts, vec!["file:input.csv"]);
    }

    #[test]
    fn several_fields_each_carry_their_own_file() {
        let uploads = vec![
            upload("input.csv", Some("text/csv"), b"a,b"),
            upload("baseline.csv", Some("text/csv"), b"c,d"),
        ];
        let arguments = json!({});
        let action = HttpAction {
            body: Some(HookBody::Multipart(vec![
                PartSpec {
                    field: "input".to_owned(),
                    sources: vec!["input.csv".to_owned()],
                },
                PartSpec {
                    field: "reference".to_owned(),
                    sources: vec!["baseline.csv".to_owned()],
                },
            ])),
            ..action()
        };

        let fired = fire_once(
            &action,
            &calling("run_task", &arguments),
            &uploads,
            &Captured::new(),
        )
        .expect("render");

        assert_eq!(
            fired.parts,
            vec!["input:input.csv", "reference:baseline.csv"]
        );
    }

    #[test]
    fn a_field_naming_a_file_the_run_is_not_carrying_fails_the_hook() {
        // Loudly, and saying what it looked for: a part quietly left out is a
        // `422` about a field nobody in the file ever mentioned.
        let uploads = vec![upload("report.pdf", Some("application/pdf"), b"%PDF")];
        let arguments = json!({});

        let (message, _) = fire_once(
            &attaching("file", &["raport.pdf"]),
            &calling("run_task", &arguments),
            &uploads,
            &Captured::new(),
        )
        .expect_err("no such upload");

        assert!(message.contains("raport.pdf"), "{message}");
        assert!(message.contains("file"), "{message}");
        // And what there was instead, because the typo is usually visible from
        // the two names side by side.
        assert!(message.contains("report.pdf"), "{message}");
    }

    #[test]
    fn a_field_that_names_nothing_at_all_fails_rather_than_going_out_empty() {
        // `{{ uploads }}` on a run where nobody attached anything. Sending a
        // multipart with no file in it is how the endpoint ends up explaining
        // our own config to us, in the form of a `422`.
        let arguments = json!({});

        let (message, _) = fire_once(
            &attaching("file", &["{{ uploads }}"]),
            &calling("run_task", &arguments),
            &[],
            &Captured::new(),
        )
        .expect_err("nothing attached");

        assert!(message.contains("file"), "{message}");
        assert!(message.contains("nothing was attached"), "{message}");
    }

    #[test]
    fn a_field_naming_something_that_is_not_a_file_says_so() {
        let arguments = json!({});
        let vars = Captured::from([("attempt".to_owned(), json!(3))]);

        let (message, _) = fire_once(
            &attaching("file", &["{{ vars.attempt }}"]),
            &calling("run_task", &arguments),
            &[],
            &vars,
        )
        .expect_err("not a file");

        assert!(message.contains("file"), "{message}");
    }

    #[test]
    fn a_json_body_can_reach_the_bytes_when_it_asks_for_them() {
        let uploads = vec![upload("notes.txt", Some("text/plain"), b"ping")];
        let arguments = json!({});
        let action =
            sending(json!({"name": "{{ uploads[0].name }}", "text": "{{ uploads[0].text }}"}));

        let fired = fire_once(
            &action,
            &calling("write_file", &arguments),
            &uploads,
            &Captured::new(),
        )
        .expect("render");
        let body: Value = serde_json::from_str(&fired.body).expect("json");

        assert_eq!(body["name"], "notes.txt");
        assert_eq!(body["text"], "ping");
    }

    #[test]
    fn a_url_template_is_rendered_from_the_same_variables() {
        let arguments = json!({});
        let action = HttpAction {
            url: HookUrl::Template("https://audit.internal/sessions/{{ vars.session }}".to_owned()),
            ..action()
        };
        let vars = Captured::from([("session".to_owned(), json!("abc-123"))]);

        let fired =
            fire_once(&action, &calling("read_file", &arguments), &[], &vars).expect("render");

        assert_eq!(
            fired.url.as_str(),
            "https://audit.internal/sessions/abc-123"
        );
    }

    #[test]
    fn a_url_template_also_sees_the_call_it_fired_around() {
        // One context for `url:`, `json:` and `multipart:`, so anything the
        // payload carries is addressable — not a second, smaller vocabulary to
        // look up.
        let arguments = json!({});
        let action = HttpAction {
            url: HookUrl::Template("https://audit.internal/{{ server }}/{{ tool }}".to_owned()),
            ..action()
        };

        let fired = fire_once(
            &action,
            &calling("write_file", &arguments),
            &[],
            &Captured::new(),
        )
        .expect("render");

        assert_eq!(
            fired.url.as_str(),
            "https://audit.internal/files/write_file"
        );
    }

    #[test]
    fn a_url_template_reading_a_variable_nobody_captured_is_an_error() {
        let arguments = json!({});
        let action = HttpAction {
            url: HookUrl::Template("https://audit.internal/sessions/{{ vars.session }}".to_owned()),
            ..action()
        };

        // Rendering it loosely would send the request to `/sessions/`, which is a
        // different endpoint that may well answer `200`.
        let (message, _) = fire_once(
            &action,
            &calling("read_file", &arguments),
            &[],
            &Captured::new(),
        )
        .expect_err("undefined");

        assert!(message.contains("undefined"), "{message}");
        assert!(message.contains("url"), "{message}");
    }

    #[test]
    fn a_url_template_that_renders_to_something_that_is_not_a_url_says_what_it_produced() {
        let arguments = json!({});
        let action = HttpAction {
            url: HookUrl::Template("{{ vars.wherever }}".to_owned()),
            ..action()
        };
        let vars = Captured::from([("wherever".to_owned(), json!("not a url"))]);

        let (message, _) = fire_once(&action, &calling("read_file", &arguments), &[], &vars)
            .expect_err("not a url");

        // The rendered text, because that is the part nobody can see by reading
        // the file.
        assert!(message.contains("not a url"), "{message}");
    }

    #[test]
    fn a_fixed_url_is_used_as_written_whatever_the_run_captured() {
        let arguments = json!({});
        let vars = Captured::from([("session".to_owned(), json!("abc-123"))]);

        let fired =
            fire_once(&action(), &calling("read_file", &arguments), &[], &vars).expect("render");

        assert_eq!(fired.url.as_str(), "https://audit.internal/events");
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

        assert!(hook.condition.is_none());
    }

    #[test]
    fn a_condition_reads_what_the_run_has_captured() {
        let empty = Captured::new();
        assert_eq!(holds("{{ vars.session is defined }}", &empty), Ok(false));

        let captured = Captured::from([("session".to_owned(), json!("abc-123"))]);
        assert_eq!(holds("{{ vars.session is defined }}", &captured), Ok(true));
    }

    #[test]
    fn an_absent_variable_is_false_rather_than_a_failure() {
        // The one place in a hook where undefined is not an error. Everywhere
        // else a template names something absent, a request would go out with a
        // hole in it; here the question *is* whether it is there.
        let empty = Captured::new();

        assert_eq!(holds("{{ vars.session }}", &empty), Ok(false));
        // And it chains, so asking about a field of something absent is still a
        // question about the something, not a failure about the field.
        assert_eq!(holds("{{ vars.job.id }}", &empty), Ok(false));
    }

    #[test]
    fn a_condition_can_ask_about_more_than_presence() {
        let captured = Captured::from([
            ("session".to_owned(), json!("abc-123")),
            ("attempts".to_owned(), json!(3)),
        ]);

        // The whole reason this is an expression: a list of names could only ever
        // have asked the first of these.
        assert_eq!(holds("{{ vars.attempts > 2 }}", &captured), Ok(true));
        assert_eq!(holds("{{ vars.attempts > 5 }}", &captured), Ok(false));
        assert_eq!(
            holds(
                "{{ vars.session is defined and vars.attempts > 2 }}",
                &captured
            ),
            Ok(true)
        );
    }

    #[test]
    fn a_condition_sees_the_call_it_is_asked_about() {
        let empty = Captured::new();

        // Same context as `url:`, `json:` and `headers:` — one vocabulary.
        assert_eq!(holds("{{ tool == 'read_file' }}", &empty), Ok(true));
        assert_eq!(holds("{{ phase == 'after' }}", &empty), Ok(true));
        assert_eq!(holds("{{ arguments.size > 1024 }}", &empty), Ok(true));
        // Which is what makes "audit the calls that actually worked" writable.
        assert_eq!(holds("{{ not result.is_error }}", &empty), Ok(true));
    }

    #[test]
    fn a_variable_captured_as_null_does_not_hold_but_is_still_defined() {
        // A departure from `when_defined:`, which could only ask about presence.
        // An expression is read as a question about a value, and `null` is not
        // much of a value — so say which question you meant.
        let captured = Captured::from([("session".to_owned(), Value::Null)]);

        assert_eq!(holds("{{ vars.session }}", &captured), Ok(false));
        assert_eq!(holds("{{ vars.session is defined }}", &captured), Ok(true));
    }

    #[test]
    fn a_condition_that_cannot_be_evaluated_is_a_failure_rather_than_a_skip() {
        // Undefined being false here does not make every mistake silent: an
        // unknown filter is a broken file, not a call that did not qualify.
        let error = holds("{{ vars.session | teleport }}", &Captured::new())
            .expect_err("an unknown filter cannot be evaluated");

        assert!(error.contains("`if`"), "{error}");
        assert!(error.contains("teleport"), "{error}");
    }

    #[test]
    fn a_condition_is_decided_apart_from_the_phase_and_the_tool() {
        // Two questions, two places: `fires` is what the file says, and `if:` is
        // what this call and this run amount to. A hook can be right about the
        // tool and still have nothing to address itself with.
        let hook = conditional("audit", "{{ vars.session is defined }}");

        assert!(hook.fires(HookPhase::After, "read_file"));
        assert_eq!(
            holds("{{ vars.session is defined }}", &Captured::new()),
            Ok(false)
        );
    }

    #[test]
    fn a_condition_may_be_written_bare_or_wrapped() {
        let captured = Captured::from([("session".to_owned(), json!("abc-123"))]);

        assert_eq!(holds("vars.session is defined", &captured), Ok(true));
        assert_eq!(holds("{{ vars.session is defined }}", &captured), Ok(true));
    }

    #[test]
    fn a_condition_keeps_the_spelling_it_was_written_with() {
        // Because that is what the trace quotes back when the answer is no, and
        // a reader matching it against `mcp.yaml` should find it verbatim.
        let condition = HookCondition::compile("  {{ vars.session is defined }}  ").expect("valid");

        assert_eq!(condition.source(), "{{ vars.session is defined }}");
    }

    #[test]
    fn a_template_is_not_a_condition() {
        // It renders to `yes` or to the empty string, and only one of those is
        // falsy by accident. Refused, with the reason.
        let error = HookCondition::compile("{% if vars.session %}yes{% endif %}")
            .expect_err("a block is not an expression");
        assert!(error.contains("expression"), "{error}");

        // Same for an expression with text around it: `session: {{ vars.x }}` is
        // a non-empty string whatever `x` turns out to be.
        let error = HookCondition::compile("session: {{ vars.session }}")
            .expect_err("interpolation is not an expression");
        assert!(error.contains("expression"), "{error}");

        // And an empty one says what to do instead.
        let error = HookCondition::compile("   ").expect_err("nothing is not a condition");
        assert!(error.contains("empty"), "{error}");
    }

    /// Runs `hook`'s condition against an empty run, collecting what it filed.
    fn gate(hook: &Hook) -> (Result<bool, String>, Vec<HookRecord>) {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let vars = Captured::new();
        let rendering = Rendering {
            payload: &payload,
            call: &payload,
            env: std::env::vars().collect(),
            vars: &vars,
            uploads: &[],
        };

        let filed = Mutex::new(Vec::new());
        let outcome = qualifies(hook, &payload, &rendering, &|record| {
            filed.lock().expect("journal").push(record);
        });

        (outcome, filed.into_inner().expect("journal"))
    }

    #[test]
    fn a_condition_that_does_not_hold_files_one_record_per_action_and_no_more() {
        let mut hook = conditional("audit", "{{ vars.session is defined }}");
        // Two actions, because a hook that sits a call out sits out all of it,
        // and each of its addresses is its own line in the trace.
        hook.actions.push(HookAction::Http(action()));

        let (outcome, filed) = gate(&hook);

        assert_eq!(outcome, Ok(false));
        assert_eq!(filed.len(), 2);
        assert_eq!(filed[0].step, 1);
        assert_eq!(filed[1].step, 2);
        for record in &filed {
            assert_eq!(
                record.skipped.as_deref(),
                Some("{{ vars.session is defined }}")
            );
            assert!(record.error.is_none());
            assert!(!record.stopped_the_call);
        }
    }

    #[test]
    fn a_condition_that_holds_files_nothing_and_lets_the_hook_through() {
        let hook = conditional("audit", "{{ tool == 'read_file' }}");

        let (outcome, filed) = gate(&hook);

        assert_eq!(outcome, Ok(true));
        assert!(filed.is_empty(), "{filed:#?}");
    }

    #[test]
    fn a_hook_with_no_condition_never_asks_anything() {
        let hook = hook("audit", &[HookPhase::After], &[]);

        let (outcome, filed) = gate(&hook);

        assert_eq!(outcome, Ok(true));
        assert!(filed.is_empty(), "{filed:#?}");
    }

    #[test]
    fn a_broken_condition_is_a_failure_the_on_error_setting_decides_about() {
        let hook = conditional("audit", "{{ vars.session | teleport }}");

        let (outcome, filed) = gate(&hook);

        // Filed as a failure rather than as a skip: nothing was sent either way,
        // but only one of the two is the hook working as declared.
        assert!(outcome.is_err(), "{outcome:?}");
        assert_eq!(filed.len(), 1);
        assert!(filed[0].skipped.is_none());
        assert!(filed[0].error.is_some());
        assert!(filed[0].stopped_the_call);

        // And `on_error: continue` is the same record with the call left alone.
        let lenient = Hook {
            on_error: OnError::Continue,
            ..conditional("audit", "{{ vars.session | teleport }}")
        };
        let (outcome, filed) = gate(&lenient);

        assert!(outcome.is_err(), "{outcome:?}");
        assert!(filed[0].error.is_some());
        assert!(!filed[0].stopped_the_call);
    }

    #[test]
    fn an_unfired_hook_is_recorded_as_a_non_event_rather_than_a_failure() {
        let arguments = json!({});
        let payload = Payload {
            phase: HookPhase::After,
            server: "files",
            tool: "read_file",
            arguments: &arguments,
            result: None,
        };
        let hook = conditional("audit", "{{ vars.session is defined }}");

        let mut record = unfired(&hook, &hook.actions[0], 0, &payload);
        record.skipped = hook.condition.as_ref().map(|c| c.source().to_owned());

        assert_eq!(
            record.skipped.as_deref(),
            Some("{{ vars.session is defined }}")
        );
        // Not a failure, and nothing went out.
        assert!(record.error.is_none());
        assert!(!record.stopped_the_call);
        assert_eq!(record.status, 0);
        assert!(record.request.is_empty());
        // Enough to know which hook, and which of its actions, this was.
        assert_eq!(record.hook, "audit");
        assert_eq!(record.step, 1);
        assert_eq!(record.tool, "read_file");
        assert_eq!(record.url, "https://audit.internal/events");
    }

    #[test]
    fn actions_are_counted_the_way_a_person_counts_them() {
        assert_eq!(step_of(0), 1);
        assert_eq!(step_of(2), 3);
    }

    #[test]
    fn an_undefined_variable_is_named_rather_than_left_as_undefined_value() {
        let arguments = json!({});
        let action = HttpAction {
            url: HookUrl::Template(
                "https://audit.internal/{{ vars.session }}/{{ vars.job }}".to_owned(),
            ),
            ..action()
        };

        let (message, _) = fire_once(
            &action,
            &calling("read_file", &arguments),
            &[],
            &Captured::new(),
        )
        .expect_err("undefined");

        // "undefined value" on a URL reading two variables leaves you guessing.
        assert!(message.contains("session"), "{message}");
        assert!(message.contains("job"), "{message}");
    }

    #[test]
    fn a_json_body_names_what_it_could_not_read_either() {
        let arguments = json!({});
        let action = sending(json!({"session": "{{ vars.session }}"}));

        let message = rendered(&action, &calling("read_file", &arguments)).expect_err("undefined");

        assert!(message.contains("session"), "{message}");
    }

    #[test]
    fn an_error_names_the_variable_and_never_the_template() {
        // A `json:` field may hold a literal credential. An error message is
        // exactly where one must not end up — the same rule the headers follow.
        let arguments = json!({});
        let action = sending(json!({"key": "hunter2-{{ vars.suffix }}"}));

        let message = rendered(&action, &calling("read_file", &arguments)).expect_err("undefined");

        assert!(message.contains("suffix"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }

    #[test]
    fn an_upload_with_no_guessable_type_still_goes_out_as_something() {
        // `application/octet-stream` is what a part with no better idea is
        // supposed to say, and a part with no type at all is one an endpoint has
        // to guess about.
        let unknown = upload("blob.whatever", None, b"\x00\x01");
        let attached = Attached {
            field: "file".to_owned(),
            upload: &unknown,
        };

        assert_eq!(mime_of(&unknown), "application/octet-stream");
        assert_eq!(describe(&attached).content_type, "application/octet-stream");
        // And the trace says what it was sent *as*, not only which file it was.
        assert_eq!(describe(&attached).field, "file");
    }

    #[test]
    fn a_form_carries_one_part_per_file() {
        let files = [
            upload("a.txt", Some("text/plain"), b"one"),
            upload("b.png", Some("image/png"), &[0x89, b'P']),
        ];
        let attached: Vec<Attached<'_>> = files
            .iter()
            .map(|upload| Attached {
                field: "file".to_owned(),
                upload,
            })
            .collect();

        let built = form(&attached).expect("form");
        // `Form` does not hand its parts back, so the boundary is what there is
        // to assert on: it exists, which means both files encoded without
        // anybody complaining about a media type.
        assert!(!built.boundary().is_empty());
    }

    #[test]
    fn a_lone_expression_is_the_whole_string_or_it_is_not_one() {
        assert_eq!(lone_expression("{{ call }}"), Some(" call "));
        assert_eq!(lone_expression("  {{ call }}  "), Some(" call "));
        assert_eq!(lone_expression("a {{ call }}"), None);
        assert_eq!(lone_expression("{{ a }} {{ b }}"), None);
        assert_eq!(lone_expression("plain"), None);
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
