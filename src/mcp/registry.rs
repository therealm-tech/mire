//! `mcp.yaml`, sitting next to the profiles and `auth.yaml`.
//!
//! A server is declared once and referenced by name, for the same reason auth is:
//! the same server is worth pointing several profiles at, and worth replaying
//! across auth modes without duplicating anything.
//!
//! Same loading policy as everywhere else — a bad entry is reported and skipped,
//! the rest still work, and a server declared in two layered directories is the
//! later one's, with the one it displaced named in the log.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::Duration;

use reqwest::{Client, Method};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use url::Url;
use validator::{Validate, ValidationErrors};

use super::capture::CaptureRule;
use super::client::{McpClient, McpServer};
use super::headers::HeaderTemplates;
use super::hook::{
    Hook, HookAction, HookBody, HookCondition, HookPhase, HookUrl, HttpAction, NamePattern,
    OnError, PartSpec,
};
use crate::issue::LoadIssue;

/// File declaring the MCP servers, in the profiles directory.
pub const MCP_REGISTRY_FILE: &str = "mcp.yaml";

/// Default per-request timeout. Generous: a real tool does real work.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Default per-hook timeout. Shorter than a tool's: a hook is overhead on
/// somebody else's call, and a slow audit sink must not look like a slow tool.
const DEFAULT_HOOK_TIMEOUT_MS: u64 = 10_000;

/// A server as advertised to the UI. Carries no credential.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpDescriptor {
    /// Registry name, referenced from a profile's `mcp:` list.
    pub name: String,
    /// The endpoint, so the UI can show what it is about to talk to.
    pub url: String,
    /// Auth provider it authenticates with, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// Tools it is restricted to. Empty means everything it advertises.
    pub tools: Vec<String>,
    /// Names of the extra headers it sends. **Names only** — the values are
    /// rendered per request and are usually credentials.
    pub headers: Vec<String>,
    /// Auth providers its header templates read, beyond `auth:`.
    ///
    /// A server whose credential comes from a template shows no `auth:`, so
    /// without this `GET /api/mcp` — the place you look to answer "what is this
    /// server about to do" — would not mention that it authenticates at all, nor
    /// explain a `not_signed_in` coming back from it.
    pub uses_auth: Vec<String>,
    /// What fires around a tool call on this server.
    ///
    /// Listed for the same reason the server is: "what is this about to do" has
    /// to include the third party it is about to tell.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookDescriptor>,
    /// What it keeps out of its tool results, and from which tools.
    ///
    /// **Names only** — a captured value is a session id as often as not, and
    /// this listing is read before a run rather than during one, so there is
    /// nothing to show anyway. Listed because a hook's templated URL above is
    /// unreadable without knowing where `vars.session` is supposed to come from.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub capture: Vec<CaptureDescriptor>,
}

/// One capture rule, as advertised to the UI. Carries no captured value.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaptureDescriptor {
    /// Tools it applies to, as the patterns `mcp.yaml` wrote. Empty means every
    /// tool.
    pub tools: Vec<String>,
    /// The variables it fills, by name.
    pub vars: Vec<String>,
}

/// A hook as advertised to the UI. Carries no credential.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HookDescriptor {
    /// The hook's name, unique within its server.
    pub name: String,
    /// Phases it fires on.
    pub on: Vec<HookPhase>,
    /// Tools it applies to, as the patterns `mcp.yaml` wrote. Empty means every
    /// tool.
    pub tools: Vec<String>,
    /// The `if:` condition it is fired under, as `mcp.yaml` wrote it. Absent
    /// means it fires on every call it covers.
    #[serde(rename = "if", skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// What its failure does to the call.
    pub on_error: OnError,
    /// What it does when it fires, in order.
    pub actions: Vec<ActionDescriptor>,
}

/// One action of a hook, as advertised to the UI.
///
/// Says what it sends as well as where, because "what is this about to do" is
/// not answered by an address: a hook that posts a JSON document somebody wrote
/// and a hook that ships a run's files to a third party are different events,
/// and the difference is the half worth reading before a run rather than after.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionDescriptor {
    /// The kind: `http`.
    pub kind: String,
    /// Where it goes, as written — template and all.
    pub url: String,
    /// The method it goes out as.
    pub method: String,
    /// Auth provider it authenticates with, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// Names of the extra headers it sends. **Names only** — the values are
    /// rendered per request and are usually credentials.
    pub headers: Vec<String>,
    /// What its request carries: `json`, `multipart`, or `nothing`.
    pub sends: String,
    /// The multipart fields it fills, when that is what it sends. Naming them
    /// is the point: a run's files leaving for a third address is exactly what
    /// "what is this about to do" has to cover.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    /// Auth providers its header templates read, beyond `auth:`.
    pub uses_auth: Vec<String>,
}

/// Every declared MCP server, keyed by name.
#[derive(Debug, Default)]
pub struct McpRegistry {
    clients: BTreeMap<String, McpClient>,
    descriptors: Vec<McpDescriptor>,
    /// Which file each server came from — see [`crate::auth::AuthRegistry`] for
    /// why the file matters and not just the name.
    sources: BTreeMap<String, PathBuf>,
    issues: Vec<LoadIssue>,
}

