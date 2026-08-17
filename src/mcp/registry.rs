//! `mcp.yaml`, sitting next to the profiles and `auth.yaml`.
//!
//! A server is declared once and referenced by name, for the same reason auth is:
//! the same server is worth pointing several profiles at, and worth replaying
//! across auth modes without duplicating anything.
//!
//! Same loading policy as everywhere else — a bad entry is reported and skipped,
//! the rest still work.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

use reqwest::{Client, Method};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::debug;
use url::Url;

use super::client::{McpClient, McpServer};
use super::headers::HeaderTemplates;
use super::hook::{Hook, HookAction, HookPhase, HookUrl, HttpAction, NamePattern, OnError};
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
    /// Variables it waits for before firing at all. Empty means it fires
    /// whatever the run has captured.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub when_defined: Vec<String>,
    /// What its failure does to the call.
    pub on_error: OnError,
    /// The action's kind: `http`.
    pub action: String,
    /// Where it goes.
    pub url: String,
    /// Auth provider it authenticates with, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auth: Option<String>,
    /// Names of the extra headers it sends. **Names only** — the values are
    /// rendered per request and are usually credentials.
    pub headers: Vec<String>,
    /// Patterns naming the uploads it attaches. Empty means it attaches none,
    /// and saying so is the point: a run's files leaving for a third address is
    /// exactly what "what is this about to do" has to cover.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Auth providers its header templates read, beyond `auth:`.
    pub uses_auth: Vec<String>,
}

/// Every declared MCP server, keyed by name.
#[derive(Debug, Default)]
pub struct McpRegistry {
    clients: BTreeMap<String, McpClient>,
    descriptors: Vec<McpDescriptor>,
    issues: Vec<LoadIssue>,
}

impl McpRegistry {
    /// Loads `mcp.yaml` from the profiles directory.
    ///
    /// Never fails: a missing file means no servers, and a broken one is an issue
    /// you can read in the UI rather than a refusal to start.
    #[must_use]
    pub fn load(dir: &Path, http: &Client) -> Self {
        let path = dir.join(MCP_REGISTRY_FILE);
        let mut registry = Self::default();

        if !path.exists() {
            debug!(path = %path.display(), "no MCP registry");
            return registry;
        }

        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                registry
                    .issues
                    .push(LoadIssue::new(&path, error.to_string()));
                return registry;
            }
        };

        let file: RegistryFile = match serde_yaml_ng::from_str(&text) {
            Ok(file) => file,
            Err(error) => {
                registry.issues.push(LoadIssue::from_yaml(&path, &error));
                return registry;
            }
        };

        for config in file.servers {
            if registry.clients.contains_key(&config.name) {
                registry.issues.push(LoadIssue::new(
                    &path,
                    format!("duplicate MCP server `{}`", config.name),
                ));
                continue;
            }

            let headers = match HeaderTemplates::compile(&config.headers) {
                Ok(headers) => headers,
                Err(message) => {
                    registry.issues.push(LoadIssue::new(
                        &path,
                        format!("MCP server `{}`: {message}", config.name),
                    ));
                    continue;
                }
            };

            let protocol_version = match config.protocol_version.as_deref().map(str::parse) {
                None => None,
                Some(Ok(revision)) => Some(revision),
                Some(Err(message)) => {
                    registry.issues.push(LoadIssue::new(
                        &path,
                        format!("MCP server `{}`: {message}", config.name),
                    ));
                    continue;
                }
            };

            let hooks = match compile_hooks(&config.name, &config.hooks) {
                Ok(hooks) => hooks,
                Err(message) => {
                    registry.issues.push(LoadIssue::new(&path, message));
                    continue;
                }
            };

            let server = McpServer {
                name: config.name.clone(),
                url: config.url,
                auth: config.auth,
                tools: config.tools,
                headers,
                timeout: Duration::from_millis(config.timeout_ms),
                protocol_version,
                hooks,
            };

            debug!(
                name = %server.name,
                url = %server.url,
                hooks = server.hooks.len(),
                "MCP server registered"
            );
            registry.descriptors.push(McpDescriptor {
                name: server.name.clone(),
                url: server.url.to_string(),
                auth: server.auth.clone(),
                tools: server.tools.clone(),
                headers: config.headers.keys().cloned().collect(),
                uses_auth: server.headers.providers().map(str::to_owned).collect(),
                hooks: server.hooks.iter().map(describe).collect(),
            });
            registry
                .clients
                .insert(server.name.clone(), McpClient::new(server, http.clone()));
        }

        registry.descriptors.sort_by(|a, b| a.name.cmp(&b.name));
        registry
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
    /// Variables that must have been captured for it to fire. Empty — the
    /// default — is no condition.
    ///
    /// Plain names, deliberately: this asks whether a value exists, and a
    /// pattern here would invite `.*`, which is "fire once anything at all has
    /// been captured" and means nothing.
    #[serde(default)]
    when_defined: Vec<String>,
    /// What its failure does to the call. `fail` by default.
    #[serde(default)]
    on_error: OnError,
    action: ActionConfig,
}

