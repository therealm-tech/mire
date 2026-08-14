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
use reqwest::header::HeaderName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::debug;
use url::Url;

use super::client::{McpClient, McpServer, UploadBody, UploadMethod, UploadTarget};
use super::headers::HeaderTemplates;
use crate::issue::LoadIssue;
use crate::profile::JsonPathExpr;

/// File declaring the MCP servers, in the profiles directory.
pub const MCP_REGISTRY_FILE: &str = "mcp.yaml";

/// Default per-request timeout. Generous: a real tool does real work.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Multipart field an upload goes out under, when the entry does not say.
const DEFAULT_UPLOAD_FIELD: &str = "file";

/// Where the identifier is read from in the upload's answer, by default.
///
/// A cascade rather than one path, for the same reason a profile's `decode:` is
/// one: these are four spellings of a single field, and trying them in order is
/// what lets most targets be declared with a `url:` and nothing else. It is a
/// guess and it says so — a target that spells it otherwise writes its own `id:`,
/// and one answering a batch needs a wildcard anyway.
const DEFAULT_UPLOAD_ID_PATHS: [&str; 4] = ["$.id", "$.fileId", "$.file_id", "$.data.id"];

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
    /// Where a file goes before a tool can be pointed at it, if anywhere.
    ///
    /// Present is the whole signal the UI needs: a server with an upload target
    /// can be handed a file, one without it cannot, and offering the choice
    /// either way would be offering something that cannot work.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upload: Option<UploadDescriptor>,
}

/// An upload target, as advertised to the UI. Carries no credential.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct UploadDescriptor {
    /// Where the bytes go. Shown so you can see it before sending any.
    pub url: String,
    /// The method they go out as.
    pub method: String,
    /// `multipart` or `raw`, which is also how many requests a batch takes.
    ///
    /// How many that is, is `mire`'s problem rather than a client's: the browser
    /// hands over the whole batch either way and this decides what happens on
    /// the way out. It is here to be *read*, not to be branched on.
    pub body: String,
    /// Where the identifiers are read back from, in the order they are tried.
    pub id: Vec<String>,
    /// The response header they are read from instead, when the entry names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id_header: Option<String>,
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

            match build_server(config) {
                Ok((server, descriptor)) => {
                    debug!(name = %server.name, url = %server.url, "MCP server registered");
                    registry.descriptors.push(descriptor);
                    registry
                        .clients
                        .insert(server.name.clone(), McpClient::new(server, http.clone()));
                }
                Err(message) => registry.issues.push(LoadIssue::new(&path, message)),
            }
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
    /// Where to put a file this server's tools are then pointed at.
    #[serde(default)]
    upload: Option<UploadConfig>,
}

/// An upload API sitting next to an MCP server.
///
/// Declared **on the server** rather than on its own, and that placement is the
/// point: whatever the tools are going to read the file back as, they will do it
/// as whoever this server authenticates as. A separate top-level block would
/// make the two identities a coincidence of configuration; here they cannot come
/// apart, because there is only one `auth:` and it is the server's.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UploadConfig {
    /// Where the bytes go.
    url: Url,
    /// The method to send them with. `post` creates, `put` writes to a known
    /// location — which is what a pre-signed URL is.
    #[serde(default)]
    method: UploadMethodConfig,
    /// How the bytes are shaped: a form field, or the body itself.
    #[serde(default)]
    body: UploadBodyConfig,
    /// Name of the multipart field carrying each file. `multipart` only.
    #[serde(default = "default_upload_field")]
    field: String,
    /// `JSONPath`s to the identifiers in the answer, tried in order.
    ///
    /// Paths rather than a field name because an upload API is free to nest its
    /// answer, and a *list* because this is the same question `decode:` answers
    /// for a model endpoint: which field of *this* body is the one that matters,
    /// given that four APIs spell it four ways.
    ///
    /// A target answering for several files at once needs a wildcard —
    /// `$.files[*].id` — because the count has to match what went out.
    #[serde(default = "default_upload_id_paths")]
    id: Vec<JsonPathExpr>,
    /// A response header carrying the identifier instead, for the targets that
    /// answer `201` and an empty body.
    #[serde(default)]
    id_header: Option<String>,
}

/// `method:` as it is written in the file.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UploadMethodConfig {
    #[default]
    Post,
    Put,
}