impl McpRegistry {
    /// Loads `mcp.yaml` from each of the profile directories, in order.
    ///
    /// Never fails: a missing file means no servers, and a broken one is an issue
    /// you can read in the UI rather than a refusal to start.
    #[must_use]
    pub fn load_dirs(dirs: &[impl AsRef<Path>], http: &Client) -> Self {
        let mut registry = Self::default();
        for dir in dirs {
            registry.read(&dir.as_ref().join(MCP_REGISTRY_FILE), http);
        }
        registry.descriptors.sort_by(|a, b| a.name.cmp(&b.name));
        registry
    }

    /// Loads `mcp.yaml` from a single profiles directory.
    #[must_use]
    pub fn load(dir: &Path, http: &Client) -> Self {
        Self::load_dirs(&[dir], http)
    }

    /// Folds one `mcp.yaml` in, on top of whatever earlier directories declared.
    fn read(&mut self, path: &Path, http: &Client) {
        if !path.exists() {
            debug!(path = %path.display(), "no MCP registry");
            return;
        }

        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                self.issues.push(LoadIssue::new(path, error.to_string()));
                return;
            }
        };

        let file: RegistryFile = match serde_yaml_ng::from_str(&text) {
            Ok(file) => file,
            Err(error) => {
                self.issues.push(LoadIssue::from_yaml(path, &error));
                return;
            }
        };

        for config in file.servers {
            match self.sources.get(&config.name) {
                Some(previous) if previous == path => {
                    self.issues.push(LoadIssue::new(
                        path,
                        format!("duplicate MCP server `{}`", config.name),
                    ));
                    continue;
                }
                Some(previous) => warn!(
                    name = %config.name,
                    path = %path.display(),
                    shadowed = %previous.display(),
                    "MCP server overridden by a later directory"
                ),
                None => {}
            }

            let headers = match HeaderTemplates::compile(&config.headers) {
                Ok(headers) => headers,
                Err(message) => {
                    self.issues.push(LoadIssue::new(
                        path,
                        format!("MCP server `{}`: {message}", config.name),
                    ));
                    continue;
                }
            };

            let protocol_version = match config.protocol_version.as_deref().map(str::parse) {
                None => None,
                Some(Ok(revision)) => Some(revision),
                Some(Err(message)) => {
                    self.issues.push(LoadIssue::new(
                        path,
                        format!("MCP server `{}`: {message}", config.name),
                    ));
                    continue;
                }
            };

            let hooks = match compile_hooks(&config.name, &config.hooks) {
                Ok(hooks) => hooks,
                Err(message) => {
                    self.issues.push(LoadIssue::new(path, message));
                    continue;
                }
            };

            if let Err(message) = check_capture(&config.name, &config.capture) {
                self.issues.push(LoadIssue::new(path, message));
                continue;
            }

            let server = McpServer {
                name: config.name.clone(),
                url: config.url,
                auth: config.auth,
                tools: config.tools,
                headers,
                timeout: Duration::from_millis(config.timeout_ms),
                protocol_version,
                hooks,
                capture: config.capture,
            };

            debug!(
                name = %server.name,
                url = %server.url,
                hooks = server.hooks.len(),
                capture = server.capture.len(),
                "MCP server registered"
            );
            self.descriptors
                .retain(|existing| existing.name != server.name);
            self.descriptors
                .push(describe_server(&server, config.headers.keys()));
            self.sources.insert(server.name.clone(), path.to_path_buf());
            self.clients
                .insert(server.name.clone(), McpClient::new(server, http.clone()));
        }
    }

    /// Looks a server up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&McpClient> {
        self.clients.get(name)
    }

    /// Every server, for the UI.
    #[must_use]
    pub fn descriptors(&self) -> &[McpDescriptor] {
        &self.descriptors
    }

    /// Every server's name, in the order the descriptors are listed.
    ///
    /// This is the set a chat run may reach: a server is declared once, in
    /// `mcp.yaml`, and every `kind: chat` profile is offered all of it. Declaring
    /// it there is the opt-in — there is no second one per profile.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.descriptors
            .iter()
            .map(|descriptor| descriptor.name.clone())
            .collect()
    }

    /// Entries that could not be loaded, and why.
    #[must_use]
    pub fn issues(&self) -> &[LoadIssue] {
        &self.issues
    }

    /// Whether anything is declared at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    #[serde(default)]
    servers: Vec<ServerConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ServerConfig {
    name: String,
    url: Url,
    #[serde(default)]
    auth: Option<String>,
    /// Restricts what the model may reach. Empty — the default — offers whatever
    /// the server advertises.
    #[serde(default)]
    tools: Vec<String>,
    /// Extra headers, as `MiniJinja` templates rendered on every request.
    ///
    /// `env` is in scope, read fresh each time, so a rotated token is picked up
    /// without a restart. An undefined variable is an error rather than an empty
    /// header — use `| default(...)` when a header really is optional.
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default = "default_timeout_ms")]
    timeout_ms: u64,
    /// Revision to speak, skipping negotiation.
    ///
    /// A string rather than a [`crate::mcp::Revision`] so that an unknown one is
    /// a load issue naming what this build speaks, in the file it came from —
    /// rather than a serde message about an untagged enum.
    #[serde(default)]
    protocol_version: Option<String>,
    /// What fires before and after a `tools/call` on this server.
    #[serde(default)]
    hooks: Vec<HookConfig>,
    /// What to keep out of this server's tool results, for a hook or a header
    /// to use later.
    ///
    /// Deserialised straight into the rules, so a bad `JSONPath` or an
    /// uncompilable pattern is a load issue naming this file — and validated
    /// below, so a name no template could write is one too.
    #[serde(default)]
    capture: Vec<CaptureRule>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HookConfig {
    /// Required, and unique within the server: a hook is something you read
    /// about in a trace, and an unnamed one is a URL you have to recognise.
    name: String,
    /// Phases it fires on. At least one, or it is a hook that never happens.
    on: Vec<HookPhase>,
    /// Tools it applies to, as regexes matched against the whole name. Empty —
    /// the default — is every tool.
    #[serde(default)]
    tools: Vec<String>,
    /// What has to hold for it to fire, as a `MiniJinja` expression. Absent —
    /// the default — is no condition.
    ///
    /// Spelled `if:` in the file, which is a keyword here; the field carries the
    /// longer name and serde does the translating.
    #[serde(default, rename = "if")]
    condition: Option<String>,
    /// What its failure does to the call. `fail` by default.
    #[serde(default)]
    on_error: OnError,
    /// What it does, in order. At least one, or it is a hook that happens.
    actions: Vec<ActionConfig>,
}