/// Tagged from the start. See [`super::hook`] for why.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum ActionConfig {
    Http(HttpConfig),
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
    /// Body template. Absent sends the call itself as JSON.
    #[serde(default)]
    body: Option<String>,
    /// Which of the run's uploads to attach, as regexes on the file name.
    /// Empty — the default — is none of them.
    #[serde(default)]
    files: Vec<String>,
    #[serde(default = "default_hook_timeout_ms")]
    timeout_ms: u64,
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

        let ActionConfig::Http(http) = &config.action;

        let method = match &http.method {
            None => Method::POST,
            Some(verb) => Method::from_str(&verb.to_ascii_uppercase())
                .map_err(|_| format!("{label}: `{verb}` is not an HTTP method"))?,
        };
        let headers =
            HeaderTemplates::compile(&http.headers).map_err(|why| format!("{label}: {why}"))?;
        if let Some(body) = &http.body {
            crate::mcp::hook::check_template("body", body)
                .map_err(|why| format!("{label}: {why}"))?;
        }
        // A plain URL is parsed now — a typo in a scheme belongs to startup, not
        // to the first tool call. A template can only be syntax-checked here;
        // what it produces is a URL or a hook failure, per firing.
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
        // Compiled here rather than matched as text on every call: a pattern that
        // does not parse is a hook covering nothing, and finding that out on the
        // first `tools/call` is finding it out too late to matter.
        let tools = config
            .tools
            .iter()
            .map(|pattern| NamePattern::compile(pattern))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|why| format!("{label}: `tools`: {why}"))?;
        let files = http
            .files
            .iter()
            .map(|pattern| NamePattern::compile(pattern))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|why| format!("{label}: `files`: {why}"))?;

        hooks.push(Hook {
            name: config.name.clone(),
            phases,
            tools,
            when_defined: config.when_defined.clone(),
            on_error: config.on_error,
            action: HookAction::Http(HttpAction {
                url,
                method,
                auth: http.auth.clone(),
                headers,
                body: http.body.clone(),
                files,
                timeout: Duration::from_millis(http.timeout_ms),
            }),
        });
    }

    Ok(hooks)
}

