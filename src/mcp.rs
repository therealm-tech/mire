//! Model Context Protocol, client side.
//!
//! Agent mode can answer a model's tool calls two ways. Simulated tools
//! ([`crate::profile::ToolSpec`]) prove the model *emits* well-formed calls and
//! knows what to do with a result — deterministic, no dependency, nothing
//! executed. This module is the other half: real tools, on a real server, with
//! real effects.
//!
//! # Why this is hand-rolled
//!
//! Revision `2026-07-28` removed the `initialize` handshake, protocol-level
//! sessions, the standalone GET stream and SSE resumability. What is left, for a
//! client that wants `tools/list` and `tools/call`, is a `POST` of JSON-RPC with
//! three headers. `rmcp` would bring its own transport stack; going through its
//! transport-agnostic layer means writing the adapter anyway.
//!
//! Owning it keeps every call on [`crate::transport`]'s client — so `--ca-bundle`,
//! the proxy settings and the redirect policy apply to an MCP server exactly as
//! they do to a model endpoint, and so does the whole auth registry. Pointing the
//! same MCP server at `anonymous`, a token and a workload identity in turn is the
//! same move this tool exists for.

pub mod auth;
pub mod client;
pub mod headers;
pub mod registry;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use auth::McpCredentials;
pub use client::{McpClient, McpServer};
pub use headers::HeaderTemplates;
pub use registry::McpRegistry;

/// The revision this client speaks.
///
/// Sent both as the `MCP-Protocol-Version` header and inside `params._meta`; a
/// server rejects the request when they disagree, so they are built from this one
/// constant rather than written twice.
pub const PROTOCOL_VERSION: &str = "2026-07-28";

/// A tool as the server describes it.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpTool {
    /// Identifier the model calls.
    pub name: String,
    /// Human-readable name, when the server bothers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// What it does. Handed to the model as-is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema for the arguments.
    #[serde(default)]
    pub input_schema: Value,
    /// Behavioural hints (`readOnlyHint`, `destructiveHint`, …). Reported, never
    /// enforced: they are the server's own claim about itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<Value>,
    /// Which server it came from, so a name collision is traceable.
    #[serde(default)]
    pub server: String,
}

impl McpTool {
    /// Whether the server claims this tool changes nothing.
    ///
    /// Surfaced in the UI so a run against a live server is not a leap of faith.
    /// A claim, not a guarantee — hence "hint" in the specification.
    #[must_use]
    pub fn read_only(&self) -> Option<bool> {
        self.annotations.as_ref()?.get("readOnlyHint")?.as_bool()
    }

    /// Whether the server admits this tool can destroy something.
    #[must_use]
    pub fn destructive(&self) -> Option<bool> {
        self.annotations.as_ref()?.get("destructiveHint")?.as_bool()
    }
}

/// What a `tools/call` produced.
#[derive(Debug, Clone)]
pub struct ToolResult {
    /// Content blocks, flattened to something a model can read.
    pub text: String,
    /// `structuredContent`, when the server sent it.
    pub structured: Option<Value>,
    /// The server's own `isError`. **Not** a transport failure: the tool ran and
    /// reported a problem, which is a result the model is meant to see and react
    /// to, exactly like a `4xx` from an endpoint under test.
    pub is_error: bool,
    /// Round trip, in milliseconds.
    pub latency_ms: u64,
}

/// Why an MCP exchange could not produce a result.
///
/// A tool that ran and failed is not here — that is [`ToolResult::is_error`].
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// The profile names a server that `mcp.yaml` does not declare.
    #[error("unknown MCP server `{0}`")]
    UnknownServer(String),

    /// The server could not be reached, or answered nothing usable.
    #[error("MCP server `{server}`: {message}")]
    Transport {
        /// Registry name of the server.
        server: String,
        /// What went wrong, scrubbed of credentials.
        message: String,
    },

    /// The server answered a JSON-RPC error.
    #[error("MCP server `{server}`: {method} failed ({code}) — {message}")]
    Rpc {
        /// Registry name of the server.
        server: String,
        /// The method that failed.
        method: String,
        /// JSON-RPC error code.
        code: i64,
        /// Its message.
        message: String,
    },

    /// The answer did not parse, or was not the shape the revision defines.
    #[error("MCP server `{server}`: {message}")]
    Protocol {
        /// Registry name of the server.
        server: String,
        /// What was wrong with it.
        message: String,
    },

    /// The server asked for input mid-call (`resultType: "input_required"`).
    ///
    /// A harness has nobody to ask. Saying so beats reporting an empty result or
    /// looping: the call did not fail, it is *unfinishable here*.
    #[error(
        "MCP server `{server}`: `{tool}` needs interactive input ({requests}), which a test harness cannot provide"
    )]
    InputRequired {
        /// Registry name of the server.
        server: String,
        /// The tool that asked.
        tool: String,
        /// What it asked for, by method name.
        requests: String,
    },

    /// A templated header could not be produced.
    ///
    /// Never carries the value it failed to render — only what was asked for.
    #[error("MCP server `{server}`: header `{header}`: {message}")]
    Header {
        /// Registry name of the server.
        server: String,
        /// The header that could not be built.
        header: String,
        /// Why, with the template's own words rather than any value.
        message: String,
    },

    /// Credentials could not be produced for the server.
    #[error(transparent)]
    Auth(#[from] crate::auth::AuthError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annotations_are_read_but_never_assumed() {
        let mut tool = McpTool {
            name: "rm".to_owned(),
            title: None,
            description: None,
            input_schema: Value::Null,
            annotations: None,
            server: "fs".to_owned(),
        };
        // A server that says nothing is not a server that promises anything.
        assert_eq!(tool.read_only(), None);
        assert_eq!(tool.destructive(), None);

        tool.annotations = Some(serde_json::json!({"destructiveHint": true}));
        assert_eq!(tool.destructive(), Some(true));
        assert_eq!(tool.read_only(), None);
    }
}
