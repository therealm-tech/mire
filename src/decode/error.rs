//! Reading the endpoint's own account of what went wrong.
//!
//! A refusal is an answer, and it is usually the most interesting one: the body
//! carries the sentence that explains the `400`. Until now that sentence was only
//! ever raw JSON in the trace — readable, but not something the UI could put next
//! to the answer, and not something a caller could branch on.
//!
//! So it gets a cascade like every other field. `decode.error` points at the node
//! the endpoint puts its complaint in, and what comes back is normalised across
//! the usual spellings:
//!
//! * `OpenAI` — `{"error": {"message": …, "type": …, "code": …}}`
//! * Ollama — `{"error": "model 'nope' not found"}`, a bare string
//! * vLLM — `{"object": "error", "message": …, "type": …, "code": 400}`
//! * a gateway in front of either — `{"detail": "Not authenticated"}`
//!
//! Status is deliberately **not** what decides whether to look. Plenty of
//! gateways answer `200` with an error in the body, and that mismatch is exactly
//! the sort of thing this tool exists to make visible.

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use super::chat::type_name;
use super::paths::{self, resolve_one};
use super::{DecodeField, DecodeTrace};
use crate::profile::DecodeSpec;

/// What the endpoint said went wrong, normalised.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecodedError {
    /// The sentence a human reads. `None` when the node carried a class or a
    /// code but nothing to say.
    pub message: Option<String>,
    /// Error class — `invalid_request_error`, `NotFoundError`, whatever the
    /// endpoint calls its families.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// Machine-readable code. Kept as text: `context_length_exceeded` and `400`
    /// are both codes, and one type for both beats two fields.
    pub code: Option<String>,
    /// The node exactly as the endpoint sent it, so nothing normalisation did
    /// not understand is lost.
    pub raw: Value,
}

impl DecodedError {
    /// Reads the common spellings out of an error node.
    ///
    /// A bare string *is* the message — an endpoint that answers
    /// `"error": "model not found"` has said everything it has to say. An object
    /// nesting its complaint under `error` is unwrapped once, so pointing the
    /// cascade at `$` works for a body that keeps everything under that key and
    /// for one that keeps it at the top level.
    #[must_use]
    pub fn from_value(raw: &Value) -> Self {
        let node = raw
            .get("error")
            .filter(|nested| nested.is_object())
            .unwrap_or(raw);

        // An empty string is not a message, and `scalar` is where that rule
        // lives — so a node that says `"error": ""` comes back empty and gets
        // dropped rather than announced.
        if !node.is_object() {
            return Self {
                message: scalar(node),
                kind: None,
                code: None,
                raw: raw.clone(),
            };
        }

        let read = |keys: &[&str]| keys.iter().find_map(|key| scalar(node.get(*key)?));

        // `error_description` before `error`, and `error` last: an OAuth2
        // failure — which is what a gateway answers when the credential is the
        // problem — puts the code in `error` and the sentence in
        // `error_description`, and taking the first would lose the sentence.
        let message = read(&[
            "message",
            "detail",
            "error_description",
            "description",
            "error_message",
            "error",
        ]);

        // Which leaves `error` free to be the code, but only when it was not
        // already the sentence: repeating the message under a second name reads
        // as two findings where there is one.
        let code = read(&["code", "error_code", "status", "status_code"])
            .or_else(|| read(&["error"]).filter(|code| Some(code) != message.as_ref()));

        Self {
            message,
            kind: read(&["type", "kind", "error_type", "reason"]),
            code,
            raw: raw.clone(),
        }
    }

    /// Whether nothing at all could be read out of the node.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.message.is_none() && self.kind.is_none() && self.code.is_none()
    }
}

