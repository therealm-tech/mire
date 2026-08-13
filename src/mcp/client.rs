//! Streamable HTTP transport, for every revision `mire` speaks.
//!
//! One `POST` per request, to one endpoint. The answer is either a JSON object or
//! an SSE stream whose last event carries the response; a client has to accept
//! both, so both are handled here and collapsed into a single value. That much is
//! true of all three revisions, which is why they fit behind one client.
//!
//! What the revision decides, and the reason [`Revision`] answers questions
//! instead of being matched on:
//!
//! * **`2026-07-28`** mirrors selected body fields into headers (`Mcp-Method`,
//!   `Mcp-Name`, and `Mcp-Param-*` for annotated parameters) so intermediaries
//!   can route without parsing bodies — and the server rejects the request with
//!   `-32020` if a header and the body disagree. A value that cannot be a plain
//!   ASCII header is carried base64 in a sentinel wrapper, which the server undoes
//!   before comparing. There is no handshake and no session.
//! * **`2025-06-18`** and **`2025-03-26`** open with `initialize`, carry the
//!   server's `Mcp-Session-Id` on every later request, and mirror nothing. The
//!   older of the two predates the `MCP-Protocol-Version` header and is not sent
//!   one.
//!
//! Which of them is in force is settled once per server by [`super::negotiate`]
//! and cached here.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::Client;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::sync::RwLock;
use tracing::{debug, warn};
use url::Url;

use super::headers::HeaderTemplates;
use super::negotiate::{self, Session};
use super::{McpError, McpExchange, McpJournal, McpTool, Revision, ToolResult};
use crate::auth::AuthProvider;

use super::McpCredentials;
use crate::redact::Redactor;

/// Pages of `tools/list` to follow before deciding a server is toying with us.
const MAX_PAGES: usize = 20;

/// The header carrying a session, on the revisions that have one.
const SESSION_HEADER: &str = "mcp-session-id";

/// A declared MCP server.
#[derive(Debug, Clone)]
pub struct McpServer {
    /// Registry name, referenced from a profile's `mcp:` list.
    pub name: String,
    /// The MCP endpoint. One URL, `POST` only.
    pub url: Url,
    /// Auth provider to authenticate with, by registry name.
    pub auth: Option<String>,
    /// Tools to offer the model. Empty means every tool the server advertises,
    /// which is the default: a server you point at is a server you meant.
    pub tools: Vec<String>,
    /// Extra headers, rendered on every request. Where a token that does not fit
    /// the `auth:` shapes goes.
    pub headers: HeaderTemplates,
    /// Per-request timeout.
    pub timeout: Duration,
    /// Revision to use, skipping negotiation entirely.
    ///
    /// `None` — the default — negotiates. Set it when the version is the thing
    /// under test: pinning one the server refuses produces the refusal, which is
    /// a legitimate thing to want to see from a tool whose job is to tell you
    /// what your endpoint does.
    pub protocol_version: Option<Revision>,
}

impl McpServer {
    /// Whether `tool` is one this server is allowed to offer.
    #[must_use]
    pub fn offers(&self, tool: &str) -> bool {
        self.tools.is_empty() || self.tools.iter().any(|allowed| allowed == tool)
    }
}

/// Talks to one MCP server.
///
/// Cloned freely — the registry hands out clones and a configuration reload
/// rebuilds the lot — so the negotiated state is shared rather than copied.
/// A reload deliberately drops it: the file may have repointed the server.
#[derive(Debug, Clone)]
pub struct McpClient {
    server: McpServer,
    http: Client,
    /// Settled on first use, then reused. Behind a lock because the API hands
    /// out `&McpClient` from an `Arc<Config>` snapshot and several requests can
    /// arrive at an unnegotiated server at once.
    session: Arc<RwLock<Option<Session>>>,
    /// Where to record what goes over the wire, when somebody is collecting.
    ///
    /// Per *run*, not per client: the registry's client is shared by everything
    /// in flight, so a journal living on it would mix two people's traffic
    /// together. [`recording`](Self::recording) is how one run gets its own.
    journal: Option<McpJournal>,
}

impl McpClient {
    /// Wraps a server definition around the shared HTTP client.
    #[must_use]
    pub fn new(server: McpServer, http: Client) -> Self {
        Self {
            server,
            http,
            session: Arc::new(RwLock::new(None)),
            journal: None,
        }
    }

    /// The same client, writing every exchange it makes into `journal`.
    ///
    /// The negotiated session is deliberately *shared* with the client this came
    /// from — it is the same server, and paying for a handshake per run to get a
    /// recording would change the thing being recorded.
    #[must_use]
    pub fn recording(&self, journal: McpJournal) -> Self {
        Self {
            journal: Some(journal),
            ..self.clone()
        }
    }

