//! Decoding a `kind: chat` response into a [`Completion`].

use serde_json::Value;

use super::paths::{self, resolve, resolve_one};
use super::{Completion, DecodeField, DecodeTrace, Usage};
use crate::message::ToolCall;
use crate::profile::DecodeSpec;

/// Decodes `raw` according to `spec`.
///
/// Never fails: whatever could not be read is reported in the returned
/// [`DecodeTrace`] instead, so the caller can show the raw JSON next to the list
/// of paths that missed.
#[must_use]
pub fn decode(raw: &Value, spec: &DecodeSpec) -> (Completion, DecodeTrace) {
    let mut trace = DecodeTrace::default();
    let completion = Completion {
        content: decode_content(raw, spec, &mut trace),
        tool_calls: decode_tool_calls(raw, spec, &mut trace),
        finish_reason: decode_finish_reason(raw, spec, &mut trace),
        usage: decode_usage(raw, spec, &mut trace),
    };

    (completion, trace)
}

/// Decodes everything a *streamed* response can only say at the end.
///
/// The text is not read here: it was accumulated chunk by chunk (see
/// [`super::stream::delta`]) and the caller already has it. What is left lives in
/// the final chunk, which is where every endpoint puts its stop reason and its
/// token counts.
///
/// Tool calls are read too, and that works for an endpoint that sends each call
/// whole — Ollama's native API does. `OpenAI` splits a call's arguments across
/// chunks, and reassembling those is not attempted: agent mode does not stream,
/// so tool calling is tested where it is answered.
#[must_use]
pub fn decode_tail(raw: &Value, spec: &DecodeSpec, trace: &mut DecodeTrace) -> Completion {
    Completion {
        content: None,
        tool_calls: decode_tool_calls(raw, spec, trace),
        finish_reason: decode_finish_reason(raw, spec, trace),
        usage: decode_usage(raw, spec, trace),
    }
}

/// Reads the assistant text.
///
/// Multiple selected nodes are concatenated, which is what makes
/// `$.content[*].text` work for endpoints that split their answer into blocks.
fn decode_content(raw: &Value, spec: &DecodeSpec, trace: &mut DecodeTrace) -> Option<String> {
    let Some((path, nodes)) = resolve(raw, &spec.content) else {
        trace.miss(DecodeField::Content, paths::sources(&spec.content));
        return None;
    };

    let mut parts = Vec::with_capacity(nodes.len());
    for node in &nodes {
        match node {
            Value::String(text) => parts.push(text.clone()),
            other => {
                trace.issue(
                    DecodeField::Content,
                    path.source(),
                    format!("expected a string, found {}", type_name(other)),
                );
                return None;
            }
        }
    }

    trace.hit(DecodeField::Content, path.source());
    Some(parts.concat())
}

/// Reads the tool calls.
///
/// The selected nodes may be one array (the `OpenAI` shape) or several objects (a
/// wildcard over content blocks); both are flattened to a list of calls.
fn decode_tool_calls(raw: &Value, spec: &DecodeSpec, trace: &mut DecodeTrace) -> Vec<ToolCall> {
    let Some((path, nodes)) = resolve(raw, &spec.tool_calls) else {
        trace.miss(DecodeField::ToolCalls, paths::sources(&spec.tool_calls));
        return Vec::new();
    };

    let items: Vec<&Value> = match nodes.as_slice() {
        [Value::Array(array)] => array.iter().collect(),
        other => other.to_vec(),
    };

    let mut calls = Vec::with_capacity(items.len());
    for item in items {
        match tool_call_from_value(item) {
            Some(call) => calls.push(call),
            None => trace.issue(
                DecodeField::ToolCalls,
                path.source(),
                format!("cannot read a tool call out of {}", type_name(item)),
            ),
        }
    }

    if !calls.is_empty() {
        trace.hit(DecodeField::ToolCalls, path.source());
    }
    calls
}

/// Normalises the two tool-call shapes seen in the wild.
///
/// `OpenAI` nests under `function` and sends `arguments` as a JSON *string*;
/// Anthropic-style endpoints put `name` and `input` at the top level. Arguments
/// that arrive as a string are parsed, so assertions can look inside them either way.
pub(crate) fn tool_call_from_value(value: &Value) -> Option<ToolCall> {
    let object = value.as_object()?;
    let function = object.get("function").and_then(Value::as_object);

    let name = function
        .and_then(|f| f.get("name"))
        .or_else(|| object.get("name"))
        .and_then(Value::as_str)?
        .to_owned();

    let raw = function
        .and_then(|f| f.get("arguments"))
        .or_else(|| object.get("arguments"))
        .or_else(|| object.get("input"))
        .or_else(|| object.get("parameters"));

    // Whether they arrived as a string decides what a replay sends back, so it
    // is recorded before the string is parsed away.
    let arguments_as_text = matches!(raw, Some(Value::String(_)));
    let arguments = raw.map_or(Value::Null, |raw| match raw {
        Value::String(text) => {
            serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
        }
        other => other.clone(),
    });

    let id = object
        .get("id")
        .or_else(|| object.get("tool_use_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    Some(ToolCall {
        id,
        name,
        arguments,
        arguments_as_text,
    })
}

