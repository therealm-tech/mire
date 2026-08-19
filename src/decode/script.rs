//! Decoding a response with a Rhai script instead of `JSONPath` cascades.
//!
//! For the endpoint whose answer no set of paths can describe: the content is
//! assembled from three places, the tool calls need unwrapping, the vectors
//! arrive interleaved with something else. A script replaces the cascades
//! entirely — there is no precedence rule to remember, because declaring both is
//! a load error.
//!
//! It stays as non-fatal as path decoding. A script that fails puts its message
//! in the [`DecodeTrace`] next to the raw response, rather than hiding the
//! payload behind an error.

use std::collections::BTreeMap;

use rhai::Scope;
use serde::Deserialize;
use serde_json::Value;

use super::embedding::{Embedding, VectorEncoding, Vectors, summarise_all};
use super::error::DecodedError;
use super::{Completion, DecodeField, DecodeTrace, Usage};
use crate::message::ToolCall;
use crate::script::{ScriptError, ScriptSource, from_dynamic, to_dynamic};

/// How the script shows up in the trace.
const ORIGIN: &str = "<script>";

/// What a chat script is expected to return.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ScriptCompletion {
    content: Option<String>,
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<String>,
    usage: Option<Value>,
    error: Option<Value>,
}

/// What an embedding script is expected to return.
///
/// `f64` because that is Rhai's only float; the narrowing to the `f32` the rest
/// of the tool uses happens on the way out, and matches what the wire carries.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct ScriptEmbedding {
    vectors: ScriptVectors,
    usage: Option<Value>,
    error: Option<Value>,
}

/// A script returns either one vector per input, or — for a multi-vector
/// endpoint — a list of vectors per input. Neither reading fits the other's
/// JSON, so the nesting alone says which one it is.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ScriptVectors {
    /// One vector per item.
    Flat(Vec<Vec<f64>>),
    /// Several vectors per item.
    Grouped(Vec<Vec<Vec<f64>>>),
}

impl Default for ScriptVectors {
    fn default() -> Self {
        Self::Flat(Vec::new())
    }
}

/// Normalises whatever a script called an error, and traces it.
///
/// A script gets the same latitude a cascade does: a string is the message, a map
/// is read for the usual keys. What it must not do is invent an error out of
/// nothing, so a returned node with nothing error-shaped in it is dropped the way
/// [`super::error::decode`] drops one.
fn script_error(returned: Option<&Value>, trace: &mut DecodeTrace) -> Option<DecodedError> {
    let error = DecodedError::from_value(returned?);
    if error.is_empty() {
        trace.issue(
            DecodeField::Error,
            ORIGIN,
            "the script returned an `error` carrying no message, type or code",
        );
        return None;
    }
    trace.hit(DecodeField::Error, ORIGIN);
    Some(error)
}

#[expect(
    clippy::cast_possible_truncation,
    reason = "embeddings are f32 on the wire; Rhai only has f64, so this narrows back to the real precision"
)]
fn narrow(vector: Vec<f64>) -> Vec<f32> {
    vector.into_iter().map(|value| value as f32).collect()
}

impl ScriptVectors {
    /// Groups by item, narrowing to the `f32` the wire actually carries.
    fn into_vectors(self) -> Vectors {
        match self {
            Self::Flat(vectors) => Vectors::new(vectors.into_iter().map(narrow).collect()),
            Self::Grouped(items) => Vectors::grouped(
                items
                    .into_iter()
                    .map(|item| item.into_iter().map(narrow).collect())
                    .collect(),
            ),
        }
    }
}

/// Runs a decode script with the response bound into scope.
fn run(
    script: &ScriptSource,
    raw: &Value,
    status: u16,
    headers: &BTreeMap<String, String>,
) -> Result<rhai::Dynamic, ScriptError> {
    let mut scope = Scope::new();
    scope.push_dynamic("raw", to_dynamic(raw)?);
    scope.push("status", i64::from(status));
    scope.push_dynamic(
        "headers",
        to_dynamic(&serde_json::to_value(headers).unwrap_or_default())?,
    );
    script.run(&mut scope)
}