    /// The same server, told which revision to speak.
    ///
    /// `None` changes nothing, which is what "auto" means: the server keeps its
    /// own decision — negotiation, or the `protocol_version:` its entry pinned.
    ///
    /// A revision, unlike a journal, gets a *fresh* settled state rather than the
    /// shared one. Both directions matter: a revision chosen for one run must not
    /// become the revision every other run is speaking, and what the registry's
    /// client negotiated earlier must not decide this one either. That costs a
    /// handshake per run on the revisions that have one — which is the traffic you
    /// asked to see by choosing the revision in the first place.
    #[must_use]
    pub fn speaking(&self, revision: Option<Revision>) -> Self {
        let Some(revision) = revision else {
            return self.clone();
        };

        Self {
            server: McpServer {
                protocol_version: Some(revision),
                ..self.server.clone()
            },
            session: Arc::new(RwLock::new(None)),
            ..self.clone()
        }
    }

    /// Files one exchange, if anybody is collecting.
    ///
    /// A poisoned journal is dropped rather than propagated: the recording is not
    /// worth failing the run it exists to explain.
    fn file(&self, exchange: McpExchange) {
        if let Some(journal) = &self.journal
            && let Ok(mut entries) = journal.lock()
        {
            entries.push(exchange);
        }
    }

    /// The server this talks to.
    #[must_use]
    pub fn server(&self) -> &McpServer {
        &self.server
    }

    /// The revision in force, if one has been settled yet.
    ///
    /// `None` before the first call: negotiation costs a round trip and is not
    /// worth doing to populate a listing nobody asked to act on.
    pub async fn settled(&self) -> Option<Session> {
        self.session.read().await.clone()
    }

    /// The settled revision, negotiating on first use.
    ///
    /// # Errors
    ///
    /// Whatever [`negotiate`](super::negotiate::negotiate) could not resolve.
    pub async fn session(&self, credentials: &McpCredentials<'_>) -> Result<Session, McpError> {
        if let Some(session) = self.session.read().await.clone() {
            return Ok(session);
        }

        // Negotiating twice concurrently is wasteful but harmless, and holding
        // the write lock across two round trips would serialise every caller
        // behind the slowest server. Last writer wins; both wrote the same thing.
        let session = negotiate::negotiate(self, credentials).await?;

        debug!(
            server = %self.server.name,
            revision = %session.revision,
            settled = ?session.settled,
            "protocol revision in force"
        );
        *self.session.write().await = Some(session.clone());
        Ok(session)
    }

    /// Drops the settled state, so the next call negotiates again.
    ///
    /// Called when a server says the session is gone, and by nothing else: a
    /// revision does not change under a running server.
    async fn forget(&self) {
        *self.session.write().await = None;
    }

    /// Lists the tools on offer, following pagination.
    ///
    /// Tools whose `x-mcp-header` annotations are invalid are dropped with a
    /// warning rather than failing the listing — one malformed definition must
    /// not cost you every other tool on the server.
    ///
    /// # Errors
    ///
    /// Fails if the server cannot be reached, answers a JSON-RPC error, or sends
    /// something that is not a tool listing.
    pub async fn list_tools(
        &self,
        credentials: &McpCredentials<'_>,
    ) -> Result<Vec<McpTool>, McpError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;

        for _ in 0..MAX_PAGES {
            let mut params = Map::new();
            if let Some(value) = &cursor {
                params.insert("cursor".to_owned(), json!(value));
            }

            let result = self
                .call(
                    &Invocation {
                        method: "tools/list",
                        params: Value::Object(params),
                        name: None,
                        annotated: &[],
                        arguments: &Value::Null,
                    },
                    credentials,
                )
                .await?;

            let page: ToolsList =
                serde_json::from_value(result).map_err(|error| McpError::Protocol {
                    server: self.server.name.clone(),
                    message: format!("`tools/list` is not a tool listing: {error}"),
                })?;

            for tool in page.tools {
                match header_params(&tool.input_schema) {
                    Ok(_) if !self.server.offers(&tool.name) => {
                        debug!(server = %self.server.name, tool = %tool.name, "not in the server's tool list, skipped");
                    }
                    Ok(_) => tools.push(McpTool {
                        server: self.server.name.clone(),
                        ..tool
                    }),
                    Err(reason) => warn!(
                        server = %self.server.name,
                        tool = %tool.name,
                        %reason,
                        "tool rejected: invalid `x-mcp-header` annotation"
                    ),
                }
            }

            cursor = page.next_cursor;
            if cursor.is_none() {
                return Ok(tools);
            }
        }