/// One action, written under the key naming its kind: `- http:`.
///
/// A struct with one field rather than an enum, because that is what the file
/// looks like and what YAML can carry without a `!http` tag on every entry — and
/// because it keeps `deny_unknown_fields` doing the naming: `- carrier_pigeon:`
/// is refused at the line it was written on. The next kind is another field
/// beside this one plus a check that exactly one was named, which is an addition
/// rather than a break. See [`super::hook`] for why the shape is tagged at all.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionConfig {
    /// The only kind so far.
    http: HttpConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HttpConfig {
    /// A URL, or a `MiniJinja` template producing one. Read as text so a
    /// template is not rejected as a bad URL before anybody looks at it.
    url: String,
    /// `POST` by default, because a hook carries a payload.
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    auth: Option<String>,
    /// Extra headers, as `MiniJinja` templates rendered on every request —
    /// exactly the ones a server's own `headers:` takes.
    #[serde(default)]
    headers: BTreeMap<String, String>,
    /// The JSON document to send, as written. Every string in it is a template.
    #[serde(default)]
    json: Option<serde_json::Value>,
    /// The form to send, one entry per field, each naming uploads of the run.
    ///
    /// Exclusive with `json`: a request is one body, and a file declaring two is
    /// a file whose author expected something else to happen.
    #[serde(default)]
    multipart: Option<BTreeMap<String, PartConfig>>,
    #[serde(default = "default_hook_timeout_ms")]
    timeout_ms: u64,
}

/// What one multipart field carries: one file, or several.
///
/// The short form is the common one and stays short. The list is not sugar for
/// repeating the field — several files under one name is what the wire carries
/// and what every upload handler reads.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PartConfig {
    /// One template.
    One(String),
    /// Several, in order.
    Many(Vec<String>),
}

impl PartConfig {
    /// The templates it holds, however it was written.
    fn sources(&self) -> Vec<String> {
        match self {
            Self::One(source) => vec![source.clone()],
            Self::Many(sources) => sources.clone(),
        }
    }
}

/// Checks a server's capture rules, or says which one is wrong and why.
///
/// The paths and the patterns are already compiled by the time this runs — serde
/// did that, and a bad one never got here. What is left is the pair of things
/// only a whole rule can answer: a rule that captures nothing, and a variable
/// name `{{ vars.… }}` could not write.
fn check_capture(server: &str, rules: &[CaptureRule]) -> Result<(), String> {
    for (index, rule) in rules.iter().enumerate() {
        // Numbered rather than named: a rule has no name, and "the third one"
        // is what somebody counts down the file to find.
        rule.validate().map_err(|errors| {
            format!(
                "MCP server `{server}`: capture rule {}: {}",
                index + 1,
                complaints(&errors)
            )
        })?;
    }
    Ok(())
}

