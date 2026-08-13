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
//! # Speaking more than one revision
//!
//! Pointing `mire` at a server on an older revision used to produce a bare `400`
//! and nothing else to go on, because the version was a constant. It is now a
//! [`Revision`], negotiated per server and cached — see [`negotiate`].
//!
//! The three supported revisions share one endpoint and one `POST`, which is why
//! they fit behind a single client. What separates them is small and mechanical:
//! the two older ones open with `initialize` and carry a session, the newest one
//! has neither and mirrors selected body fields into headers instead.
//!
//! Owning it keeps every call on [`crate::transport`]'s client — so `--ca-bundle`,
//! the proxy settings and the redirect policy apply to an MCP server exactly as
//! they do to a model endpoint, and so does the whole auth registry. Pointing the
//! same MCP server at `anonymous`, a token and a workload identity in turn is the
//! same move this tool exists for.

pub mod auth;
pub mod client;
pub mod headers;
pub mod negotiate;
pub mod registry;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use auth::McpCredentials;
pub use client::{McpClient, McpServer};
pub use headers::HeaderTemplates;
pub use negotiate::Session;
pub use registry::McpRegistry;

/// One JSON-RPC round trip with an MCP server, as it happened.
///
/// The tool calls a run makes are only half of what it says to a server. The
/// discovery probe, the handshake and `tools/list` are the other half — and when
/// a server refuses the run before a single tool is called, they are the *only*
/// half, which is precisely when somebody needs to read them. A tool that never
/// ran because `initialize` came back `401` is not a model problem, and nothing
/// in a tool-call listing could ever say so.
///
/// Recorded whatever happens, including a request that never got an answer at
/// all: that is the most informative entry this can hold.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct McpExchange {
    /// Registry name of the server.
    pub server: String,
    /// The endpoint it went to.
    pub url: String,
    /// JSON-RPC method: `server/discover`, `initialize`, `tools/list`, …
    pub method: String,
    /// The revision it went out on, which is not always the one that ends up in
    /// force — the probes are how that gets settled.
    pub revision: Revision,
    /// A notification carries no `id` and expects no answer.
    pub notification: bool,
    /// Request headers, masked.
    pub headers: BTreeMap<String, String>,
    /// The JSON-RPC request body, masked.
    pub request: String,
    /// HTTP status, or `0` when the request never reached a server.
    pub status: u16,
    /// Whether the answer arrived as an event stream rather than one object.
    pub streaming: bool,
    /// The response body, masked. Empty when nothing came back.
    pub response: String,
    /// Round trip, in milliseconds.
    pub latency_ms: u64,
    /// Why there is no response, when there is none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Where one run collects the MCP exchanges it produced.
///
/// A plain `std::sync::Mutex` rather than tokio's: every critical section is a
/// `push` onto a `Vec` with no `await` inside it, so there is nothing to hold
/// across a yield point.
pub type McpJournal = Arc<Mutex<Vec<McpExchange>>>;

/// Takes everything recorded so far, leaving the journal empty.
///
/// A poisoned lock yields nothing rather than panicking: losing the record of a
/// run is not a reason to fail the run it is recording.
#[must_use]
pub fn drain(journal: &McpJournal) -> Vec<McpExchange> {
    journal
        .lock()
        .map(|mut entries| std::mem::take(&mut *entries))
        .unwrap_or_default()
}

/// A revision of the Streamable HTTP transport `mire` can speak.
///
/// Ordered oldest to newest, so `Ord` means "is newer than" and choosing the best
/// revision two parties share is a `max()` over the intersection.
///
/// All three are one endpoint and one `POST` per request. Everything that differs
/// between them is exposed as a method below, so the client asks the revision what
/// to do rather than matching on it in five places.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Revision {
    /// Streamable HTTP as introduced: `initialize`, `Mcp-Session-Id`.
    #[serde(rename = "2025-03-26")]
    V20250326,
    /// Adds the `MCP-Protocol-Version` header on every post-handshake request.
    #[serde(rename = "2025-06-18")]
    V20250618,
    /// Drops the handshake and the session; adds mirrored headers.
    #[serde(rename = "2026-07-28")]
    V20260728,
}

impl Revision {
    /// Every revision this client speaks, oldest first.
    pub const ALL: [Self; 3] = [Self::V20250326, Self::V20250618, Self::V20260728];

    /// The newest one, which is what `mire` prefers and proposes first.
    pub const LATEST: Self = Self::V20260728;

    /// The newest revision that still opens with a handshake.
    ///
    /// What `initialize` proposes when discovery got us nowhere: a server that
    /// speaks something older answers with the older version rather than failing,
    /// which is the whole point of that handshake.
    pub const LATEST_LEGACY: Self = Self::V20250618;