        Err(McpError::Protocol {
            server: self.server.name.clone(),
            message: format!("`tools/list` still had more after {MAX_PAGES} pages"),
        })
    }

    /// Calls a tool for real.
    ///
    /// # Errors
    ///
    /// Fails on transport and protocol problems. A tool that ran and reported a
    /// problem comes back as `Ok` with [`ToolResult::is_error`] — that is a
    /// result, and the model is supposed to see it.
    pub async fn call_tool(
        &self,
        tool: &McpTool,
        arguments: &Value,
        credentials: &McpCredentials<'_>,
    ) -> Result<ToolResult, McpError> {
        let params = json!({ "name": tool.name, "arguments": arguments });

        // The server rejects a request whose mirrored headers disagree with the
        // body, so these are derived from exactly what is about to be sent — and
        // only on the revision that defines them, which is decided a layer down
        // because a lost session can put us through negotiation again.
        let annotated = header_params(&tool.input_schema).map_err(|reason| McpError::Protocol {
            server: self.server.name.clone(),
            message: format!("`{}` has an invalid `x-mcp-header`: {reason}", tool.name),
        })?;

        let started = Instant::now();
        let result = self
            .call(
                &Invocation {
                    method: "tools/call",
                    params,
                    name: Some(&tool.name),
                    annotated: &annotated,
                    arguments,
                },
                credentials,
            )
            .await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let result = result?;
        debug!(
            server = %self.server.name,
            tool = %tool.name,
            annotated = annotated.len(),
            latency_ms,
            "tool called"
        );

        let call: CallResult =
            serde_json::from_value(result).map_err(|error| McpError::Protocol {
                server: self.server.name.clone(),
                message: format!("`tools/call` answered something unusable: {error}"),
            })?;

        // A server that needs elicitation or sampling has asked a question no
        // harness can answer. Naming it beats an empty result.
        if call.result_type.as_deref() == Some("input_required") {
            return Err(McpError::InputRequired {
                server: self.server.name.clone(),
                tool: tool.name.clone(),
                requests: call.input_request_methods(),
            });
        }

        Ok(ToolResult {
            text: flatten(&call.content),
            structured: call.structured_content,
            is_error: call.is_error,
            latency_ms,
        })
    }

    /// One JSON-RPC call, with the revision settled and the session re-established
    /// if the server has forgotten it.
    async fn call(
        &self,
        invocation: &Invocation<'_>,
        credentials: &McpCredentials<'_>,
    ) -> Result<Value, McpError> {
        let session = self.session(credentials).await?;
        let first = self.attempt(&session, invocation, credentials).await;

        // The session is gone, not the revision: a server that restarted has
        // forgotten who we are, and the answer is to introduce ourselves again.
        // Exactly once — losing it twice in one call is a real problem, and
        // retrying forever would turn it into a hang.
        if matches!(first, Err(McpError::SessionLost { .. })) {
            warn!(
                server = %self.server.name,
                "the server no longer knows our session, negotiating again"
            );
            self.forget().await;
            let session = self.session(credentials).await?;
            return self.attempt(&session, invocation, credentials).await;
        }

        first
    }

    /// One attempt, with the headers the revision in force actually defines.
    async fn attempt(
        &self,
        session: &Session,
        invocation: &Invocation<'_>,
        credentials: &McpCredentials<'_>,
    ) -> Result<Value, McpError> {
        // Only the newest revision defines these, and computing them here rather
        // than at the call site is what lets a lost session put us through
        // negotiation again without the caller having to care.
        let mirrored = if session.revision.mirrors_headers() {
            mirror_headers(invocation.annotated, invocation.arguments)
        } else {
            Vec::new()
        };

        Ok(self
            .exchange(
                session,
                invocation.method,
                invocation.params.clone(),
                invocation.name,
                &mirrored,
                credentials,
            )
            .await?
            .result)
    }

    /// One JSON-RPC round trip, at an explicit protocol state.
    ///
    /// Visible to [`super::negotiate`], which has to make requests before there is
    /// a settled session to make them at.
    pub(super) async fn exchange(
        &self,
        session: &Session,
        method: &str,
        params: Value,
        name: Option<&str>,
        mirrored: &[(String, String)],
        credentials: &McpCredentials<'_>,
    ) -> Result<Exchange, McpError> {
        let sent = self
            .send(
                session,
                method,
                Some(1),
                params,
                name,
                mirrored,
                credentials,
            )
            .await?;

        let envelope = if sent.streaming {
            last_event(&sent.body).ok_or_else(|| McpError::Protocol {
                server: self.server.name.clone(),
                message: "the event stream ended without a response".to_owned(),
            })?
        } else {
            sent.body.clone()
        };

        let parsed: Envelope =
            serde_json::from_str(&envelope).map_err(|error| McpError::Protocol {
                server: self.server.name.clone(),
                message: format!(
                    "{method} answered {} with something that is not JSON-RPC: {}",
                    sent.status,
                    sent.scrub.text(&error.to_string())
                ),
            })?;

        if let Some(error) = parsed.error {
            return Err(McpError::Rpc {
                server: self.server.name.clone(),
                method: method.to_owned(),
                code: error.code,
                message: sent.scrub.text(&error.message),
            });
        }

        // An envelope with neither half is almost never the MCP server: it is
        // whatever sits in front of it — a gateway 404, an ingress that never
        // routed the request, a proxy answering its own JSON. The server then has
        // nothing in its log and the client has nothing to go on, so the status
        // and the body go in the message; they are the only things that name the
        // culprit.
        let result = parsed.result.ok_or_else(|| McpError::Protocol {
            server: self.server.name.clone(),
            message: format!(
                "{method} answered {} with neither a result nor an error — \
                 usually something in front of the server answering instead of it: {}",
                sent.status,
                snippet(&sent.scrub.text(&envelope))
            ),
        })?;

        Ok(Exchange {
            result,
            session_id: sent.session_id,
        })
    }

    /// Sends a notification: no `id`, and therefore no answer to wait for.
    ///
    /// Visible to [`super::negotiate`] for `notifications/initialized`, which is
    /// what closes the handshake on the older revisions.
    pub(super) async fn notify(
        &self,
        session: &Session,
        method: &str,
        params: Value,
        credentials: &McpCredentials<'_>,
    ) -> Result<(), McpError> {
        let sent = self
            .send(session, method, None, params, None, &[], credentials)
            .await?;

        // `202 Accepted` with an empty body is the expected answer, but a server
        // that says `200` and nothing is not doing anything wrong either.
        if sent.status.is_success() {
            return Ok(());
        }

        Err(McpError::Protocol {
            server: self.server.name.clone(),
            message: format!(
                "{method} was refused with {}: {}",
                sent.status,
                snippet(&sent.scrub.text(&sent.body))
            ),
        })
    }

    /// The raw `POST`: builds the body and headers, sends, reads the answer.
    #[allow(clippy::too_many_arguments)]
    async fn send(
        &self,
        session: &Session,
        method: &str,
        id: Option<u32>,
        params: Value,
        name: Option<&str>,
        mirrored: &[(String, String)],
        credentials: &McpCredentials<'_>,
    ) -> Result<Sent, McpError> {
        let body = envelope(session, method, id, params);
        let mut headers = self.headers(session, method, name, mirrored)?;
        let scrub = self.authenticate(&mut headers, credentials).await?;

        // Built here and not a line earlier: the auth provider has just added its
        // header and its secret to the redactor, and a record taken before that
        // would be a record of a request nobody made — or worse, one carrying a
        // credential in the clear.
        let mut record = self.journal.as_ref().map(|_| McpExchange {
            server: self.server.name.clone(),
            url: self.server.url.to_string(),
            method: method.to_owned(),
            revision: session.revision,
            notification: id.is_none(),
            headers: scrub.headers(&readable(&headers)),
            request: scrub.text(&body.to_string()),
            status: 0,
            streaming: false,
            response: String::new(),
            latency_ms: 0,
            error: None,
        });

        let started = Instant::now();
        let sent = self
            .http
            .post(self.server.url.clone())
            .headers(headers)
            .timeout(self.server.timeout)
            .json(&body)
            .send()
            .await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let response = match sent {
            Ok(response) => response,
            Err(error) => {
                let message = scrub.text(&error.to_string());
                if let Some(mut record) = record {
                    record.latency_ms = latency_ms;
                    record.error = Some(message.clone());
                    self.file(record);
                }
                return Err(McpError::Transport {
                    server: self.server.name.clone(),
                    message,
                });
            }
        };

        let status = response.status();
        let streaming = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        // Captured before the body is consumed. On the handshaking revisions the
        // server issues this in its `initialize` reply and expects it back on
        // everything after.
        let session_id = response
            .headers()
            .get(SESSION_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let text = response.text().await.unwrap_or_default();

        if let Some(mut record) = record.take() {
            record.status = status.as_u16();
            record.streaming = streaming;
            record.response = scrub.text(&text);
            record.latency_ms = latency_ms;
            self.file(record);
        }

        debug!(
            server = %self.server.name,
            method,
            revision = %session.revision,
            status = status.as_u16(),
            streaming,
            bytes = text.len(),
            "MCP response"
        );

        // A `404` to a request that carried a session is the revision's way of
        // saying the session is gone — a restarted server, an expiry, a different
        // replica. Distinguished from a plain gateway `404` by the fact that we
        // sent a session at all, and turned into one retry a layer up.
        if status == StatusCode::NOT_FOUND && session.has_id() {
            return Err(McpError::SessionLost {
                server: self.server.name.clone(),
                revision: session.revision.to_string(),
            });
        }

        Ok(Sent {
            status,
            streaming,
            session_id,
            body: text,
            scrub,
        })
    }

    /// Adds the templated headers and the auth provider's, in that order.
    ///
    /// Returns a redactor holding every secret that went out, so anything a
    /// server quotes back at us is scrubbed from the message rather than from
    /// nothing.
    async fn authenticate(
        &self,
        headers: &mut HeaderMap,
        credentials: &McpCredentials<'_>,
    ) -> Result<Redactor, McpError> {
        // Rendered here rather than at load, so a rotated token is picked up on
        // the next call.
        let mut scrub = Redactor::new();
        for (name, value) in self
            .server
            .headers
            .render(&self.server.name, credentials.named())?
        {
            let mut rendered =
                HeaderValue::from_str(value.expose()).map_err(|_| McpError::Header {
                    server: self.server.name.clone(),
                    header: name.to_string(),
                    message: "the rendered value cannot go in an HTTP header".to_owned(),
                })?;
            rendered.set_sensitive(true);
            scrub.add(&value);
            headers.insert(name, rendered);
        }

        // The auth provider goes last: a named provider is the more specific
        // statement, and it should win over a hand-written header of the same
        // name rather than be quietly overwritten by one.
        if let Some(provider) = credentials.provider() {
            let from_auth = provider
                .apply(headers, &self.server.url, None)
                .await
                .map_err(McpError::Auth)?;
            scrub.merge(&from_auth);
        }

        Ok(scrub)
    }

    /// The headers the revision in force actually defines.
    ///
    /// Nothing here is sent unconditionally beyond content negotiation: mirrored
    /// headers on an older server are unsolicited routing metadata, and
    /// `MCP-Protocol-Version` postdates the oldest revision we speak.
    fn headers(
        &self,
        session: &Session,
        method: &str,
        name: Option<&str>,
        mirrored: &[(String, String)],
    ) -> Result<HeaderMap, McpError> {
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        // Both shapes are acceptable answers, so both have to be acceptable to us.
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );

        if session.revision.sends_version_header() {
            headers.insert(
                HeaderName::from_static("mcp-protocol-version"),
                self.header_value(session.revision.as_str(), session.revision.as_str())?,
            );
        }

        if let Some(id) = &session.id {
            headers.insert(
                HeaderName::from_static(SESSION_HEADER),
                self.header_value(id, id)?,
            );
        }

        if session.revision.mirrors_headers() {
            headers.insert(
                HeaderName::from_static("mcp-method"),
                self.header_value(method, method)?,
            );

            if let Some(name) = name {
                headers.insert(
                    HeaderName::from_static("mcp-name"),
                    self.header_value(&encode_header_value(name), name)?,
                );
            }

            // `Mcp-Param-*`, read from the arguments that are about to be sent —
            // the server compares them to the body and rejects any disagreement.
            for (name, value) in mirrored {
                let header = HeaderName::try_from(name.to_ascii_lowercase()).map_err(|_| {
                    McpError::Protocol {
                        server: self.server.name.clone(),
                        message: format!("`{name}` is not a valid header name"),
                    }
                })?;
                headers.insert(header, self.header_value(value, value)?);
            }
        }

        Ok(headers)
    }

    fn header_value(&self, rendered: &str, original: &str) -> Result<HeaderValue, McpError> {
        HeaderValue::from_str(rendered).map_err(|_| McpError::Protocol {
            server: self.server.name.clone(),
            message: format!("`{original}` cannot be sent as an HTTP header value"),
        })
    }
}