fn decode_finish_reason(raw: &Value, spec: &DecodeSpec, trace: &mut DecodeTrace) -> Option<String> {
    let Some((path, node)) = resolve_one(raw, &spec.finish_reason) else {
        trace.miss(
            DecodeField::FinishReason,
            paths::sources(&spec.finish_reason),
        );
        return None;
    };

    if let Some(reason) = node.as_str() {
        trace.hit(DecodeField::FinishReason, path.source());
        Some(reason.to_owned())
    } else {
        trace.issue(
            DecodeField::FinishReason,
            path.source(),
            format!("expected a string, found {}", type_name(node)),
        );
        None
    }
}

fn decode_usage(raw: &Value, spec: &DecodeSpec, trace: &mut DecodeTrace) -> Option<Usage> {
    let Some((path, node)) = resolve_one(raw, &spec.usage) else {
        trace.miss(DecodeField::Usage, paths::sources(&spec.usage));
        return None;
    };

    trace.hit(DecodeField::Usage, path.source());
    Some(Usage::from_value(node))
}

pub(super) fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(yaml: &str) -> DecodeSpec {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    fn openai_spec() -> DecodeSpec {
        spec(
            r#"
content: ["$.choices[0].message.content", "$.output.text"]
tool_calls: ["$.choices[0].message.tool_calls", "$.content[?(@.type == 'tool_use')]"]
finish_reason: ["$.choices[0].finish_reason", "$.stop_reason"]
usage: ["$.usage"]
"#,
        )
    }

    #[test]
    fn decodes_an_openai_response() {
        let raw = serde_json::json!({
            "choices": [{
                "message": {"role": "assistant", "content": "pong"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 1}
        });

        let (completion, trace) = decode(&raw, &openai_spec());
        assert_eq!(completion.content.as_deref(), Some("pong"));
        assert_eq!(completion.finish_reason.as_deref(), Some("stop"));
        assert_eq!(completion.usage.unwrap().total_tokens, Some(6));
        assert_eq!(
            trace.matched[&DecodeField::Content],
            "$.choices[0].message.content"
        );
    }

    /// The endpoint that does not follow the `OpenAI` shape: content in blocks,
    /// tool calls at the top level with `input`, a differently named stop field.
    #[test]
    fn decodes_a_block_shaped_response_through_the_same_profile() {
        let raw = serde_json::json!({
            "content": [
                {"type": "text", "text": "let me check "},
                {"type": "text", "text": "the weather"},
                {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "Paris"}}
            ],
            "stop_reason": "tool_use"
        });
        let mut spec = openai_spec();
        spec.content = vec![
            "$.choices[0].message.content".parse().unwrap(),
            "$.content[*].text".parse().unwrap(),
        ];

        let (completion, trace) = decode(&raw, &spec);
        assert_eq!(
            completion.content.as_deref(),
            Some("let me check the weather")
        );
        assert_eq!(trace.matched[&DecodeField::Content], "$.content[*].text");
        assert_eq!(completion.finish_reason.as_deref(), Some("tool_use"));

        assert_eq!(completion.tool_calls.len(), 1);
        let call = &completion.tool_calls[0];
        assert_eq!(call.name, "get_weather");
        assert_eq!(call.id.as_deref(), Some("tu_1"));
        assert_eq!(call.arguments, serde_json::json!({"city": "Paris"}));
    }

    #[test]
    fn openai_string_arguments_are_parsed_into_json() {
        let raw = serde_json::json!({
            "choices": [{"message": {"tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\": \"Lyon\"}"}
            }]}}]
        });

        let (completion, _) = decode(&raw, &openai_spec());
        assert_eq!(
            completion.tool_calls[0].arguments,
            serde_json::json!({"city": "Lyon"})
        );
    }

    #[test]
    fn a_missing_path_is_traced_rather_than_fatal() {
        let raw = serde_json::json!({"totally": {"different": "shape"}});

        let (completion, trace) = decode(&raw, &openai_spec());
        assert!(completion.content.is_none());
        assert_eq!(
            trace.missed[&DecodeField::Content],
            vec!["$.choices[0].message.content", "$.output.text"]
        );
    }

    #[test]
    fn a_path_that_resolves_to_the_wrong_type_says_so() {
        let raw = serde_json::json!({"choices": [{"message": {"content": 42}}]});

        let (completion, trace) = decode(&raw, &openai_spec());
        assert!(completion.content.is_none());
        assert_eq!(trace.issues.len(), 1);
        assert!(trace.issues[0].message.contains("found a number"));
    }

    #[test]
    fn an_empty_response_decodes_to_an_empty_completion() {
        let (completion, trace) = decode(&serde_json::json!({}), &openai_spec());
        assert!(completion.content.is_none());
        assert!(completion.tool_calls.is_empty());
        assert!(trace.matched.is_empty());
        assert_eq!(trace.missed.len(), 4);
    }
}