/// The sentences a `ValidationErrors` holds, without the machinery around them.
///
/// `Display` renders the whole tree, keys and all, which puts `__all__:` in
/// front of every whole-rule complaint — the internal name for "not about one
/// field", and not a thing anybody wrote in `mcp.yaml`.
fn complaints(errors: &ValidationErrors) -> String {
    errors
        .field_errors()
        .values()
        .flat_map(|failures| failures.iter())
        .map(|failure| {
            failure
                .message
                .as_ref()
                .map_or_else(|| failure.code.to_string(), ToString::to_string)
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Compiles a server's hooks, or says which one is wrong and why.
fn compile_hooks(server: &str, declared: &[HookConfig]) -> Result<Vec<Hook>, String> {
    let mut hooks: Vec<Hook> = Vec::with_capacity(declared.len());

    for config in declared {
        let label = format!("MCP server `{server}`: hook `{}`", config.name);

        if hooks.iter().any(|hook| hook.name == config.name) {
            return Err(format!("{label} is declared twice"));
        }
        // A hook that fires on nothing is a webhook somebody is waiting for.
        let phases: BTreeSet<HookPhase> = config.on.iter().copied().collect();
        if phases.is_empty() {
            return Err(format!(
                "{label}: `on` must name at least one of `before`, `after`"
            ));
        }
        // And a hook that does nothing is the same silence written differently.
        if config.actions.is_empty() {
            return Err(format!("{label}: `actions` must declare at least one"));
        }

        // Compiled here rather than matched as text on every call: a pattern that
        // does not parse is a hook covering nothing, and finding that out on the
        // first `tools/call` is finding it out too late to matter.
        let tools = config
            .tools
            .iter()
            .map(|pattern| NamePattern::compile(pattern))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|why| format!("{label}: `tools`: {why}"))?;

        let mut actions = Vec::with_capacity(config.actions.len());
        for (index, declared) in config.actions.iter().enumerate() {
            // Numbered from one, and named in every error below: "hook `audit`"
            // is not an answer when the hook makes three calls and one of them
            // has the typo.
            let label = format!("{label}: action {}", index + 1);
            actions.push(HookAction::Http(compile_http(&label, &declared.http)?));
        }

        // Compiled here for the same reason `tools:` is: a condition that does
        // not parse is a hook that fires on nothing or on everything, and the
        // first `tools/call` is too late to find that out.
        let condition = config
            .condition
            .as_deref()
            .map(HookCondition::compile)
            .transpose()
            .map_err(|why| format!("{label}: `if` {why}"))?;

        hooks.push(Hook {
            name: config.name.clone(),
            phases,
            tools,
            condition,
            on_error: config.on_error,
            actions,
        });
    }

    Ok(hooks)
}

/// One `kind: http` action, compiled.
fn compile_http(label: &str, http: &HttpConfig) -> Result<HttpAction, String> {
    let method = match &http.method {
        None => Method::POST,
        Some(verb) => Method::from_str(&verb.to_ascii_uppercase())
            .map_err(|_| format!("{label}: `{verb}` is not an HTTP method"))?,
    };
    let headers =
        HeaderTemplates::compile(&http.headers).map_err(|why| format!("{label}: {why}"))?;

    // Two bodies is not a body. Refused here rather than resolved by precedence:
    // whichever one this picked, it would be the other one somebody meant.
    let body = match (&http.json, &http.multipart) {
        (Some(_), Some(_)) => {
            return Err(format!(
                "{label}: `json` and `multipart` are two bodies; declare one"
            ));
        }
        (Some(json), None) => {
            check_json(label, json)?;
            Some(HookBody::Json(json.clone()))
        }
        (None, Some(multipart)) => Some(HookBody::Multipart(compile_parts(label, multipart)?)),
        (None, None) => None,
    };

    // A plain URL is parsed now — a typo in a scheme belongs to startup, not to
    // the first tool call. A template can only be syntax-checked here; what it
    // produces is a URL or a hook failure, per firing.
    let url = if HookUrl::is_template(&http.url) {
        crate::mcp::hook::check_template("url", &http.url)
            .map_err(|why| format!("{label}: {why}"))?;
        HookUrl::Template(http.url.clone())
    } else {
        HookUrl::Fixed(
            Url::parse(&http.url)
                .map_err(|why| format!("{label}: `url`: `{}` is not a URL: {why}", http.url))?,
        )
    };

    Ok(HttpAction {
        url,
        method,
        auth: http.auth.clone(),
        headers,
        body,
        timeout: Duration::from_millis(http.timeout_ms),
    })
}

/// Every string of a JSON body is a template, and every one is checked now.
///
/// The whole tree, not the top level: a template three fields deep is exactly
/// the one nobody re-reads, and a syntax error in it belongs to startup like
/// every other one here.
fn check_json(label: &str, node: &serde_json::Value) -> Result<(), String> {
    match node {
        serde_json::Value::String(text) => {
            crate::mcp::hook::check_template("json", text).map_err(|why| format!("{label}: {why}"))
        }
        serde_json::Value::Array(items) => {
            items.iter().try_for_each(|item| check_json(label, item))
        }
        serde_json::Value::Object(fields) => fields
            .values()
            .try_for_each(|value| check_json(label, value)),
        _ => Ok(()),
    }
}

/// The multipart fields, in the order `mcp.yaml` wrote them.
fn compile_parts(
    label: &str,
    declared: &BTreeMap<String, PartConfig>,
) -> Result<Vec<PartSpec>, String> {
    let mut parts = Vec::with_capacity(declared.len());

    for (field, config) in declared {
        let sources = config.sources();
        // An empty list is a field that will never carry anything, which is a
        // `422` waiting for the first run rather than a startup issue.
        if sources.is_empty() {
            return Err(format!("{label}: `multipart`: `{field}` names no file"));
        }
        for source in &sources {
            crate::mcp::hook::check_template(&format!("multipart.{field}"), source)
                .map_err(|why| format!("{label}: {why}"))?;
        }
        parts.push(PartSpec {
            field: field.clone(),
            sources,
        });
    }

    Ok(parts)
}

/// One server, for the UI. Names only, never a credential.
///
/// `declared` is the header names as `mcp.yaml` wrote them: the compiled
/// templates no longer have them, and a listing of what a server sends is one of
/// the two halves of "what is this about to do".
fn describe_server<'a>(
    server: &McpServer,
    declared: impl Iterator<Item = &'a String>,
) -> McpDescriptor {
    McpDescriptor {
        name: server.name.clone(),
        url: server.url.to_string(),
        auth: server.auth.clone(),
        tools: server.tools.clone(),
        headers: declared.cloned().collect(),
        uses_auth: server.headers.providers().map(str::to_owned).collect(),
        hooks: server.hooks.iter().map(describe).collect(),
        capture: server.capture.iter().map(describe_capture).collect(),
    }
}

/// One capture rule, for the UI. Names only, never a captured value.
fn describe_capture(rule: &CaptureRule) -> CaptureDescriptor {
    CaptureDescriptor {
        // The patterns as written, for the same reason a hook's are.
        tools: rule.tools.iter().map(|p| p.as_str().to_owned()).collect(),
        vars: rule.vars.keys().cloned().collect(),
    }
}

/// One hook, for the UI. Names only, never a rendered value.
fn describe(hook: &Hook) -> HookDescriptor {
    HookDescriptor {
        name: hook.name.clone(),
        on: hook.phases.iter().copied().collect(),
        // The patterns as written. The compiled form carries anchors this added,
        // and a UI showing those would be quoting something nobody typed.
        tools: hook.tools.iter().map(|p| p.as_str().to_owned()).collect(),
        condition: hook.condition.as_ref().map(|c| c.source().to_owned()),
        on_error: hook.on_error,
        actions: hook.actions.iter().map(describe_action).collect(),
    }
}

/// One action, for the UI.
fn describe_action(action: &HookAction) -> ActionDescriptor {
    let HookAction::Http(http) = action;
    let (sends, fields) = match &http.body {
        None => ("nothing", Vec::new()),
        Some(HookBody::Json(_)) => ("json", Vec::new()),
        Some(HookBody::Multipart(parts)) => (
            "multipart",
            parts.iter().map(|part| part.field.clone()).collect(),
        ),
    };

    ActionDescriptor {
        kind: action.kind().to_owned(),
        // As written, template and all: a UI showing a rendered URL would be
        // showing one firing of many, and this listing belongs to none of them.
        url: http.url.source().to_owned(),
        method: http.method.to_string(),
        auth: http.auth.clone(),
        headers: http.headers.names().map(str::to_owned).collect(),
        sends: sends.to_owned(),
        fields,
        uses_auth: action.header_providers().map(str::to_owned).collect(),
    }
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_hook_timeout_ms() -> u64 {
    DEFAULT_HOOK_TIMEOUT_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(tag: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-mcp-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(MCP_REGISTRY_FILE), body).unwrap();
        dir
    }

    #[test]
    fn no_registry_means_no_servers_and_no_complaints() {
        let dir = std::env::temp_dir().join(format!("mire-mcp-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let registry = McpRegistry::load(&dir, &Client::new());
        assert!(registry.is_empty());
        assert!(registry.issues().is_empty());
    }

    #[test]
    fn a_later_directory_takes_a_server_the_earlier_one_declared() {
        let base = write(
            "layer-base",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n",
        );
        let mine = write(
            "layer-mine",
            "servers:\n  - name: files\n    url: https://staging.internal/mcp\n",
        );

        let registry = McpRegistry::load_dirs(&[&base, &mine], &Client::new());

        assert_eq!(registry.descriptors().len(), 1);
        assert_eq!(
            registry.descriptors()[0].url,
            "https://staging.internal/mcp"
        );
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
    }

    /// Overriding is only ever *across* directories. Twice in one file is still
    /// the typo it always was.
    #[test]
    fn a_duplicate_inside_the_later_directory_is_still_reported() {
        let base = write(
            "layer-dup-base",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n",
        );
        let mine = write(
            "layer-dup-mine",
            "servers:\n  - name: files\n    url: https://a.internal/mcp\n  - name: files\n    url: https://b.internal/mcp\n",
        );

        let registry = McpRegistry::load_dirs(&[&base, &mine], &Client::new());

        assert_eq!(registry.descriptors().len(), 1);
        assert_eq!(registry.descriptors()[0].url, "https://a.internal/mcp");
        assert_eq!(registry.issues().len(), 1);
        assert!(
            registry.issues()[0]
                .message
                .contains("duplicate MCP server")
        );
    }

    #[test]
    fn a_server_loads_with_its_defaults() {
        let dir = write(
            "basic",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let client = registry.get("files").unwrap();
        assert_eq!(client.server().timeout, Duration::from_secs(30));
        // No list means no restriction, which is the documented default.
        assert!(client.server().tools.is_empty());
        assert!(client.server().offers("whatever"));
    }

    #[test]
    fn a_tool_list_restricts_what_the_model_can_reach() {
        let dir = write(
            "restricted",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    auth: workload\n    tools:\n      - read_file\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        let client = registry.get("files").unwrap();
        assert_eq!(client.server().auth.as_deref(), Some("workload"));
        assert!(client.server().offers("read_file"));
        assert!(!client.server().offers("write_file"));
    }

    #[test]
    fn a_duplicate_name_is_reported_and_the_first_one_wins() {
        let dir = write(
            "dup",
            "servers:\n  - name: a\n    url: https://one.internal/mcp\n  - name: a\n    url: https://two.internal/mcp\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("duplicate"));
        assert_eq!(
            registry.get("a").unwrap().server().url.as_str(),
            "https://one.internal/mcp"
        );
    }

    #[test]
    fn a_registry_that_does_not_parse_is_an_issue_rather_than_a_refusal_to_start() {
        let dir = write("syntax", "servers: [unclosed\n");
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.is_empty());
        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].line.is_some());
    }

    const WITH_HOOK: &str = r"
servers:
  - name: files
    url: https://mcp.internal/mcp
    hooks:
      - name: audit
        on:
          - before
          - after
        tools:
          - write_file
        actions:
          - http:
              url: https://audit.internal/events
              auth: workload
              headers:
                x-source: mire
";

    /// A server declaring one hook whose single action is `body`.
    fn one_action(body: &str) -> String {
        format!(
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      \
             - name: audit\n        on:\n          - before\n        actions:\n          - http:\n{body}"
        )
    }

    #[test]
    fn a_hook_loads_with_its_phases_its_tools_and_its_defaults() {
        let dir = write("hook", WITH_HOOK);
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let hooks = &registry.get("files").unwrap().server().hooks;
        assert_eq!(hooks.len(), 1);

        let hook = &hooks[0];
        assert!(hook.fires(HookPhase::Before, "write_file"));
        assert!(hook.fires(HookPhase::After, "write_file"));
        assert!(!hook.fires(HookPhase::Before, "read_file"));
        // A hook is loud by default: it is something you asked for.
        assert_eq!(hook.on_error, OnError::Fail);

        assert_eq!(hook.actions.len(), 1);
        let action = &hook.actions[0];
        assert_eq!(action.auth(), Some("workload"));
        assert_eq!(action.url().source(), "https://audit.internal/events");

        let HookAction::Http(http) = action;
        assert_eq!(http.method, Method::POST);
        assert_eq!(http.timeout, Duration::from_millis(DEFAULT_HOOK_TIMEOUT_MS));
        // Neither `json:` nor `multipart:` is no body at all — the one default
        // that cannot surprise the endpoint on the other end.
        assert!(http.body.is_none());
    }

    #[test]
    fn a_hook_is_advertised_by_name_and_never_by_value() {
        let dir = write("hook-descriptor", WITH_HOOK);
        let registry = McpRegistry::load(&dir, &Client::new());

        let hooks = &registry.descriptors()[0].hooks;
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name, "audit");
        assert_eq!(hooks[0].on, vec![HookPhase::Before, HookPhase::After]);

        let action = &hooks[0].actions[0];
        assert_eq!(action.kind, "http");
        assert_eq!(action.method, "POST");
        // What it sends, not only where: a hook posting a document somebody wrote
        // and a hook shipping a run's files are different events.
        assert_eq!(action.sends, "nothing");
        // The header's name travels; its rendered value never does.
        assert_eq!(action.headers, vec!["x-source".to_owned()]);
    }

    #[test]
    fn several_actions_load_in_the_order_they_were_written() {
        // The shape this is all for: the file goes to the API about to run it,
        // the line goes to the audit sink, and both belong to one event.
        let dir = write(
            "hook-actions",
            r"
servers:
  - name: files
    url: https://mcp.internal/mcp
    hooks:
      - name: upload
        on:
          - before
        actions:
          - http:
              url: https://intake.internal/inputs
              multipart:
                file: '{{ uploads[0].path }}'
          - http:
              url: https://audit.internal/events
              json:
                tool: '{{ tool }}'
",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let hook = &registry.get("files").unwrap().server().hooks[0];
        assert_eq!(hook.actions.len(), 2);
        assert_eq!(
            hook.actions[0].url().source(),
            "https://intake.internal/inputs"
        );
        assert_eq!(
            hook.actions[1].url().source(),
            "https://audit.internal/events"
        );

        let advertised = &registry.descriptors()[0].hooks[0].actions;
        assert_eq!(advertised[0].sends, "multipart");
        assert_eq!(advertised[0].fields, vec!["file".to_owned()]);
        assert_eq!(advertised[1].sends, "json");
    }

    #[test]
    fn a_hook_that_does_nothing_is_refused_like_one_that_fires_on_nothing() {
        let dir = write(
            "hook-no-action",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on:\n          - before\n        actions: []\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("audit"));
        assert!(registry.issues()[0].message.contains("`actions`"));
    }

    #[test]
    fn a_hook_that_fires_on_nothing_is_a_webhook_nobody_will_ever_get() {
        let dir = write(
            "hook-no-phase",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: []\n        actions:\n          - http:\n              url: https://audit.internal/events\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("audit"));
        assert!(registry.issues()[0].message.contains("`on`"));
    }

    #[test]
    fn two_hooks_of_the_same_name_are_reported() {
        let dir = write(
            "hook-dup",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on:\n          - before\n        actions:\n          - http:\n              url: https://one.internal/e\n      - name: audit\n        on:\n          - after\n        actions:\n          - http:\n              url: https://two.internal/e\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("declared twice"));
        // The server goes with it rather than loading half its hooks: a gate that
        // silently did not load is worse than one that refused to.
        assert!(registry.get("files").is_none());
    }

    #[test]
    fn a_url_holding_a_template_is_kept_as_one() {
        let dir = write(
            "hook-url-template",
            &one_action("              url: https://audit.internal/{{ vars.session }}\n"),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let action = &registry.get("files").unwrap().server().hooks[0].actions[0];
        assert!(matches!(action.url(), HookUrl::Template(_)));
        // As written, template and all: what it renders to belongs to a firing.
        assert_eq!(
            action.url().source(),
            "https://audit.internal/{{ vars.session }}"
        );
    }

    /// An `mcp.yaml` whose one hook is fired under `condition`.
    fn one_condition(condition: &str) -> String {
        format!(
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        \
             on:\n          - after\n        if: '{condition}'\n        actions:\n          - http:\n              \
             url: https://audit.internal/events\n"
        )
    }

    #[test]
    fn a_hook_loads_the_condition_it_fires_under() {
        let dir = write(
            "hook-if",
            &one_condition("{{ vars.session is defined and vars.job is defined }}"),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let hook = &registry.get("files").unwrap().server().hooks[0];
        assert_eq!(
            hook.condition.as_ref().map(HookCondition::source),
            Some("{{ vars.session is defined and vars.job is defined }}")
        );

        // And it is advertised, as written, because "what is this about to do"
        // has to cover a hook that will sit out most of a run.
        let advertised = &registry.descriptors()[0].hooks[0];
        assert_eq!(
            advertised.condition.as_deref(),
            Some("{{ vars.session is defined and vars.job is defined }}")
        );
    }

    #[test]
    fn a_condition_may_be_written_without_the_delimiters() {
        let dir = write("hook-if-bare", &one_condition("vars.session is defined"));
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let hook = &registry.get("files").unwrap().server().hooks[0];
        // Kept as written, because that is what the trace will quote back.
        assert_eq!(
            hook.condition.as_ref().map(HookCondition::source),
            Some("vars.session is defined")
        );
    }

    #[test]
    fn a_condition_that_does_not_parse_is_caught_at_startup() {
        let dir = write("hook-if-bad", &one_condition("{{ vars.session is }}"));
        let registry = McpRegistry::load(&dir, &Client::new());

        // Compiled at load, like `tools:` — a condition nobody can evaluate is a
        // hook that fires on nothing, and the first tool call is too late.
        assert_eq!(registry.issues().len(), 1);
        assert!(
            registry.issues()[0].message.contains("`if`"),
            "{:?}",
            registry.issues()
        );
        assert!(
            registry.issues()[0].message.contains("audit"),
            "{:?}",
            registry.issues()
        );
    }

    #[test]
    fn a_condition_that_is_a_template_rather_than_an_expression_is_refused() {
        let dir = write(
            "hook-if-template",
            &one_condition("{% if vars.session %}yes{% endif %}"),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        // It would render to `yes` or to the empty string, and only one of those
        // is falsy by accident. Refused rather than quietly accepted.
        assert_eq!(registry.issues().len(), 1);
        assert!(
            registry.issues()[0].message.contains("`if`"),
            "{:?}",
            registry.issues()
        );
    }

    #[test]
    fn a_hook_with_no_condition_is_the_ordinary_case() {
        let dir = write("hook-no-condition", WITH_HOOK);
        let registry = McpRegistry::load(&dir, &Client::new());

        let hook = &registry.get("files").unwrap().server().hooks[0];
        assert!(hook.condition.is_none());
    }

    #[test]
    fn a_url_that_is_not_a_url_and_not_a_template_is_caught_at_startup() {
        let dir = write(
            "hook-url-bad",
            &one_action("              url: audit.internal/events\n"),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        // Reading `url:` as text to allow templates must not cost the plain case
        // its startup check.
        assert_eq!(registry.issues().len(), 1);
        assert!(
            registry.issues()[0].message.contains("audit"),
            "{:?}",
            registry.issues()
        );
        assert!(
            registry.issues()[0].message.contains("`url`"),
            "{:?}",
            registry.issues()
        );
    }

    #[test]
    fn a_url_template_that_does_not_parse_is_caught_at_startup() {
        let dir = write(
            "hook-url-template-bad",
            &one_action("              url: https://audit.internal/{{ vars.session\n"),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(
            registry.issues()[0].message.contains("`url`"),
            "{:?}",
            registry.issues()
        );
    }

    #[test]
    fn a_json_body_loads_as_the_document_it_is() {
        let dir = write(
            "hook-json",
            &one_action(
                "              url: https://audit.internal/e\n              json:\n                tool: '{{ tool }}'\n                count: 3\n",
            ),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let HookAction::Http(http) = &registry.get("files").unwrap().server().hooks[0].actions[0];
        let Some(HookBody::Json(document)) = &http.body else {
            panic!("a json body");
        };
        // The shape survives the file: a number is a number, and the endpoint's
        // schema is what was written rather than what a string template produced.
        assert_eq!(document["tool"], "{{ tool }}");
        assert_eq!(document["count"], 3);
    }

    #[test]
    fn a_json_template_that_does_not_parse_is_caught_at_startup() {
        // Three fields deep, because that is the one nobody re-reads.
        let dir = write(
            "hook-json-bad",
            &one_action(
                "              url: https://audit.internal/e\n              json:\n                nested:\n                  deep: '{{ unclosed'\n",
            ),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("audit"));
        assert!(registry.issues()[0].message.contains("json"));
    }

    #[test]
    fn a_multipart_field_loads_in_either_form() {
        let dir = write(
            "hook-multipart",
            &one_action(
                "              url: https://intake.internal/inputs\n              multipart:\n                file: '{{ uploads[0].path }}'\n                extra:\n                  - a.txt\n                  - b.txt\n",
            ),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let HookAction::Http(http) = &registry.get("files").unwrap().server().hooks[0].actions[0];
        let Some(HookBody::Multipart(parts)) = &http.body else {
            panic!("a multipart body");
        };
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].field, "extra");
        assert_eq!(parts[0].sources, vec!["a.txt", "b.txt"]);
        assert_eq!(parts[1].field, "file");
        assert_eq!(parts[1].sources, vec!["{{ uploads[0].path }}"]);

        // Names only in the descriptor, the same as the headers beside them.
        let advertised = &registry.descriptors()[0].hooks[0].actions[0];
        assert_eq!(advertised.sends, "multipart");
        assert_eq!(
            advertised.fields,
            vec!["extra".to_owned(), "file".to_owned()]
        );
    }

    #[test]
    fn a_multipart_template_that_does_not_parse_is_caught_at_startup() {
        let dir = write(
            "hook-multipart-bad",
            &one_action(
                "              url: https://intake.internal/inputs\n              multipart:\n                file: '{{ unclosed'\n",
            ),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("multipart.file"));
    }

    #[test]
    fn a_body_and_a_form_together_are_refused_rather_than_ranked() {
        // Whichever one this picked, it would be the other one somebody meant.
        let dir = write(
            "hook-two-bodies",
            &one_action(
                "              url: https://audit.internal/e\n              json:\n                tool: '{{ tool }}'\n              multipart:\n                file: report.pdf\n",
            ),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("`json`"));
        assert!(registry.issues()[0].message.contains("`multipart`"));
    }

    #[test]
    fn an_issue_names_which_action_of_which_hook_it_was() {
        // "hook `audit`" is not an answer when the hook makes three calls and
        // one of them has the typo.
        let dir = write(
            "hook-action-number",
            r"
servers:
  - name: files
    url: https://mcp.internal/mcp
    hooks:
      - name: audit
        on:
          - before
        actions:
          - http:
              url: https://one.internal/e
          - http:
              url: two.internal/e
",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(
            registry.issues()[0].message.contains("action 2"),
            "{:?}",
            registry.issues()
        );
    }

    #[test]
    fn a_tool_pattern_that_is_not_a_regex_is_caught_at_startup_too() {
        let dir = write(
            "hook-tools",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: gate\n        on:\n          - before\n        tools:\n          - 'write_('\n        actions:\n          - http:\n              url: https://policy.internal/decide\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("gate"));
        assert!(registry.issues()[0].message.contains("write_("));
        // A gate whose pattern does not compile covers nothing, so the server it
        // guards does not load either.
        assert!(registry.get("files").is_none());
    }

    #[test]
    fn a_method_that_is_not_one_is_caught_too() {
        let dir = write(
            "hook-method",
            &one_action(
                "              url: https://audit.internal/e\n              method: 'not a verb'\n",
            ),
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("not an HTTP method"));
    }

    #[test]
    fn an_action_kind_nobody_implements_names_itself() {
        let dir = write(
            "hook-kind",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on:\n          - before\n        actions:\n          - carrier_pigeon:\n              url: https://audit.internal/e\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("carrier_pigeon"));
    }

    #[test]
    fn an_unknown_field_is_caught_rather_than_silently_ignored() {
        let dir = write(
            "typo",
            "servers:\n  - name: a\n    url: https://mcp.internal/mcp\n    timeout: 5000\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());
        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("timeout"));
    }
}