/// The JSON-RPC body one request goes out as.
///
/// `id` is what separates a request from a notification: a notification is
/// exactly a request without one, and a server is entitled to answer nothing at
/// all to it.
fn envelope(session: &Session, method: &str, id: Option<u32>, mut params: Value) -> Value {
    // The handshake-free revision has no earlier exchange to have established the
    // protocol version or who is calling, so every request carries both. The
    // older ones said it once, in `initialize`, and repeating it there would be
    // inventing a field the revision does not define.
    if !session.revision.handshakes()
        && let Some(object) = params.as_object_mut()
    {
        object.insert(
            "_meta".to_owned(),
            json!({
                "io.modelcontextprotocol/protocolVersion": session.revision.as_str(),
                "io.modelcontextprotocol/clientInfo": {
                    "name": "mire",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "io.modelcontextprotocol/clientCapabilities": {},
            }),
        );
    }

    let mut body = json!({"jsonrpc": "2.0", "method": method, "params": params});
    if let Some(id) = id
        && let Some(object) = body.as_object_mut()
    {
        object.insert("id".to_owned(), json!(id));
    }
    body
}

/// Headers as text, for the journal.
///
/// A value that is not valid UTF-8 is named rather than dropped: a header that
/// cannot be printed is still a header that was sent, and its absence from the
/// record would be the one thing nobody could work out from the outside.
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

/// A parameter the server wants mirrored into a header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeaderParam {
    /// The `{Name}` in `Mcp-Param-{Name}`.
    pub header: String,
    /// Property path from the schema root, every step a `properties` key.
    pub path: Vec<String>,
}

