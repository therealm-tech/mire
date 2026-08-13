//! `mcp.yaml`, sitting next to the profiles and `auth.yaml`.
//!
//! A server is declared once and referenced by name, for the same reason auth is:
//! the same server is worth pointing several profiles at, and worth replaying
//! across auth modes without duplicating anything.
//!
//! Same loading policy as everywhere else — a bad entry is reported and skipped,
//! the rest still work.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use reqwest::Client;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::debug;
use url::Url;

use super::client::{McpClient, McpServer};
use super::headers::HeaderTemplates;
use crate::issue::LoadIssue;

/// File declaring the MCP servers, in the profiles directory.
pub const MCP_REGISTRY_FILE: &str = "mcp.yaml";

/// Default per-request timeout. Generous: a real tool does real work.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

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

            let server = McpServer {
                name: config.name.clone(),
                url: config.url,
                auth: config.auth,
                tools: config.tools,
                headers,
                timeout: Duration::from_millis(config.timeout_ms),
                protocol_version,
            };

            debug!(name = %server.name, url = %server.url, "MCP server registered");
            registry.descriptors.push(McpDescriptor {
                name: server.name.clone(),
                url: server.url.to_string(),
                auth: server.auth.clone(),
                tools: server.tools.clone(),
                headers: config.headers.keys().cloned().collect(),
                uses_auth: server.headers.providers().map(str::to_owned).collect(),
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
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
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