/// Reads the error, when the endpoint reported one.
///
/// Two things are deliberately quiet here. A cascade that misses on a `2xx` is
/// not recorded: the endpoint answered, there is no error, and listing the paths
/// that failed to find one on every successful call would bury the trace under
/// the one thing that went right. A cascade that misses on a refusal *is*
/// recorded — that is a profile with a blind spot, and it is worth naming.
///
/// A node that resolves but carries no message, class or code is not reported as
/// an error either — and on a `2xx` it is not reported at all. `error: ["$"]` is
/// the sane way to cover an endpoint that keeps its complaint at the top level,
/// and it resolves against every good answer too; saying so each time would be
/// crying wolf on a response nobody has a question about.
#[must_use]
pub fn decode(
    raw: &Value,
    spec: &DecodeSpec,
    status: u16,
    trace: &mut DecodeTrace,
) -> Option<DecodedError> {
    let refused = status >= 400;

    let Some((path, node)) = resolve_one(raw, &spec.error) else {
        if refused {
            trace.miss(DecodeField::Error, paths::sources(&spec.error));
        }
        return None;
    };

    let error = DecodedError::from_value(node);
    if error.is_empty() {
        if refused {
            trace.issue(
                DecodeField::Error,
                path.source(),
                format!(
                    "expected an error, found {} carrying no message, type or code",
                    type_name(node)
                ),
            );
        }
        return None;
    }

    trace.hit(DecodeField::Error, path.source());
    Some(error)
}