/// Collects `x-mcp-header` annotations, or explains why the tool is unusable.
///
/// The constraints are the specification's: a non-empty HTTP token, unique
/// case-insensitively, only on `string` / `integer` / `boolean`, and only where
/// the property is reachable through `properties` alone — never through `items`,
/// `oneOf`, `$ref` and friends, because those paths are not static.
///
/// # Errors
///
/// Returns the reason the annotation set is invalid. A client must then exclude
/// the tool entirely rather than call it with headers the server will reject.
pub fn header_params(schema: &Value) -> Result<Vec<HeaderParam>, String> {
    let mut found = Vec::new();
    let mut seen: HashMap<String, String> = HashMap::new();
    collect(schema, &mut Vec::new(), &mut found, &mut seen)?;
    Ok(found)
}

fn collect(
    schema: &Value,
    path: &mut Vec<String>,
    found: &mut Vec<HeaderParam>,
    seen: &mut HashMap<String, String>,
) -> Result<(), String> {
    let Some(properties) = schema.get("properties").and_then(Value::as_object) else {
        return Ok(());
    };

    for (key, property) in properties {
        path.push(key.clone());

        if let Some(annotation) = property.get("x-mcp-header") {
            let header = annotation
                .as_str()
                .ok_or_else(|| format!("`{key}`: `x-mcp-header` must be a string"))?;
            validate_token(key, header)?;

            let lowered = header.to_ascii_lowercase();
            if let Some(previous) = seen.insert(lowered, key.clone()) {
                return Err(format!(
                    "`{header}` is declared twice (`{previous}` and `{key}`)"
                ));
            }

            match property.get("type").and_then(Value::as_str) {
                Some("string" | "integer" | "boolean") => {}
                Some(other) => {
                    return Err(format!(
                        "`{key}`: `{other}` cannot be mirrored into a header"
                    ));
                }
                None => {
                    return Err(format!("`{key}`: no `type`, so it cannot be mirrored"));
                }
            }

            found.push(HeaderParam {
                header: header.to_owned(),
                path: path.clone(),
            });
        }

        // Only `properties` chains are statically reachable, so only they are
        // walked: an annotation under `items` or `oneOf` is the tool's problem.
        collect(property, path, found, seen)?;
        path.pop();
    }

    Ok(())
}