/// Decodes a `kind: chat` response with a script.
///
/// The error, when the script reported one, comes back next to the completion
/// rather than inside it: an endpoint refusing a call has not produced a partial
/// completion, and both kinds of profile report a refusal the same way.
#[must_use]
pub fn decode_chat(
    raw: &Value,
    status: u16,
    headers: &BTreeMap<String, String>,
    script: &ScriptSource,
) -> (Completion, Option<DecodedError>, DecodeTrace) {
    let mut trace = DecodeTrace::default();

    let returned = match run(script, raw, status, headers).and_then(|value| {
        from_dynamic::<ScriptCompletion>(
            &value,
            "a map with `content`, `tool_calls`, `finish_reason`, `usage` and `error`",
        )
    }) {
        Ok(returned) => returned,
        Err(error) => {
            trace.issue(DecodeField::Script, ORIGIN, error.to_string());
            return (Completion::default(), None, trace);
        }
    };

    // Only the fields the script actually filled are reported as matched, so the
    // trace reads the same way it does for a cascade.
    if returned.content.is_some() {
        trace.hit(DecodeField::Content, ORIGIN);
    }
    if !returned.tool_calls.is_empty() {
        trace.hit(DecodeField::ToolCalls, ORIGIN);
    }
    if returned.finish_reason.is_some() {
        trace.hit(DecodeField::FinishReason, ORIGIN);
    }
    let usage = returned.usage.as_ref().map(Usage::from_value);
    if usage.is_some() {
        trace.hit(DecodeField::Usage, ORIGIN);
    }
    let error = script_error(returned.error.as_ref(), &mut trace);

    let completion = Completion {
        content: returned.content,
        tool_calls: returned.tool_calls,
        finish_reason: returned.finish_reason,
        usage,
    };
    (completion, error, trace)
}

