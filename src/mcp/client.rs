//! Streamable HTTP transport for MCP revision `2026-07-28`.
//!
//! One `POST` per request, to one endpoint. The answer is either a JSON object or
//! an SSE stream whose last event carries the response; a client has to accept
//! both, so both are handled here and collapsed into a single value.
//!
//! Two rules from the revision earn most of the code below:
//!
//! * selected body fields are **mirrored into headers** (`Mcp-Method`,
//!   `Mcp-Name`, and `Mcp-Param-*` for annotated parameters) so intermediaries
//!   can route without parsing bodies — and the server rejects the request with
//!   `-32020` if a header and the body disagree;
//! * a value that cannot be a plain ASCII header is carried base64 in a sentinel
//!   wrapper, which the server undoes before comparing.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::Client;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracing::{debug, warn};
use url::Url;

use super::headers::HeaderTemplates;
use super::{McpError, McpTool, PROTOCOL_VERSION, ToolResult};
use crate::auth::AuthProvider;

use super::McpCredentials;
use crate::redact::Redactor;

/// Pages of `tools/list` to follow before deciding a server is toying with us.
const MAX_PAGES: usize = 20;

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
}

impl McpServer {
    /// Whether `tool` is one this server is allowed to offer.
    #[must_use]
    pub fn offers(&self, tool: &str) -> bool {
        self.tools.is_empty() || self.tools.iter().any(|allowed| allowed == tool)
    }
}

/// Talks to one MCP server.
#[derive(Debug, Clone)]
pub struct McpClient {
    server: McpServer,
    http: Client,
}

impl McpClient {
    /// Wraps a server definition around the shared HTTP client.
    #[must_use]
    pub fn new(server: McpServer, http: Client) -> Self {
        Self { server, http }
    }

    /// The server this talks to.
    #[must_use]
    pub fn server(&self) -> &McpServer {
        &self.server
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
                .request("tools/list", Value::Object(params), None, &[], credentials)
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
        // body, so these are derived from exactly what is about to be sent.
        let annotated = header_params(&tool.input_schema).map_err(|reason| McpError::Protocol {
            server: self.server.name.clone(),
            message: format!("`{}` has an invalid `x-mcp-header`: {reason}", tool.name),
        })?;
        let mirrored = mirror_headers(&annotated, arguments);

        let started = Instant::now();
        let result = self
            .request(
                "tools/call",
                params,
                Some(&tool.name),
                &mirrored,
                credentials,
            )
            .await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);

        let result = result?;
        debug!(
            server = %self.server.name,
            tool = %tool.name,
            mirrored = mirrored.len(),
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

    /// One JSON-RPC round trip.
    async fn request(
        &self,
        method: &str,
        mut params: Value,
        name: Option<&str>,
        mirrored: &[(String, String)],
        credentials: &McpCredentials<'_>,
    ) -> Result<Value, McpError> {
        // Every request carries its own protocol version and identity: there is
        // no handshake to have established it earlier.
        if let Some(object) = params.as_object_mut() {
            object.insert(
                "_meta".to_owned(),
                json!({
                    "io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
                    "io.modelcontextprotocol/clientInfo": {
                        "name": "mire",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "io.modelcontextprotocol/clientCapabilities": {},
                }),
            );
        }
        let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});

        let mut headers = self.headers(method, name, mirrored)?;

        // Rendered here rather than at load, so a rotated token is picked up on
        // the next call. Everything they produce goes into the redactor before
        // anything can quote it back at us.
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
                .apply(&mut headers, &self.server.url, None)
                .await
                .map_err(McpError::Auth)?;
            scrub.merge(&from_auth);
        }

        let response = self
            .http
            .post(self.server.url.clone())
            .headers(headers)
            .timeout(self.server.timeout)
            .json(&body)
            .send()
            .await
            .map_err(|error| McpError::Transport {
                server: self.server.name.clone(),
                message: scrub.text(&error.to_string()),
            })?;

        let status = response.status();
        let streaming = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.starts_with("text/event-stream"));
        let text = response.text().await.unwrap_or_default();

        debug!(
            server = %self.server.name,
            method,
            status = status.as_u16(),
            streaming,
            bytes = text.len(),
            "MCP response"
        );

        let envelope = if streaming {
            last_event(&text).ok_or_else(|| McpError::Protocol {
                server: self.server.name.clone(),
                message: "the event stream ended without a response".to_owned(),
            })?
        } else {
            text.clone()
        };

        let parsed: Envelope =
            serde_json::from_str(&envelope).map_err(|error| McpError::Protocol {
                server: self.server.name.clone(),
                message: format!(
                    "{method} answered {status} with something that is not JSON-RPC: {}",
                    scrub.text(&error.to_string())
                ),
            })?;

        if let Some(error) = parsed.error {
            return Err(McpError::Rpc {
                server: self.server.name.clone(),
                method: method.to_owned(),
                code: error.code,
                message: scrub.text(&error.message),
            });
        }

        // An envelope with neither half is almost never the MCP server: it is
        // whatever sits in front of it — a gateway 404, an ingress that never
        // routed the request, a proxy answering its own JSON. The server then has
        // nothing in its log and the client has nothing to go on, so the status
        // and the body go in the message; they are the only things that name the
        // culprit.
        parsed.result.ok_or_else(|| McpError::Protocol {
            server: self.server.name.clone(),
            message: format!(
                "{method} answered {status} with neither a result nor an error — \
                 usually something in front of the server answering instead of it: {}",
                snippet(&scrub.text(&envelope))
            ),
        })
    }

    /// The headers the revision requires, mirrored from the body.
    fn headers(
        &self,
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
        headers.insert(
            HeaderName::from_static("mcp-protocol-version"),
            HeaderValue::from_static(PROTOCOL_VERSION),
        );
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

        // `Mcp-Param-*`, read from the arguments that are about to be sent — the
        // server compares them to the body and rejects any disagreement.
        for (name, value) in mirrored {
            let header = HeaderName::try_from(name.to_ascii_lowercase()).map_err(|_| {
                McpError::Protocol {
                    server: self.server.name.clone(),
                    message: format!("`{name}` is not a valid header name"),
                }
            })?;
            headers.insert(header, self.header_value(value, value)?);
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
        };
        assert!(server.offers("anything"));

        server.tools = vec!["read_file".to_owned()];
        assert!(server.offers("read_file"));
        assert!(!server.offers("delete_everything"));
    }
}