/// RFC 9110 `tchar`, which is what an HTTP field name may contain.
fn validate_token(key: &str, header: &str) -> Result<(), String> {
    if header.is_empty() {
        return Err(format!("`{key}`: `x-mcp-header` must not be empty"));
    }
    let ok = header
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "`{key}`: `{header}` is not a valid HTTP header name"
        ))
    }
}

/// Reads each annotated value out of the arguments actually being sent.
///
/// A parameter that is absent or `null` contributes no header — the server is
/// told not to expect one.
#[must_use]
pub fn mirror_headers(params: &[HeaderParam], arguments: &Value) -> Vec<(String, String)> {
    let mut mirrored = Vec::new();

    for param in params {
        let mut cursor = arguments;
        for step in &param.path {
            let Some(next) = cursor.get(step) else {
                cursor = &Value::Null;
                break;
            };
            cursor = next;
        }

        let rendered = match cursor {
            Value::String(text) => text.clone(),
            Value::Bool(flag) => flag.to_string(),
            Value::Number(number) if number.is_i64() || number.is_u64() => number.to_string(),
            _ => continue,
        };

        mirrored.push((
            format!("Mcp-Param-{}", param.header),
            encode_header_value(&rendered),
        ));
    }

    mirrored
}

/// Wraps a value the sentinel way when it cannot be a plain ASCII header.
///
/// The specification is specific about the trigger: non-ASCII, control
/// characters, leading or trailing whitespace — and any plain value that happens
/// to look like the sentinel, which would otherwise be ambiguous.
#[must_use]
pub fn encode_header_value(value: &str) -> String {
    let plain = value
        .bytes()
        .all(|byte| (0x20..=0x7E).contains(&byte) || byte == b'\t')
        && value.trim() == value
        && !(value.starts_with("=?base64?") && value.ends_with("?="));

    if plain {
        value.to_owned()
    } else {
        format!("=?base64?{}?=", BASE64.encode(value.as_bytes()))
    }
}

/// A body fragment short enough to sit inside an error message.
///
/// Whitespace is collapsed and the tail is cut: the point is to identify who
/// answered, and a gateway says that in its first few words.
fn snippet(body: &str) -> String {
    const MAX: usize = 200;

    let collapsed = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut text: String = collapsed.chars().take(MAX).collect();
    if collapsed.chars().count() > MAX {
        text.push('…');
    }
    format!("`{text}`")
}

/// Pulls the last `data:` payload out of an SSE body.
///
/// The final JSON-RPC response terminates the stream, so the last event is the
/// answer and everything before it is progress or logging.
fn last_event(body: &str) -> Option<String> {
    let mut current = String::new();
    let mut last = None;

    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("data:") {
            if !current.is_empty() {
                current.push('\n');
            }
            current.push_str(rest.trim_start());
        } else if line.trim().is_empty() && !current.is_empty() {
            last = Some(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        last = Some(current);
    }
    last
}

/// Turns content blocks into something a model can be handed back.
///
/// Text is joined; anything else is described rather than inlined. A tool that
/// answers with a megabyte of base64 PNG should not become a megabyte of prompt.
fn flatten(content: &[Value]) -> String {
    let mut parts = Vec::new();

    for block in content {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => parts.push(
                block
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            ),
            Some(kind) => {
                let mime = block
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let bytes = block
                    .get("data")
                    .and_then(Value::as_str)
                    .map_or(0, |data| data.len() * 3 / 4);
                parts.push(format!("[{kind}: {mime}, {bytes} bytes]"));
            }
            None => parts.push(block.to_string()),
        }
    }

    parts.join("\n")
}

