//! The conversation vocabulary shared by rendering and decoding.
//!
//! Deliberately close to the `OpenAI` shape, because that is what most templates
//! will `tojson` straight into the body — but nothing forces an endpoint to accept
//! it, and a template is free to remap these fields into whatever it needs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

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
    /// What was said. Absent on an assistant turn that only emitted tool calls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Content>,
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
    /// A user turn, carrying text or text and the files sent with it.
    #[must_use]
    pub fn user(content: impl Into<Content>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

/// What a turn carries.
///
/// A turn with nothing attached to it is a plain string, and stays one on the
/// wire — attaching a file to *one* turn is not a reason to change the shape of
/// every other one, and an endpoint that only ever accepted strings keeps
/// working exactly as it did.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Content {
    /// One string. What a conversation with no files in it is made of.
    Text(String),
    /// Several parts: the text, and whatever was attached alongside it.
    Parts(Vec<Part>),
}

impl Content {
    /// The words in this turn, with the attachments left out.
    ///
    /// Text parts are joined by a blank line, because that is where they came
    /// from: separate parts, not a sentence split in two.
    #[must_use]
    pub fn text(&self) -> Option<String> {
        match self {
            Self::Text(text) => Some(text.clone()),
            Self::Parts(parts) => {
                let joined: Vec<&str> = parts
                    .iter()
                    .filter_map(|part| match part {
                        Part::Named(NamedPart::Text { text }) => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                (!joined.is_empty()).then(|| joined.join("\n\n"))
            }
        }
    }

    /// The parts, or nothing when this turn is a plain string.
    #[must_use]
    pub fn parts(&self) -> &[Part] {
        match self {
            Self::Text(_) => &[],
            Self::Parts(parts) => parts,
        }
    }
}

impl From<String> for Content {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<&str> for Content {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<Vec<Part>> for Content {
    fn from(parts: Vec<Part>) -> Self {
        Self::Parts(parts)
    }
}

/// One piece of a turn that carries more than text.
///
/// The named shapes below are `OpenAI`'s, for the same reason [`Message`] is
/// `OpenAI`-shaped: it is what `{{ messages | tojson }}` puts on the wire
/// without a template having to think about it. Anything else is carried
/// through untouched, so an endpoint spelling this differently is a profile
/// question — a template or a request script remaps these like any other field —
/// and never a reason for `mire` to refuse the body.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum Part {
    /// A shape `mire` can name, and therefore show you.
    Named(NamedPart),
    /// Anything else, on the wire exactly as it arrived.
    Other(Value),
}

/// The part shapes `mire` knows by name.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NamedPart {
    /// Words, sitting alongside whatever else this turn carries.
    Text {
        /// The text itself.
        text: String,
    },
    /// An image.
    ImageUrl {
        /// Where it is.
        image_url: ImageUrl,
    },
    /// A file, by name and by content.
    File {
        /// Which file.
        file: FileRef,
    },
}

/// Where an image comes from.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct ImageUrl {
    /// A `data:` URL when the bytes travel with the request, or a plain URL when
    /// the endpoint is expected to go and fetch it itself — which is a different
    /// thing to test, and one this leaves possible.
    pub url: String,
    /// `low`, `high` or `auto`, for the endpoints that read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// A file, inline or by reference.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct FileRef {
    /// Name as it was on disk. Not decoration: several endpoints decide how to
    /// parse a file from its extension and nothing else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// The bytes, as a `data:` URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_data: Option<String>,
    /// An identifier in the endpoint's own file store, for the ones that have
    /// one and expect an upload to have happened first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_id: Option<String>,
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
            content: Some(Content::Text(String::new())),
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

    /// The point of the whole thing: a question plus the file it is about,
    /// in the shape `{{ messages | tojson }}` already puts on the wire.
    #[test]
    fn a_turn_with_a_file_serialises_as_openai_content_parts() {
        let message = Message::user(vec![
            Part::Named(NamedPart::Text {
                text: "what is in this?".to_owned(),
            }),
            Part::Named(NamedPart::ImageUrl {
                image_url: ImageUrl {
                    url: "data:image/png;base64,iVBOR".to_owned(),
                    detail: None,
                },
            }),
            Part::Named(NamedPart::File {
                file: FileRef {
                    filename: Some("report.pdf".to_owned()),
                    file_data: Some("data:application/pdf;base64,JVBER".to_owned()),
                    file_id: None,
                },
            }),
        ]);

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "role": "user",
                "content": [
                    {"type": "text", "text": "what is in this?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,iVBOR"}},
                    {
                        "type": "file",
                        "file": {
                            "filename": "report.pdf",
                            "file_data": "data:application/pdf;base64,JVBER",
                        },
                    },
                ],
            })
        );
    }

    /// A turn with nothing attached must not grow an array around itself. Every
    /// profile written before files existed sends exactly what it always sent.
    #[test]
    fn a_turn_with_no_file_is_still_a_bare_string() {
        let message: Message =
            serde_json::from_value(serde_json::json!({"role": "user", "content": "ping"})).unwrap();

        assert!(matches!(message.content, Some(Content::Text(_))));
        assert_eq!(
            serde_json::to_value(&message).unwrap()["content"],
            serde_json::json!("ping")
        );
    }

    /// `mire` names three shapes because it draws them. It is not the authority
    /// on what an endpoint accepts, so a fourth goes out the way it came in.
    #[test]
    fn a_part_shape_mire_does_not_know_still_goes_out() {
        let message: Message = serde_json::from_value(serde_json::json!({
            "role": "user",
            "content": [{"type": "input_audio", "input_audio": {"data": "UklGR", "format": "wav"}}],
        }))
        .unwrap();

        assert!(matches!(
            message.content.as_ref().unwrap().parts(),
            [Part::Other(_)]
        ));
        assert_eq!(
            serde_json::to_value(&message).unwrap()["content"][0]["input_audio"]["format"],
            serde_json::json!("wav")
        );
    }

    #[test]
    fn the_words_of_a_turn_are_readable_without_its_attachments() {
        let content = Content::Parts(vec![
            Part::Named(NamedPart::Text {
                text: "first".to_owned(),
            }),
            Part::Named(NamedPart::File {
                file: FileRef {
                    filename: Some("a.pdf".to_owned()),
                    file_data: Some("data:application/pdf;base64,JVBER".to_owned()),
                    file_id: None,
                },
            }),
            Part::Named(NamedPart::Text {
                text: "second".to_owned(),
            }),
        ]);

        assert_eq!(content.text().as_deref(), Some("first\n\nsecond"));
        assert_eq!(
            Content::Text("plain".to_owned()).text().as_deref(),
            Some("plain")
        );
    }
}