/// `body:` as it is written in the file.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UploadBodyConfig {
    #[default]
    Multipart,
    Raw,
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_upload_field() -> String {
    DEFAULT_UPLOAD_FIELD.to_owned()
}

/// # Panics
///
/// Never in practice: the constants are literals this crate's tests parse.
fn default_upload_id_paths() -> Vec<JsonPathExpr> {
    DEFAULT_UPLOAD_ID_PATHS
        .iter()
        .map(|path| path.parse().expect("a default id path is a valid JSONPath"))
        .collect()
}

/// Turns one entry into a server and the descriptor the UI reads.
///
/// Separate from [`McpRegistry::load`] because the two do different jobs: that
/// one walks a file and collects what went wrong, this one turns exactly one
/// entry into something usable. Every failure is a sentence naming the server,
/// which is what a reader needs to find the four lines to fix.
fn build_server(config: ServerConfig) -> Result<(McpServer, McpDescriptor), String> {
    let name = config.name.clone();
    let headers = HeaderTemplates::compile(&config.headers)
        .map_err(|message| format!("MCP server `{name}`: {message}"))?;

    let protocol_version = match config.protocol_version.as_deref().map(str::parse) {
        None => None,
        Some(Ok(revision)) => Some(revision),
        Some(Err(message)) => return Err(format!("MCP server `{name}`: {message}")),
    };

    let upload = config
        .upload
        .map(|upload| build_upload(&name, upload))
        .transpose()?;

    let declared: Vec<String> = config.headers.keys().cloned().collect();
    let server = McpServer {
        name: config.name,
        url: config.url,
        auth: config.auth,
        tools: config.tools,
        headers,
        timeout: Duration::from_millis(config.timeout_ms),
        protocol_version,
        upload,
    };

    let descriptor = McpDescriptor {
        name: server.name.clone(),
        url: server.url.to_string(),
        auth: server.auth.clone(),
        tools: server.tools.clone(),
        headers: declared,
        uses_auth: server.headers.providers().map(str::to_owned).collect(),
        upload: server.upload.as_ref().map(|upload| UploadDescriptor {
            url: upload.url.to_string(),
            method: upload.method.as_str().to_owned(),
            body: match &upload.body {
                UploadBody::Multipart { .. } => "multipart".to_owned(),
                UploadBody::Raw => "raw".to_owned(),
            },
            id: upload
                .id
                .iter()
                .map(|path| path.source().to_owned())
                .collect(),
            id_header: upload.id_header.as_ref().map(ToString::to_string),
        }),
    };
    Ok((server, descriptor))
}