/// What one call is, independent of the protocol state it goes out on.
///
/// Bundled rather than passed as seven arguments through three frames: a lost
/// session sends the whole thing round again, and a long positional signature
/// repeated at every level is a transposed pair waiting to happen.
#[derive(Debug)]
struct Invocation<'a> {
    method: &'a str,
    params: Value,
    /// The tool name, for `Mcp-Name` where the revision mirrors it.
    name: Option<&'a str>,
    /// Parameters carrying an `x-mcp-header` annotation…
    annotated: &'a [HeaderParam],
    /// …and the arguments to read their values out of.
    arguments: &'a Value,
}

/// What one round trip produced.
#[derive(Debug)]
pub(super) struct Exchange {
    /// The JSON-RPC `result`.
    pub result: Value,
    /// `Mcp-Session-Id`, when the server sent one back. Only `initialize` really
    /// issues it, but reading it everywhere costs nothing and a server is allowed
    /// to be surprising.
    pub session_id: Option<String>,
}

/// A raw answer, before anyone has decided what it means.
#[derive(Debug)]
struct Sent {
    status: StatusCode,
    streaming: bool,
    session_id: Option<String>,
    body: String,
    /// Carries the credentials that went out, so any of them quoted back at us
    /// is scrubbed from the message rather than from nothing.
    scrub: Redactor,
}

#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ToolsList {
    #[serde(default)]
    tools: Vec<McpTool>,
    #[serde(default)]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallResult {
    #[serde(default)]
    result_type: Option<String>,
    #[serde(default)]
    content: Vec<Value>,
    #[serde(default)]
    structured_content: Option<Value>,
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    input_requests: Vec<Value>,
}