/// One hook, for the UI. Names only, never a rendered value.
fn describe(hook: &Hook) -> HookDescriptor {
    let HookAction::Http(http) = &hook.action;
    HookDescriptor {
        name: hook.name.clone(),
        on: hook.phases.iter().copied().collect(),
        // The patterns as written. The compiled form carries anchors this added,
        // and a UI showing those would be quoting something nobody typed.
        tools: hook.tools.iter().map(|p| p.as_str().to_owned()).collect(),
        when_defined: hook.when_defined.clone(),
        on_error: hook.on_error,
        action: hook.kind().to_owned(),
        // As written, template and all: a UI showing a rendered URL would be
        // showing one firing of many, and this listing belongs to none of them.
        url: http.url.source().to_owned(),
        auth: http.auth.clone(),
        headers: http.headers.names().map(str::to_owned).collect(),
        files: http.files.iter().map(|p| p.as_str().to_owned()).collect(),
        uses_auth: hook.header_providers().map(str::to_owned).collect(),
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
        action:
          kind: http
          url: https://audit.internal/events
          auth: workload
          headers:
            x-source: mire
";

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
        assert_eq!(hook.auth(), Some("workload"));
        assert_eq!(hook.url().source(), "https://audit.internal/events");

        let HookAction::Http(http) = &hook.action;
        assert_eq!(http.method, Method::POST);
        assert_eq!(http.timeout, Duration::from_millis(DEFAULT_HOOK_TIMEOUT_MS));
        assert!(http.body.is_none());
    }

    #[test]
    fn a_hook_is_advertised_by_name_and_never_by_value() {
        let dir = write("hook-descriptor", WITH_HOOK);
        let registry = McpRegistry::load(&dir, &Client::new());

        let hooks = &registry.descriptors()[0].hooks;
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].name, "audit");
        assert_eq!(hooks[0].action, "http");
        assert_eq!(hooks[0].on, vec![HookPhase::Before, HookPhase::After]);
        // The header's name travels; its rendered value never does.
        assert_eq!(hooks[0].headers, vec!["x-source".to_owned()]);
    }

    #[test]
    fn a_hook_that_fires_on_nothing_is_a_webhook_nobody_will_ever_get() {
        let dir = write(
            "hook-no-phase",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: []\n        action:\n          kind: http\n          url: https://audit.internal/events\n",
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
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: [before]\n        action:\n          kind: http\n          url: https://one.internal/e\n      - name: audit\n        on: [after]\n        action:\n          kind: http\n          url: https://two.internal/e\n",
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
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: [after]\n        action:\n          kind: http\n          url: https://audit.internal/{{ vars.session }}\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let hook = &registry.get("files").unwrap().server().hooks[0];
        assert!(matches!(hook.url(), HookUrl::Template(_)));
        // As written, template and all: what it renders to belongs to a firing.
        assert_eq!(
            hook.url().source(),
            "https://audit.internal/{{ vars.session }}"
        );
    }

    #[test]
    fn a_hook_loads_the_variables_it_waits_for() {
        let dir = write(
            "hook-when-defined",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        \
             on: [after]\n        when_defined: [session, job]\n        action:\n          kind: http\n          \
             url: https://audit.internal/events\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let hook = &registry.get("files").unwrap().server().hooks[0];
        assert_eq!(hook.when_defined, vec!["session", "job"]);

        // And it is advertised, because "what is this about to do" has to cover
        // a hook that will sit out most of a run.
        let advertised = &registry.descriptors()[0].hooks[0];
        assert_eq!(advertised.when_defined, vec!["session", "job"]);
    }

    #[test]
    fn a_hook_that_waits_for_nothing_is_the_ordinary_case() {
        let dir = write("hook-no-condition", WITH_HOOK);
        let registry = McpRegistry::load(&dir, &Client::new());

        let hook = &registry.get("files").unwrap().server().hooks[0];
        assert!(hook.when_defined.is_empty());
    }

    #[test]
    fn a_url_that_is_not_a_url_and_not_a_template_is_caught_at_startup() {
        let dir = write(
            "hook-url-bad",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: [after]\n        action:\n          kind: http\n          url: audit.internal/events\n",
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
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: [after]\n        action:\n          kind: http\n          url: https://audit.internal/{{ vars.session\n",
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
    fn a_body_template_that_does_not_parse_is_caught_at_startup() {
        let dir = write(
            "hook-body",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: [before]\n        action:\n          kind: http\n          url: https://audit.internal/e\n          body: '{{ unclosed'\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("audit"));
        assert!(registry.issues()[0].message.contains("body"));
    }

    #[test]
    fn a_files_pattern_loads_and_a_broken_one_is_caught_at_startup() {
        let dir = write(
            "hook-files",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: [before]\n        action:\n          kind: http\n          url: https://audit.internal/e\n          files:\n            - '.*\\.pdf'\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let hooks = &registry.get("files").unwrap().server().hooks;
        let HookAction::Http(http) = &hooks[0].action;
        assert_eq!(http.files.len(), 1);
        assert_eq!(http.files[0].as_str(), r".*\.pdf");
        // Names only in the descriptor, the same as the headers beside them.
        assert_eq!(registry.descriptors()[0].hooks[0].files, vec![r".*\.pdf"]);

        let broken = write(
            "hook-files-broken",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: [before]\n        action:\n          kind: http\n          url: https://audit.internal/e\n          files:\n            - '*.pdf'\n",
        );
        let registry = McpRegistry::load(&broken, &Client::new());
        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("`files`"));
        assert!(registry.issues()[0].message.contains("*.pdf"));
    }

    #[test]
    fn a_tool_pattern_that_is_not_a_regex_is_caught_at_startup_too() {
        let dir = write(
            "hook-tools",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: gate\n        on: [before]\n        tools:\n          - 'write_('\n        action:\n          kind: http\n          url: https://policy.internal/decide\n",
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
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: [before]\n        action:\n          kind: http\n          url: https://audit.internal/e\n          method: 'not a verb'\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("not an HTTP method"));
    }

    #[test]
    fn an_action_kind_nobody_implements_names_itself() {
        let dir = write(
            "hook-kind",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    hooks:\n      - name: audit\n        on: [before]\n        action:\n          kind: carrier_pigeon\n",
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
