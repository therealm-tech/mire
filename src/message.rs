//! The conversation vocabulary shared by rendering and decoding.
//!
//! Deliberately close to the `OpenAI` shape, because that is what most templates
//! will `tojson` straight into the body — but nothing forces an endpoint to accept
//! it, and a template is free to remap these fields into whatever it needs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Who a message is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// System prompt.
    System,
    /// The caller.
    User,
    /// The model.
    Assistant,
    /// The result of a simulated tool, fed back in agent mode.
    Tool,
}

/// One conversation turn.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct Message {
    /// Who is speaking.
    pub role: Role,
    /// Text content. Absent on an assistant turn that only emitted tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Tool calls emitted by the model on this turn.
    ///
    /// Serialised in **wire** shape, not in the normalised one: this field is
    /// read back by an endpoint, and every endpoint seen so far insists on the
    /// nesting. See [`wire`].
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        serialize_with = "wire::serialize",
        deserialize_with = "wire::deserialize"
    )]
    #[schemars(with = "Vec<wire::WireToolCall>")]
    pub tool_calls: Vec<ToolCall>,
    /// Which tool call this message answers. Set on `role: tool`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    /// A user turn carrying plain text.
    #[must_use]
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// A tool call as emitted by the model.
///
/// `arguments` is always **parsed**: an endpoint that sends them as a JSON string
/// has that string parsed here, so validating them against the declared schema —
/// one of the two things agent mode exists to check — works the same either way.
/// How they arrived is remembered in [`Self::arguments_as_text`], because
/// replaying them has to give them back the way that endpoint wants them.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ToolCall {
    /// Provider-assigned identifier, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Tool name.
    pub name: String,
    /// Arguments, parsed.
    pub arguments: serde_json::Value,
    /// Whether the endpoint encoded the arguments as a JSON string.
    ///
    /// Not part of the API surface — it is a fact about the endpoint, not about
    /// the call — but it decides what a replay sends. `OpenAI` wants a string
    /// and rejects an object; Ollama's native API wants an object and rejects a
    /// string. Handing back what arrived is the only rule that satisfies both.
    #[serde(default, skip_serializing)]
    #[schemars(skip)]
    pub arguments_as_text: bool,
}

/// Turning tool calls back into something an endpoint will accept.
///
/// The normalised [`ToolCall`] is for reading — the UI, the trace, the schema
/// check. It is **not** what goes back on the wire, and the difference is not
/// cosmetic: measured against a local Ollama, the flat shape
/// `{"name": …, "arguments": {…}}` is refused by both of its endpoints, with
/// `400 invalid tool call arguments`.
///
/// What both accept is the nesting below. What they disagree about is
/// `arguments`, so that part is handed back exactly as it arrived.
pub mod wire {
    use schemars::JsonSchema;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use serde_json::Value;

    use super::ToolCall;

    /// A tool call in the shape an endpoint reads back.
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    pub struct WireToolCall {
        /// Provider-assigned identifier, echoed so `tool_call_id` can match it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub id: Option<String>,
        /// Always `function`. Present because `OpenAI` requires it.
        #[serde(rename = "type", default = "function")]
        pub kind: String,
        /// Name and arguments.
        pub function: WireFunction,
    }

    /// The call itself.
    #[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
    pub struct WireFunction {
        /// Tool name.
        pub name: String,
        /// Arguments, encoded the way the endpoint that produced them wants them
        /// back: a JSON string for `OpenAI`, an object for Ollama's own API.
        pub arguments: Value,
    }

    fn function() -> String {
        "function".to_owned()
    }

    impl From<&ToolCall> for WireToolCall {
        fn from(call: &ToolCall) -> Self {
            let arguments = if call.arguments_as_text {
                Value::String(call.arguments.to_string())
            } else {
                call.arguments.clone()
            };
            Self {
                id: call.id.clone(),
                kind: function(),
                function: WireFunction {
                    name: call.name.clone(),
                    arguments,
                },
            }
        }
    }

    /// Writes the nested shape.
    ///
    /// # Errors
    ///
    /// Only if the underlying serializer fails.
    pub fn serialize<S: Serializer>(calls: &[ToolCall], serializer: S) -> Result<S::Ok, S::Error> {
        let wire: Vec<WireToolCall> = calls.iter().map(WireToolCall::from).collect();
        wire.serialize(serializer)
    }

    /// Reads either shape.
    ///
    /// Tolerant on the way in for the same reason the decoder is: this accepts a
    /// conversation somebody pasted back from a response, and refusing the shape
    /// we just emitted would be an unkind way to find that out.
    ///
    /// # Errors
    ///
    /// Only if the input is not a list of objects.
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<ToolCall>, D::Error> {
        let values = Vec::<Value>::deserialize(deserializer)?;
        Ok(values
            .iter()
            .filter_map(crate::decode::chat::tool_call_from_value)
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(arguments: serde_json::Value, as_text: bool) -> ToolCall {
        ToolCall {
            id: Some("call_1".to_owned()),
            name: "get_weather".to_owned(),
            arguments,
            arguments_as_text: as_text,
        }
    }

    /// Measured against a local Ollama: the flat shape this used to emit is
    /// refused by `/v1/chat/completions` *and* by `/api/chat`, so the second turn
    /// of every agent run died with a `400`.
    #[test]
    fn tool_calls_go_back_out_nested_under_function() {
        let message = Message {
            role: Role::Assistant,
            content: Some(String::new()),
            tool_calls: vec![call(serde_json::json!({"city": "Lyon"}), false)],
            tool_call_id: None,
        };

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(
            json["tool_calls"][0],
            serde_json::json!({
                "id": "call_1",
                "type": "function",
                "function": {"name": "get_weather", "arguments": {"city": "Lyon"}},
            })
        );
    }

    /// The one thing the two endpoints disagree about. `OpenAI` rejects an
    /// object here; Ollama's native API rejects a string. Handing back what
    /// arrived is what satisfies both without a profile knob.
    #[test]
    fn arguments_are_handed_back_the_way_they_arrived() {
        let message = Message {
            role: Role::Assistant,
            content: None,
            tool_calls: vec![call(serde_json::json!({"city": "Lyon"}), true)],
            tool_call_id: None,
        };

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(
            json["tool_calls"][0]["function"]["arguments"],
            serde_json::json!(r#"{"city":"Lyon"}"#)
        );
    }

    #[test]
    fn a_conversation_pasted_back_in_is_understood() {
        // Exactly what the previous test emits, read again.
        let message: Message = serde_json::from_value(serde_json::json!({
            "role": "assistant",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "get_weather", "arguments": "{\"city\":\"Lyon\"}"},
            }],
        }))
        .unwrap();

        assert_eq!(message.tool_calls[0].name, "get_weather");
        // Parsed, so the schema check can look inside it.
        assert_eq!(message.tool_calls[0].arguments["city"], "Lyon");
        assert!(message.tool_calls[0].arguments_as_text);
    }

    #[test]
    fn a_plain_user_turn_serialises_without_empty_fields() {
        let json = serde_json::to_value(Message::user("hello")).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"role": "user", "content": "hello"})
        );
    }
}