impl CallResult {
    /// The methods the server wants answered, for the error message.
    fn input_request_methods(&self) -> String {
        let methods: Vec<_> = self
            .input_requests
            .iter()
            .filter_map(|request| request.get("method").and_then(Value::as_str))
            .collect();
        if methods.is_empty() {
            "unspecified".to_owned()
        } else {
            methods.join(", ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_value_travels_as_itself() {
        assert_eq!(encode_header_value("us-west1"), "us-west1");
        assert_eq!(encode_header_value("42"), "42");
    }

    #[test]
    fn anything_a_header_cannot_carry_is_wrapped() {
        // The examples from the specification, verbatim.
        assert_eq!(
            encode_header_value("Hello, 世界"),
            "=?base64?SGVsbG8sIOS4lueVjA==?="
        );
        assert_eq!(encode_header_value(" padded "), "=?base64?IHBhZGRlZCA=?=");
        assert_eq!(
            encode_header_value("line1\nline2"),
            "=?base64?bGluZTEKbGluZTI=?="
        );
    }

    #[test]
    fn a_value_that_looks_like_the_sentinel_is_encoded_too() {
        // Otherwise a server could not tell the wrapper from the payload.
        assert_eq!(
            encode_header_value("=?base64?literal?="),
            "=?base64?PT9iYXNlNjQ/bGl0ZXJhbD89?="
        );
    }

    #[test]
    fn annotated_parameters_are_found_through_properties() {
        let schema = json!({
            "type": "object",
            "properties": {
                "region": {"type": "string", "x-mcp-header": "Region"},
                "query": {"type": "string"},
                "nested": {
                    "type": "object",
                    "properties": {
                        "tenant": {"type": "integer", "x-mcp-header": "Tenant"},
                    },
                },
            },
        });

        let params = header_params(&schema).unwrap();
        assert_eq!(params.len(), 2);
        assert!(params.contains(&HeaderParam {
            header: "Region".to_owned(),
            path: vec!["region".to_owned()],
        }));
        assert!(params.contains(&HeaderParam {
            header: "Tenant".to_owned(),
            path: vec!["nested".to_owned(), "tenant".to_owned()],
        }));
    }

    #[test]
    fn an_invalid_annotation_makes_the_whole_tool_unusable() {
        for (schema, expected) in [
            (
                json!({"properties": {"a": {"type": "string", "x-mcp-header": ""}}}),
                "must not be empty",
            ),
            (
                json!({"properties": {"a": {"type": "string", "x-mcp-header": "bad header"}}}),
                "not a valid HTTP header name",
            ),
            (
                // `number` is explicitly excluded, unlike `integer`.
                json!({"properties": {"a": {"type": "number", "x-mcp-header": "A"}}}),
                "cannot be mirrored",
            ),
            (
                json!({"properties": {
                    "a": {"type": "string", "x-mcp-header": "Dup"},
                    "b": {"type": "string", "x-mcp-header": "dup"},
                }}),
                "declared twice",
            ),
        ] {
            let error = header_params(&schema).unwrap_err();
            assert!(
                error.contains(expected),
                "{error} should mention {expected}"
            );
        }
    }

    #[test]
    fn an_annotation_off_the_static_path_is_simply_not_found() {
        // Under `items`, so not statically reachable — and therefore not a header.
        let schema = json!({
            "properties": {
                "rows": {
                    "type": "array",
                    "items": {"properties": {"id": {"type": "string", "x-mcp-header": "Id"}}},
                },
            },
        });
        assert!(header_params(&schema).unwrap().is_empty());
    }

    #[test]
    fn only_arguments_that_are_present_produce_a_header() {
        let params = vec![
            HeaderParam {
                header: "Region".to_owned(),
                path: vec!["region".to_owned()],
            },
            HeaderParam {
                header: "Missing".to_owned(),
                path: vec!["absent".to_owned()],
            },
            HeaderParam {
                header: "Null".to_owned(),
                path: vec!["nothing".to_owned()],
            },
        ];
        let arguments = json!({"region": "us-west1", "nothing": null});

        let mirrored = mirror_headers(&params, &arguments);
        assert_eq!(
            mirrored,
            vec![("Mcp-Param-Region".to_owned(), "us-west1".to_owned())]
        );
    }

    #[test]
    fn booleans_and_integers_render_the_way_the_spec_says() {
        let params = vec![
            HeaderParam {
                header: "Flag".to_owned(),
                path: vec!["flag".to_owned()],
            },
            HeaderParam {
                header: "Count".to_owned(),
                path: vec!["count".to_owned()],
            },
        ];
        let mirrored = mirror_headers(&params, &json!({"flag": false, "count": -7}));
        assert_eq!(mirrored[0].1, "false");
        assert_eq!(mirrored[1].1, "-7");
    }

    #[test]
    fn a_body_in_an_error_message_is_collapsed_and_cut() {
        assert_eq!(
            snippet("{\n  \"message\": \"no Route matched\"\n}"),
            "`{ \"message\": \"no Route matched\" }`"
        );
        let long = snippet(&"x".repeat(500));
        assert!(long.ends_with("…`"));
        assert!(long.chars().count() < 250);
    }

    #[test]
    fn the_last_event_of_a_stream_is_the_answer() {
        let body = concat!(
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n",
            "\n",
            "event: message\n",
            "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n",
            "\n",
        );
        let last = last_event(body).unwrap();
        assert!(last.contains("\"result\""));
    }

    #[test]
    fn a_multi_line_data_field_is_rejoined() {
        let body = "data: {\"a\":\ndata: 1}\n\n";
        assert_eq!(last_event(body).unwrap(), "{\"a\":\n1}");
    }

    #[test]
    fn text_blocks_join_and_binary_blocks_are_described() {
        let content = vec![
            json!({"type": "text", "text": "21 degrees"}),
            json!({"type": "image", "mimeType": "image/png", "data": "AAAAAAAA"}),
        ];
        let flat = flatten(&content);
        assert!(flat.starts_with("21 degrees"));
        assert!(flat.contains("[image: image/png, 6 bytes]"));
        // The payload itself never reaches the prompt.
        assert!(!flat.contains("AAAAAAAA"));
    }

    #[test]
    fn an_empty_tool_list_means_everything_is_offered() {
        let mut server = McpServer {
            name: "fs".to_owned(),
            url: Url::parse("https://mcp.internal/mcp").unwrap(),
            auth: None,
            tools: Vec::new(),
            headers: HeaderTemplates::default(),
            timeout: Duration::from_secs(30),
            protocol_version: None,
        };
        assert!(server.offers("anything"));

        server.tools = vec!["read_file".to_owned()];
        assert!(server.offers("read_file"));
        assert!(!server.offers("delete_everything"));
    }

    #[tokio::test]
    async fn a_stated_revision_is_this_clients_business_and_nobody_elses() {
        let client = McpClient::new(
            McpServer {
                name: "fs".to_owned(),
                url: Url::parse("https://mcp.internal/mcp").unwrap(),
                auth: None,
                tools: Vec::new(),
                headers: HeaderTemplates::default(),
                timeout: Duration::from_secs(30),
                protocol_version: None,
            },
            Client::new(),
        );
        *client.session.write().await = Some(Session::sessionless(
            Revision::LATEST,
            negotiate::Settled::Discovered,
        ));

        // Auto is not a choice: the same server, still settled on what it
        // negotiated.
        let auto = client.speaking(None);
        assert_eq!(auto.server().protocol_version, None);
        assert_eq!(
            auto.settled().await.map(|session| session.revision),
            Some(Revision::LATEST)
        );

        // A stated revision starts from nothing rather than inheriting whatever
        // some earlier caller settled on…
        let stated = client.speaking(Some(Revision::V20250326));
        assert_eq!(stated.server().protocol_version, Some(Revision::V20250326));
        assert!(stated.settled().await.is_none());

        // …and what it settles stays its own.
        assert_eq!(
            client.settled().await.map(|session| session.revision),
            Some(Revision::LATEST)
        );
    }
}