    /// The wire spelling, which is also what the specification calls it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V20250326 => "2025-03-26",
            Self::V20250618 => "2025-06-18",
            Self::V20260728 => "2026-07-28",
        }
    }

    /// Whether the revision opens with `initialize` and carries a session.
    ///
    /// The dividing line between the two transports, and the only structural
    /// difference: everything else is which headers go on a request.
    #[must_use]
    pub const fn handshakes(self) -> bool {
        !matches!(self, Self::V20260728)
    }

    /// Whether selected body fields are mirrored into `Mcp-Method`, `Mcp-Name`
    /// and `Mcp-Param-*`.
    ///
    /// Only the newest revision, and sending them to an older server is not
    /// harmless: it is unsolicited routing metadata an intermediary may act on.
    #[must_use]
    pub const fn mirrors_headers(self) -> bool {
        matches!(self, Self::V20260728)
    }

    /// Whether requests carry the `MCP-Protocol-Version` header.
    ///
    /// `2025-03-26` predates it, and a server from that revision is entitled to
    /// reject a header it never defined.
    #[must_use]
    pub const fn sends_version_header(self) -> bool {
        !matches!(self, Self::V20250326)
    }

    /// Whether the revision defines `server/discover`.
    ///
    /// Discovery is itself a method of the newest revision, which is why probing
    /// with it cannot be the only step — see [`negotiate`].
    #[must_use]
    pub const fn discoverable(self) -> bool {
        matches!(self, Self::V20260728)
    }
}

impl fmt::Display for Revision {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for Revision {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|revision| revision.as_str() == text)
            .ok_or_else(|| {
                let known: Vec<_> = Self::ALL.iter().rev().map(|r| r.as_str()).collect();
                format!(
                    "unknown MCP revision `{text}`; this build speaks {}",
                    known.join(", ")
                )
            })
    }
}

/// `JsonSchema` by hand: the derive would publish the Rust variant names, and
/// this type crosses the API as its wire spelling.
impl JsonSchema for Revision {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Revision".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "string",
            "enum": Self::ALL.iter().copied().map(Self::as_str).collect::<Vec<_>>(),
            "description": "An MCP Streamable HTTP revision, as the specification spells it.",
        })
    }
}

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

    /// The server and this build share no revision.
    ///
    /// The failure the whole negotiation exists to report: before it, this was a
    /// bare `400` with the version buried in a header nobody printed.
    #[error(
        "MCP server `{server}`: no revision in common — `mire` speaks {ours}, the server offers {theirs}"
    )]
    NoCommonRevision {
        /// Registry name of the server.
        server: String,
        /// What this build can speak, newest first.
        ours: String,
        /// What the server said it speaks, in its own words.
        theirs: String,
    },

    /// The server no longer knows the session we were given.
    ///
    /// Retried once, transparently, because a restarted server is not a failure
    /// worth reporting. Surfaces only when it happens again immediately after —
    /// which is a server losing sessions faster than they can be established.
    #[error("MCP server `{server}`: the {revision} session was rejected twice in a row")]
    SessionLost {
        /// Registry name of the server.
        server: String,
        /// The revision whose session was lost.
        revision: String,
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

    #[test]
    fn revisions_order_oldest_to_newest() {
        // The ordering is load-bearing: picking the best revision two parties
        // share is a `max()` over the intersection, and nothing else.
        let mut all = Revision::ALL;
        all.sort_unstable();
        assert_eq!(all, Revision::ALL);
        assert_eq!(Revision::ALL.iter().copied().max(), Some(Revision::LATEST));
        assert!(Revision::LATEST_LEGACY < Revision::LATEST);
    }

    #[test]
    fn every_revision_round_trips_through_its_wire_spelling() {
        for revision in Revision::ALL {
            assert_eq!(revision.as_str().parse::<Revision>(), Ok(revision));
            assert_eq!(revision.to_string(), revision.as_str());
        }
    }

    #[test]
    fn an_unknown_revision_says_what_this_build_speaks() {
        let error = "1999-01-01".parse::<Revision>().unwrap_err();
        assert!(error.contains("1999-01-01"), "{error}");
        // Newest first, because that is the one a reader is looking for.
        assert!(
            error.contains("2026-07-28, 2025-06-18, 2025-03-26"),
            "{error}"
        );
    }

    #[test]
    fn the_handshake_is_what_separates_the_two_transports() {
        assert!(!Revision::LATEST.handshakes());
        assert!(Revision::V20250618.handshakes());
        assert!(Revision::V20250326.handshakes());

        // Mirrored headers are unsolicited routing metadata anywhere else.
        assert!(Revision::LATEST.mirrors_headers());
        assert!(!Revision::V20250618.mirrors_headers());

        // `MCP-Protocol-Version` postdates the oldest revision we speak.
        assert!(!Revision::V20250326.sends_version_header());
        assert!(Revision::V20250618.sends_version_header());

        // Discovery is a method of the newest revision, so it cannot be the only
        // probe — that asymmetry is the reason `negotiate` has a ladder.
        assert!(Revision::LATEST.discoverable());
        assert!(!Revision::V20250618.discoverable());
    }

    #[test]
    fn a_revision_serialises_as_the_specification_spells_it() {
        let json = serde_json::to_string(&Revision::V20250618).expect("serialise");
        assert_eq!(json, "\"2025-06-18\"");
        let back: Revision = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, Revision::V20250618);
    }
}