/// Decodes a `kind: embedding` response with a script.
#[must_use]
pub fn decode_embedding(
    raw: &Value,
    status: u16,
    headers: &BTreeMap<String, String>,
    script: &ScriptSource,
    include_vectors: bool,
) -> (Embedding, Vectors, Option<DecodedError>, DecodeTrace) {
    let mut trace = DecodeTrace::default();

    let returned = match run(script, raw, status, headers).and_then(|value| {
        from_dynamic::<ScriptEmbedding>(&value, "a map with `vectors`, `usage` and `error`")
    }) {
        Ok(returned) => returned,
        Err(error) => {
            trace.issue(DecodeField::Script, ORIGIN, error.to_string());
            let empty = Vectors::default();
            let embedding = summarise_all(&empty, VectorEncoding::None, None, false);
            return (embedding, empty, None, trace);
        }
    };

    let usage = returned.usage.as_ref().map(Usage::from_value);
    if usage.is_some() {
        trace.hit(DecodeField::Usage, ORIGIN);
    }
    let error = script_error(returned.error.as_ref(), &mut trace);

    let vectors = returned.vectors.into_vectors();
    let encoding = if vectors.items().is_empty() {
        trace.miss(DecodeField::Vectors, vec![ORIGIN.to_owned()]);
        VectorEncoding::None
    } else {
        trace.hit(DecodeField::Vectors, ORIGIN);
        // Whatever the wire format was, the script already turned it into floats.
        VectorEncoding::Float
    };

    let embedding = summarise_all(&vectors, encoding, usage, include_vectors);
    (embedding, vectors, error, trace)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(source: &str) -> ScriptSource {
        source.parse().unwrap()
    }

    fn headers() -> BTreeMap<String, String> {
        BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())])
    }

    /// The shape no cascade reaches: the answer is split across a list of
    /// segments that have to be joined, and the stop reason is a boolean.
    #[test]
    fn a_script_decodes_a_response_no_cascade_could() {
        let raw = serde_json::json!({
            "segments": [
                {"kind": "text", "value": "the answer "},
                {"kind": "meta", "value": "IGNORE ME"},
                {"kind": "text", "value": "in two pieces"}
            ],
            "complete": true,
            "counters": {"in": 12, "out": 5}
        });

        let (completion, _, trace) = decode_chat(
            &raw,
            200,
            &headers(),
            &script(
                r#"
                let text = "";
                for segment in raw.segments {
                    if segment.kind == "text" { text += segment.value; }
                }
                #{
                    content: text,
                    finish_reason: if raw.complete { "stop" } else { "length" },
                    usage: #{ prompt_tokens: raw.counters["in"], completion_tokens: raw.counters.out },
                }
                "#,
            ),
        );

        assert_eq!(
            completion.content.as_deref(),
            Some("the answer in two pieces")
        );
        assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
        assert_eq!(completion.usage.unwrap().total_tokens, Some(17));
        assert_eq!(trace.matched[&DecodeField::Content], ORIGIN);
    }

    /// Ollama reports nanoseconds; turning that into tokens per second is
    /// arithmetic, which is exactly what a cascade cannot do. It also pins down
    /// how large integers survive the trip into Rhai.
    #[test]
    fn a_script_can_do_arithmetic_on_the_response() {
        let raw = serde_json::json!({
            "message": {"content": "hi"},
            "done_reason": "stop",
            "eval_count": 8,
            "eval_duration": 16_000_000_000_i64,
        });

        let (completion, _, _) = decode_chat(
            &raw,
            200,
            &headers(),
            &script(
                r"
                let ns = raw.eval_duration.to_float();
                let speed = if ns > 0.0 { raw.eval_count.to_float() * 1000000000.0 / ns } else { 0.0 };
                #{ content: raw.message.content, finish_reason: `${raw.done_reason} (${speed} tok/s)` }
                ",
            ),
        );

        assert_eq!(completion.content.as_deref(), Some("hi"));
        assert_eq!(
            completion.finish_reason.as_deref(),
            Some("stop (0.5 tok/s)")
        );
    }

    /// The motivating case for scripts: a reasoning block has to be *removed*
    /// from the content, and no `JSONPath` rewrites a string.
    #[test]
    fn a_script_strips_a_reasoning_block_a_path_could_only_select() {
        let raw = serde_json::json!({
            "message": {"content": "<think>\nlet me see, 2+2\n</think>\n\n4"},
            "done_reason": "stop"
        });

        let (completion, _, _) = decode_chat(
            &raw,
            200,
            &headers(),
            &script(
                r#"
                let content = raw.message.content;
                let close = content.index_of("</think>");
                if close >= 0 {
                    content = content.sub_string(close + 8);
                    content.trim();
                }
                #{ content: content, finish_reason: raw.done_reason }
                "#,
            ),
        );

        assert_eq!(completion.content.as_deref(), Some("4"));
    }

    #[test]
    fn a_script_can_read_the_status_and_the_headers() {
        let (completion, _, _) = decode_chat(
            &serde_json::json!({}),
            503,
            &headers(),
            &script(r#"#{ content: `${status} ${headers["content-type"]}` }"#),
        );
        assert_eq!(completion.content.as_deref(), Some("503 application/json"));
    }

    #[test]
    fn a_failing_script_is_traced_rather_than_fatal() {
        let (completion, _, trace) = decode_chat(
            &serde_json::json!({"a": 1}),
            200,
            &headers(),
            &script("raw.nope.deeper"),
        );

        assert!(completion.content.is_none());
        assert_eq!(trace.issues.len(), 1);
        assert_eq!(trace.issues[0].field, DecodeField::Script);
    }

    #[test]
    fn a_script_returning_the_wrong_shape_says_what_was_wanted() {
        let (_, _, trace) = decode_chat(&serde_json::json!({}), 200, &headers(), &script("42"));
        assert!(
            trace.issues[0].message.contains("expected a map"),
            "{}",
            trace.issues[0].message
        );
    }

    /// The endpoint that reports its failure where nothing else does — in a
    /// header, with the body carrying a perfectly ordinary-looking `200`.
    #[test]
    fn a_script_can_report_an_error_of_its_own() {
        let mut headers = headers();
        headers.insert("x-upstream-status".to_owned(), "503".to_owned());

        let (completion, error, trace) = decode_chat(
            &serde_json::json!({"message": {"content": ""}}),
            200,
            &headers,
            &script(
                r#"
                let upstream = headers["x-upstream-status"];
                if upstream != "200" {
                    #{ error: #{ message: `upstream answered ${upstream}`, code: upstream } }
                } else {
                    #{ content: raw.message.content }
                }
                "#,
            ),
        );

        assert!(completion.content.is_none());
        let error = error.unwrap();
        assert_eq!(error.message.as_deref(), Some("upstream answered 503"));
        assert_eq!(error.code.as_deref(), Some("503"));
        assert_eq!(trace.matched[&DecodeField::Error], ORIGIN);
    }

    /// A script saying `error: ""` has not found an error, and a red banner over
    /// a good answer is worse than nothing.
    #[test]
    fn a_script_error_with_nothing_in_it_is_an_issue_not_an_error() {
        let (_, error, trace) = decode_chat(
            &serde_json::json!({}),
            200,
            &headers(),
            &script(r#"#{ content: "fine", error: "" }"#),
        );

        assert!(error.is_none());
        assert_eq!(trace.issues[0].field, DecodeField::Error);
    }

    #[test]
    fn an_embedding_script_reports_an_error_the_same_way() {
        let (embedding, _, error, _) = decode_embedding(
            &serde_json::json!({"error": "the model is not an embedding model"}),
            400,
            &headers(),
            &script("#{ vectors: [], error: raw.error }"),
            false,
        );

        assert_eq!(embedding.count, 0);
        assert_eq!(
            error.unwrap().message.as_deref(),
            Some("the model is not an embedding model")
        );
    }

    #[test]
    fn a_script_can_unpack_vectors_a_cascade_cannot_reach() {
        // Vectors interleaved with their labels, which no JSONPath selects alone.
        let raw = serde_json::json!({
            "rows": [
                {"label": "a", "values": [1.0, 0.0]},
                {"label": "b", "values": [0.0, 2.0]}
            ]
        });

        let (embedding, vectors, _, trace) = decode_embedding(
            &raw,
            200,
            &headers(),
            &script("#{ vectors: raw.rows.map(|row| row.values) }"),
            true,
        );

        assert_eq!(embedding.count, 2);
        assert_eq!(embedding.dimensions.uniform(), Some(2));
        assert!((embedding.vectors[1].norm - 2.0).abs() < 1e-6);
        assert_eq!(vectors.items()[0][0], vec![1.0, 0.0]);
        assert_eq!(trace.matched[&DecodeField::Vectors], ORIGIN);
        assert_eq!(embedding.full.unwrap().len(), 2);
    }

    #[test]
    fn an_embedding_script_that_finds_nothing_reports_a_miss() {
        let (embedding, _, _, trace) = decode_embedding(
            &serde_json::json!({}),
            200,
            &headers(),
            &script("#{ vectors: [] }"),
            false,
        );
        assert_eq!(embedding.count, 0);
        assert!(trace.missed.contains_key(&DecodeField::Vectors));
    }

    #[test]
    fn the_sandbox_still_applies_to_a_decode_script() {
        let (completion, _, trace) = decode_chat(
            &serde_json::json!({}),
            200,
            &headers(),
            &script("let n = 0; loop { n += 1; } #{ content: \"unreachable\" }"),
        );
        assert!(completion.content.is_none());
        assert!(trace.issues[0].message.to_lowercase().contains("operation"));
    }
}