/// Turns one `upload:` block into a target, or says why it cannot.
///
/// Contradictions are refused rather than quietly resolved. `field:` under a raw
/// body is the case worth spelling out: it reads as configured and does nothing,
/// which is the shape of bug somebody loses an afternoon to.
fn build_upload(server: &str, config: UploadConfig) -> Result<UploadTarget, String> {
    let named_field = config.field != DEFAULT_UPLOAD_FIELD;
    let body = match config.body {
        UploadBodyConfig::Multipart => UploadBody::Multipart {
            field: config.field,
        },
        UploadBodyConfig::Raw if named_field => {
            return Err(format!(
                "MCP server `{server}`: `field: {}` has no meaning under `body: raw`, \
                 where the file is the body and there is no form to put a field in",
                config.field
            ));
        }
        UploadBodyConfig::Raw => UploadBody::Raw,
    };

    let id_header =
        match config.id_header {
            None => None,
            Some(name) => Some(HeaderName::try_from(&name).map_err(|_| {
                format!("MCP server `{server}`: `{name}` is not a valid header name")
            })?),
        };

    if config.id.is_empty() {
        return Err(format!(
            "MCP server `{server}`: `id:` is empty, so there is nowhere to read an identifier from"
        ));
    }

    Ok(UploadTarget {
        url: config.url,
        method: match config.method {
            UploadMethodConfig::Post => UploadMethod::Post,
            UploadMethodConfig::Put => UploadMethod::Put,
        },
        body,
        id: config.id,
        id_header,
    })
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
    fn a_server_with_no_upload_block_has_nowhere_to_put_a_file() {
        let dir = write(
            "no-upload",
            "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.get("files").unwrap().server().upload.is_none());
        // The UI reads the descriptor, not the client, and offers the shape on
        // the strength of this being present.
        assert!(registry.descriptors()[0].upload.is_none());
    }

    #[test]
    fn an_upload_target_needs_only_a_url() {
        let dir = write(
            "upload",
            "servers:\n  - name: files\n    url: https://files.internal/mcp\n    upload:\n      url: https://files.internal/v1/documents\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let upload = registry.get("files").unwrap().server().upload.clone();
        let upload = upload.expect("declared");
        assert_eq!(upload.url.as_str(), "https://files.internal/v1/documents");
        // The defaults are the shape most upload APIs have, and every one of them
        // is overridable — none is what this tool believes an upload API *is*.
        assert_eq!(upload.method, UploadMethod::Post);
        assert_eq!(
            upload.body,
            UploadBody::Multipart {
                field: "file".to_owned()
            }
        );
        assert!(upload.id_header.is_none());
        // A cascade, so `id`, `fileId`, `file_id` and `data.id` are all read
        // without anybody having to say which one their target uses.
        let tried: Vec<&str> = upload.id.iter().map(JsonPathExpr::source).collect();
        assert_eq!(tried, ["$.id", "$.fileId", "$.file_id", "$.data.id"]);
    }

    /// A pre-signed `PUT` is the other half of the world, and nothing about it
    /// looks like a form.
    #[test]
    fn a_raw_target_puts_the_file_in_the_body() {
        let dir = write(
            "upload-raw",
            "servers:\n  - name: bucket\n    url: https://bucket.internal/mcp\n    upload:\n      url: https://bucket.internal/objects\n      method: put\n      body: raw\n      id_header: location\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let upload = registry
            .get("bucket")
            .unwrap()
            .server()
            .upload
            .clone()
            .expect("declared");
        assert_eq!(upload.method, UploadMethod::Put);
        assert_eq!(upload.body, UploadBody::Raw);
        assert_eq!(
            upload.id_header.as_ref().map(HeaderName::as_str),
            Some("location")
        );

        // Reported as it was written, so `GET /api/mcp` answers "what is this
        // server about to do" without anybody opening the file.
        let descriptor = registry.descriptors()[0].upload.clone().expect("declared");
        assert_eq!(descriptor.body, "raw");
        assert_eq!(descriptor.method, "PUT");
    }

    #[test]
    fn a_target_can_name_its_own_field_and_paths() {
        let dir = write(
            "upload-named",
            "servers:\n  - name: files\n    url: https://files.internal/mcp\n    upload:\n      url: https://files.internal/v1/documents\n      field: document\n      id:\n        - $.documents[*].ref\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let upload = registry
            .get("files")
            .unwrap()
            .server()
            .upload
            .clone()
            .expect("declared");
        assert_eq!(
            upload.body,
            UploadBody::Multipart {
                field: "document".to_owned()
            }
        );
        assert_eq!(
            registry.descriptors()[0].upload.as_ref().unwrap().body,
            "multipart"
        );
        assert_eq!(
            registry.descriptors()[0].upload.as_ref().unwrap().id,
            ["$.documents[*].ref"]
        );
    }

    /// Configured and doing nothing is the shape of bug somebody loses an
    /// afternoon to, so it is refused with the reason rather than ignored.
    #[test]
    fn a_form_field_under_a_raw_body_is_a_contradiction_and_says_so() {
        let dir = write(
            "upload-contradiction",
            "servers:\n  - name: bucket\n    url: https://bucket.internal/mcp\n    upload:\n      url: https://bucket.internal/objects\n      body: raw\n      field: document\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        let message = &registry.issues()[0].message;
        assert!(message.contains("field"), "{message}");
        assert!(message.contains("raw"), "{message}");
        assert!(registry.get("bucket").is_none());
    }

    #[test]
    fn a_path_that_does_not_parse_names_the_file_rather_than_failing_at_call_time() {
        let dir = write(
            "upload-bad-path",
            "servers:\n  - name: files\n    url: https://files.internal/mcp\n    upload:\n      url: https://files.internal/v1/documents\n      id:\n        - 'not a path'\n",
        );
        let registry = McpRegistry::load(&dir, &Client::new());

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.get("files").is_none());
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
