//! Turning whatever the endpoint answered into a normalised shape.
//!
//! This is the layer that keeps a non-conforming endpoint from leaking upwards:
//! above it, everything is a [`Completion`]. Below it, anything goes.
//!
//! Decoding is deliberately **non-fatal**. A path that misses does not fail the
//! call — it lands in the [`DecodeTrace`], next to the raw response. When you are
//! staring at an unfamiliar JSON shape, an error that hides the payload is the
//! last thing you want.

pub mod chat;
pub mod embedding;
pub mod error;
pub mod paths;
pub mod script;
pub mod stream;

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::Serialize;

use crate::message::ToolCall;

/// A normalised field, and the key used in the decode trace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DecodeField {
    /// Assistant text.
    Content,
    /// Assistant text inside one chunk of a streamed response.
    Delta,
    /// Tool calls.
    ToolCalls,
    /// Stop reason.
    FinishReason,
    /// Token accounting.
    Usage,
    /// What the endpoint said went wrong.
    Error,
    /// Embedding vectors.
    Vectors,
    /// The decode script, when there is one, rather than any one field.
    Script,
}

/// What the endpoint returned, in a shape the rest of the tool understands.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Decoded {
    /// A `kind: chat` response.
    Completion(Completion),
    /// A `kind: embedding` response. Vectors are summarised, never rendered
    /// whole — see [`embedding`].
    Embedding(Box<EmbeddingResult>),
}

/// An embedding response together with the shape checks derived from it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddingResult {
    /// The normalised response.
    #[serde(flatten)]
    pub embedding: embedding::Embedding,
    /// What held and what did not.
    pub checks: embedding::EmbeddingChecks,
}

/// Normalised chat response.
///
/// The raw body and the HTTP metadata sit alongside this in the API response
/// rather than inside it, so they are not duplicated per decoded shape.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Completion {
    /// Assistant text. `None` when no configured path resolved to a string.
    pub content: Option<String>,
    /// Tool calls the model emitted.
    pub tool_calls: Vec<ToolCall>,
    /// Why generation stopped.
    pub finish_reason: Option<String>,
    /// Token accounting.
    pub usage: Option<Usage>,
}

/// Token accounting, normalised across the usual spellings.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Tokens in the prompt.
    pub prompt_tokens: Option<u64>,
    /// Tokens generated.
    pub completion_tokens: Option<u64>,
    /// Total, as reported or as the sum of the two above.
    pub total_tokens: Option<u64>,
    /// The usage object exactly as the endpoint sent it.
    pub raw: serde_json::Value,
}

impl Usage {
    /// Reads the common spellings out of a usage object.
    ///
    /// Covers `prompt_tokens` / `input_tokens` / `prompt_eval_count` and
    /// `completion_tokens` / `output_tokens` / `eval_count`, and computes the
    /// total when the endpoint omits it. The `*_count` pair is Ollama's native
    /// API, where those fields sit at the top level rather than under `usage` —
    /// point the `usage` path at `$` for that one.
    #[must_use]
    pub fn from_value(raw: &serde_json::Value) -> Self {
        let read = |keys: &[&str]| keys.iter().find_map(|key| raw.get(*key)?.as_u64());

        let prompt_tokens = read(&[
            "prompt_tokens",
            "input_tokens",
            "prompt_eval_count",
            "promptTokens",
        ]);
        let completion_tokens = read(&[
            "completion_tokens",
            "output_tokens",
            "generated_tokens",
            "eval_count",
            "completionTokens",
        ]);
        let total_tokens = read(&["total_tokens", "totalTokens"])
            .or_else(|| Some(prompt_tokens? + completion_tokens?));

        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            raw: raw.clone(),
        }
    }
}

/// HTTP-level facts about the exchange.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HttpMeta {
    /// Response status.
    pub status: u16,
    /// Response headers, with credentials masked.
    pub headers: BTreeMap<String, String>,
    /// Wall-clock time from sending the request to having the full body.
    pub latency_ms: u64,
    /// Time to first token. Only meaningful for a streamed response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
}

/// A path that resolved to something unusable.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecodeIssue {
    /// Field being decoded.
    pub field: DecodeField,
    /// The path that resolved.
    pub path: String,
    /// What was found there, and what was wanted.
    pub message: String,
}

/// Which path won, which missed, and what went wrong — the whole point of the
/// "assisted decode discovery" workflow.
#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecodeTrace {
    /// Field to the path that resolved it.
    pub matched: BTreeMap<DecodeField, String>,
    /// Field to every path tried, when none resolved.
    pub missed: BTreeMap<DecodeField, Vec<String>>,
    /// Paths that resolved to the wrong kind of value.
    pub issues: Vec<DecodeIssue>,
}

impl DecodeTrace {
    /// Records that `path` resolved `field`.
    pub fn hit(&mut self, field: DecodeField, path: &str) {
        self.matched.insert(field, path.to_owned());
    }

    /// Records that none of `tried` resolved `field`. A field with no configured
    /// path at all is not recorded: nothing was asked for, nothing is missing.
    pub fn miss(&mut self, field: DecodeField, tried: Vec<String>) {
        if !tried.is_empty() {
            self.missed.insert(field, tried);
        }
    }

    /// Records a path that resolved to an unusable value.
    pub fn issue(&mut self, field: DecodeField, path: &str, message: impl Into<String>) {
        self.issues.push(DecodeIssue {
            field,
            path: path.to_owned(),
            message: message.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_normalises_the_anthropic_spelling() {
        let usage = Usage::from_value(&serde_json::json!({
            "input_tokens": 12,
            "output_tokens": 30,
        }));
        assert_eq!(usage.prompt_tokens, Some(12));
        assert_eq!(usage.completion_tokens, Some(30));
        assert_eq!(usage.total_tokens, Some(42));
    }

    /// Ollama's native API puts its counters at the top level, next to
    /// everything else, so `usage: ["$"]` has to work.
    #[test]
    fn usage_normalises_the_ollama_native_spelling() {
        let usage = Usage::from_value(&serde_json::json!({
            "model": "qwen3:0.6b",
            "done_reason": "stop",
            "prompt_eval_count": 11,
            "eval_count": 7,
        }));
        assert_eq!(usage.prompt_tokens, Some(11));
        assert_eq!(usage.completion_tokens, Some(7));
        assert_eq!(usage.total_tokens, Some(18));
    }

    #[test]
    fn usage_prefers_a_reported_total() {
        let usage = Usage::from_value(&serde_json::json!({
            "prompt_tokens": 1,
            "completion_tokens": 1,
            "total_tokens": 99,
        }));
        assert_eq!(usage.total_tokens, Some(99));
    }

    #[test]
    fn a_field_with_no_configured_path_is_not_reported_as_missing() {
        let mut trace = DecodeTrace::default();
        trace.miss(DecodeField::Usage, Vec::new());
        assert!(trace.missed.is_empty());
    }
}