/// A scalar as text. An object or an array is not a message, however much it
/// sits where one belongs.
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(sources: &[&str]) -> DecodeSpec {
        DecodeSpec {
            error: sources
                .iter()
                .map(|source| source.parse().unwrap())
                .collect(),
            ..DecodeSpec::default()
        }
    }

    fn openai_spec() -> DecodeSpec {
        spec(&["$.error", "$.detail"])
    }

    #[test]
    fn decodes_the_openai_error_shape() {
        let raw = serde_json::json!({
            "error": {
                "message": "This model's maximum context length is 32768 tokens.",
                "type": "invalid_request_error",
                "code": "context_length_exceeded"
            }
        });

        let mut trace = DecodeTrace::default();
        let error = decode(&raw, &openai_spec(), 400, &mut trace).unwrap();

        assert!(error.message.unwrap().contains("maximum context length"));
        assert_eq!(error.kind.as_deref(), Some("invalid_request_error"));
        assert_eq!(error.code.as_deref(), Some("context_length_exceeded"));
        assert_eq!(trace.matched[&DecodeField::Error], "$.error");
    }

    /// Ollama's whole error is a string, and it is the message.
    #[test]
    fn decodes_a_bare_string_error() {
        let raw = serde_json::json!({"error": "model 'nope' not found, try pulling it first"});

        let mut trace = DecodeTrace::default();
        let error = decode(&raw, &openai_spec(), 404, &mut trace).unwrap();

        assert_eq!(
            error.message.as_deref(),
            Some("model 'nope' not found, try pulling it first")
        );
        assert!(error.kind.is_none());
    }

    /// vLLM keeps the fields at the top level and numbers its codes.
    #[test]
    fn decodes_a_flat_error_with_a_numeric_code() {
        let raw = serde_json::json!({
            "object": "error",
            "message": "the model is still loading",
            "type": "ServiceUnavailable",
            "code": 503
        });

        let mut trace = DecodeTrace::default();
        let error = decode(&raw, &spec(&["$.error", "$"]), 503, &mut trace).unwrap();

        assert_eq!(error.message.as_deref(), Some("the model is still loading"));
        assert_eq!(error.code.as_deref(), Some("503"));
        assert_eq!(trace.matched[&DecodeField::Error], "$");
    }

    /// The reason this is not keyed off the status: a gateway that swallows the
    /// upstream failure and answers `200` with the complaint in the body.
    #[test]
    fn an_error_reported_under_a_two_hundred_is_still_an_error() {
        let raw = serde_json::json!({"error": {"message": "upstream refused the request"}});

        let mut trace = DecodeTrace::default();
        let error = decode(&raw, &openai_spec(), 200, &mut trace).unwrap();
        assert_eq!(
            error.message.as_deref(),
            Some("upstream refused the request")
        );
    }

    #[test]
    fn a_good_response_reports_neither_an_error_nor_a_miss() {
        let raw = serde_json::json!({"choices": [{"message": {"content": "pong"}}]});

        let mut trace = DecodeTrace::default();
        assert!(decode(&raw, &openai_spec(), 200, &mut trace).is_none());
        assert!(trace.missed.is_empty());
        assert!(trace.issues.is_empty());
    }

    /// The other half of that rule: when the endpoint *did* refuse and no path
    /// found the sentence, the profile has a blind spot worth naming.
    #[test]
    fn a_refusal_no_path_reaches_is_recorded_as_a_miss() {
        let raw = serde_json::json!({"failure": {"why": "quota"}});

        let mut trace = DecodeTrace::default();
        assert!(decode(&raw, &openai_spec(), 429, &mut trace).is_none());
        assert_eq!(
            trace.missed[&DecodeField::Error],
            vec!["$.error", "$.detail"]
        );
    }

    #[test]
    fn a_profile_that_asks_for_nothing_is_never_reported_as_missing() {
        let mut trace = DecodeTrace::default();
        assert!(
            decode(
                &serde_json::json!({}),
                &DecodeSpec::default(),
                500,
                &mut trace
            )
            .is_none()
        );
        assert!(trace.missed.is_empty());
    }

    /// `$` is how you cover an endpoint that keeps its complaint at the top
    /// level, and it resolves against every good answer as well. On a refusal
    /// that is worth explaining; on a `200` it is worth nothing at all.
    #[test]
    fn a_node_with_nothing_error_shaped_in_it_is_an_issue_only_on_a_refusal() {
        let raw = serde_json::json!({"choices": [{"message": {"content": "pong"}}]});

        let mut quiet = DecodeTrace::default();
        assert!(decode(&raw, &spec(&["$"]), 200, &mut quiet).is_none());
        assert!(quiet.issues.is_empty());

        let mut loud = DecodeTrace::default();
        assert!(decode(&raw, &spec(&["$"]), 500, &mut loud).is_none());
        assert_eq!(loud.issues.len(), 1);
        assert!(
            loud.issues[0].message.contains("no message, type or code"),
            "{}",
            loud.issues[0].message
        );
    }

    /// The gateway's answer when the credential is what went wrong. Taking
    /// `error` as the message would hand back `invalid_token` and drop the only
    /// sentence explaining it.
    #[test]
    fn an_oauth_error_keeps_the_sentence_and_the_code_apart() {
        let raw = serde_json::json!({
            "error": "invalid_token",
            "error_description": "The access token expired"
        });

        let mut trace = DecodeTrace::default();
        let error = decode(&raw, &spec(&["$"]), 401, &mut trace).unwrap();

        assert_eq!(error.message.as_deref(), Some("The access token expired"));
        assert_eq!(error.code.as_deref(), Some("invalid_token"));
    }

    /// And when `error` is all there is, it is the message once, not the message
    /// and the code.
    #[test]
    fn a_lone_error_key_is_not_repeated_as_a_code() {
        let raw = serde_json::json!({"error": "model 'nope' not found"});

        let mut trace = DecodeTrace::default();
        let error = decode(&raw, &spec(&["$"]), 404, &mut trace).unwrap();

        assert_eq!(error.message.as_deref(), Some("model 'nope' not found"));
        assert!(error.code.is_none());
    }

    #[test]
    fn the_node_is_kept_verbatim_next_to_the_normalised_fields() {
        let raw = serde_json::json!({"error": {"message": "nope", "param": "messages"}});

        let mut trace = DecodeTrace::default();
        let error = decode(&raw, &openai_spec(), 400, &mut trace).unwrap();
        assert_eq!(error.raw["param"], "messages");
    }
}
