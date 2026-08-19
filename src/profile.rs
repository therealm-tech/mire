//! Profiles: one YAML file per model endpoint.
//!
//! The file on disk is the source of truth. `mire` only ever reads it — editing
//! happens in your editor, and [`crate::config::ConfigStore`] picks the change up.

pub mod loader;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json_path::JsonPath;
use url::Url;
use validator::{Validate, ValidationError};

use crate::script::ScriptSource;

/// Default request timeout when a profile does not set `timeout_ms`.
const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// What the endpoint is expected to do, which decides the request template
/// variables, the normalised output shape, and the assertions that apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    /// Question in, answer out. Decodes to [`crate::decode::Completion`].
    Chat,
    /// Text in, vectors out. Decodes to [`crate::decode::Embedding`].
    Embedding,
}

/// HTTP method used to call the endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    /// The default, and what every model endpoint seen so far uses.
    #[default]
    Post,
    /// For endpoints that take their input in the query string.
    Get,
    /// Present because "exotic endpoint" is the whole point of this tool.
    Put,
    /// Idem.
    Patch,
}

impl From<HttpMethod> for reqwest::Method {
    fn from(method: HttpMethod) -> Self {
        match method {
            HttpMethod::Post => Self::POST,
            HttpMethod::Get => Self::GET,
            HttpMethod::Put => Self::PUT,
            HttpMethod::Patch => Self::PATCH,
        }
    }
}

/// A `JSONPath` expression, compiled at load time so a typo is a startup error
/// naming the file and field rather than a silent decode miss at call time.
#[derive(Debug, Clone)]
pub struct JsonPathExpr {
    source: String,
    compiled: JsonPath,
}

impl JsonPathExpr {
    /// The expression as written in the YAML.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// The compiled expression.
    #[must_use]
    pub fn compiled(&self) -> &JsonPath {
        &self.compiled
    }
}

impl std::str::FromStr for JsonPathExpr {
    type Err = serde_json_path::ParseError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        Ok(Self {
            compiled: JsonPath::parse(source)?,
            source: source.to_owned(),
        })
    }
}

impl<'de> Deserialize<'de> for JsonPathExpr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let source = String::deserialize(deserializer)?;
        source.parse().map_err(serde::de::Error::custom)
    }
}

impl Serialize for JsonPathExpr {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.source)
    }
}

impl JsonSchema for JsonPathExpr {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JsonPathExpr".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = String::json_schema(generator);
        schema.insert(
            "description".into(),
            "A JSONPath expression (RFC 9535), e.g. `$.choices[0].message.content`".into(),
        );
        schema
    }
}

/// Where a request body comes from.
#[derive(Debug, Clone, Copy)]
pub enum RequestSource<'a> {
    /// A `MiniJinja` template.
    Template(&'a str),
    /// A Rhai script.
    Script(&'a ScriptSource),
    /// A `multipart/form-data`, one entry per field.
    Multipart(&'a MultipartSpec),
}

/// How the request body is produced.
///
/// Exactly one of the three, and the template is the one to reach for. A script
/// is code in a config file and earns its place only when the template cannot
/// express the shape; `multipart:` is for the endpoints that do not take a JSON
/// document at all — a transcriber, a diariser, anything whose input is bytes
/// with a few knobs beside them.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = exactly_one_request_source))]
pub struct RequestSpec {
    /// `MiniJinja` template rendered against `messages`, `input`, `tools`, `model`
    /// and `params`. Must render to valid JSON.
    #[serde(default)]
    pub template: Option<String>,
    /// Rhai script seeing the same variables and returning the body — a string,
    /// or a map or array that gets serialised to JSON.
    #[serde(default)]
    pub script: Option<ScriptSource>,
    /// Form fields, in the order the file wrote them. See [`MultipartSpec`].
    #[serde(default)]
    pub multipart: Option<MultipartSpec>,
}

impl RequestSpec {
    /// Which of the three was declared.
    #[must_use]
    pub fn source(&self) -> Option<RequestSource<'_>> {
        match (&self.template, &self.script, &self.multipart) {
            (Some(template), _, _) => Some(RequestSource::Template(template)),
            (None, Some(script), _) => Some(RequestSource::Script(script)),
            (None, None, Some(multipart)) => Some(RequestSource::Multipart(multipart)),
            (None, None, None) => None,
        }
    }
}

/// A `multipart/form-data` body, one entry per form field.
///
/// A list rather than a map, because the wire is a list: parts go out in the
/// order the file wrote them. Deserialised *from* a YAML mapping, which is the
/// shape anybody writing one expects — see the [`Deserialize`] impl.
///
/// ```yaml
/// request:
///   multipart:
///     file:
///       upload: '{{ uploads[0] }}'
///     model: whisper-1
///     response_format: json
/// ```
#[derive(Debug, Clone, Default)]
pub struct MultipartSpec(Vec<PartSpec>);

impl MultipartSpec {
    /// The fields, in the order the file declared them.
    #[must_use]
    pub fn parts(&self) -> &[PartSpec] {
        &self.0
    }
}

/// One field of a multipart body.
#[derive(Debug, Clone)]
pub struct PartSpec {
    /// The form field, as the profile named it.
    pub field: String,
    /// What it carries.
    pub part: PartKind,
}

/// What one field of a form carries: text, or files.
///
/// The two are told apart by which key the file used, never by guessing at the
/// rendered value. A `model: whisper-1` that quietly became a file part because
/// something in the upload directory happened to be called `whisper-1` is
/// exactly the kind of surprise this tool exists to not have.
#[derive(Debug, Clone)]
pub enum PartKind {
    /// A text field: one `MiniJinja` template, rendered against the same context
    /// a `template:` sees.
    Text {
        /// The template. Rendered per call, like everything else here.
        template: String,
        /// `content-type` of the part, when the endpoint wants one declared —
        /// `application/json` for the config blob some diarisers take. Left off,
        /// the part goes out as a plain form field.
        media_type: Option<String>,
    },
    /// A file field: templates naming uploads of the call.
    File {
        /// Templates, each naming one or more uploads. A field carrying several
        /// files sends several parts under the same name, which is what every
        /// server-side upload handler already reads.
        sources: Vec<String>,
        /// `content-type` of the part, overriding the one guessed from the
        /// extension. The guess is an extension lookup and nothing more; a
        /// profile that knows better says so.
        media_type: Option<String>,
        /// `filename` of the part, overriding the stored name.
        ///
        /// Worth having because some transcription endpoints refuse a file whose
        /// name carries no extension they recognise, and the name on disk is
        /// whatever the browser handed over. Only for a field carrying exactly
        /// one file — two parts under one filename is a form nobody meant.
        filename: Option<String>,
    },
}

/// The wire shape of one field: the scalar shorthand, or the long form.
///
/// `model: whisper-1` is the common case and stays one line. The long form is
/// what a file field needs, and what a text field reaches for when it has a
/// `type:` to declare.
///
/// Hand-written rather than `#[serde(untagged)]` so that a typo inside the long
/// form is reported as one. Untagged, a stray `typ:` makes the whole variant
/// fail to match and the error becomes "data did not match any variant" — which
/// is precisely the message somebody reads three times before finding the typo.
#[derive(Debug, Clone)]
enum PartConfig {
    /// `field: <template>` — a text part, and nothing else to say about it.
    Shorthand(Scalar),
    /// `field: {text: …}` or `field: {upload: …}`, with an optional `type:`.
    Long(LongPart),
}

impl<'de> Deserialize<'de> for PartConfig {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Either;

        impl<'de> serde::de::Visitor<'de> for Either {
            type Value = PartConfig;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a form field: a value, or a mapping with `text` or `upload`")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                LongPart::deserialize(serde::de::value::MapAccessDeserializer::new(map))
                    .map(PartConfig::Long)
            }
        }

        // Every scalar spelling routes through `Scalar`, so `model: whisper-1`
        // and `temperature: 0` are read the same way and by the same code.
        struct Both(Either);

        impl<'de> serde::de::Visitor<'de> for Both {
            type Value = PartConfig;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.expecting(formatter)
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                map: A,
            ) -> Result<Self::Value, A::Error> {
                self.0.visit_map(map)
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(PartConfig::Shorthand(Scalar(value.to_owned())))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(PartConfig::Shorthand(Scalar(value.to_string())))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(PartConfig::Shorthand(Scalar(value.to_string())))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(PartConfig::Shorthand(Scalar(value.to_string())))
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(PartConfig::Shorthand(Scalar(value.to_string())))
            }
        }

        deserializer.deserialize_any(Both(Either))
    }
}

impl JsonSchema for PartConfig {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "PartConfig".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let long = generator.subschema_for::<LongPart>();
        schemars::json_schema!({
            "description": "A text part written as a bare value, or the long form of either kind.",
            "anyOf": [
                {"type": ["string", "number", "boolean"]},
                long,
            ],
        })
    }
}

/// A form field's value, as the file wrote it.
///
/// Any scalar, not just a string: `temperature: 0` and `translate: false` are
/// how anybody writes a knob in YAML, and every part of a form goes out as text
/// regardless. Refusing the number would be refusing the natural spelling for
/// nothing gained on the wire.
#[derive(Debug, Clone)]
struct Scalar(String);

impl<'de> Deserialize<'de> for Scalar {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct AnyScalar;

        impl serde::de::Visitor<'_> for AnyScalar {
            type Value = Scalar;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a string, a number or a boolean")
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(Scalar(value.to_owned()))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(Scalar(value.to_string()))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                Ok(Scalar(value.to_string()))
            }

            fn visit_f64<E: serde::de::Error>(self, value: f64) -> Result<Self::Value, E> {
                Ok(Scalar(value.to_string()))
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(Scalar(value.to_string()))
            }
        }

        deserializer.deserialize_any(AnyScalar)
    }
}

impl Serialize for Scalar {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl JsonSchema for Scalar {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Scalar".into()
    }

    fn json_schema(_generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": ["string", "number", "boolean"],
            "description": "A form field's value. Rendered as a template, sent as text.",
        })
    }
}

/// The long form of a field. Exactly one of `text:` and `upload:`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct LongPart {
    /// A text field's template.
    #[serde(default)]
    text: Option<Scalar>,
    /// A file field's templates, each naming uploads of the call. One, or a list.
    #[serde(default)]
    upload: Option<OneOrMany>,
    /// `content-type` of the part.
    #[serde(default, rename = "type")]
    media_type: Option<String>,
    /// `filename` of the part. File fields only.
    #[serde(default)]
    filename: Option<String>,
}

/// One template, or several. The short form is the common one and stays short.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(untagged)]
enum OneOrMany {
    /// One template.
    One(String),
    /// Several, in order.
    Many(Vec<String>),
}

impl OneOrMany {
    fn into_vec(self) -> Vec<String> {
        match self {
            Self::One(source) => vec![source],
            Self::Many(sources) => sources,
        }
    }
}

impl PartConfig {
    /// The part this field declares, or why it declares nothing usable.
    fn into_kind(self, field: &str) -> Result<PartKind, String> {
        let long = match self {
            Self::Shorthand(Scalar(template)) => {
                return Ok(PartKind::Text {
                    template,
                    media_type: None,
                });
            }
            Self::Long(long) => long,
        };

        match (long.text, long.upload) {
            (Some(_), Some(_)) => Err(format!(
                "`{field}` sets both `text` and `upload`; a part carries one or the other"
            )),
            (None, None) => Err(format!(
                "`{field}` sets neither `text` nor `upload`, so it would carry nothing"
            )),
            (Some(Scalar(template)), None) => {
                if long.filename.is_some() {
                    return Err(format!(
                        "`{field}` sets `filename` on a `text` part, which has no file to name"
                    ));
                }
                Ok(PartKind::Text {
                    template,
                    media_type: long.media_type,
                })
            }
            (None, Some(upload)) => {
                let sources = upload.into_vec();
                // An empty list is a field that will never carry anything, which
                // is a `422` waiting for the first call rather than something to
                // find out at startup.
                if sources.is_empty() {
                    return Err(format!("`{field}` names no file"));
                }
                Ok(PartKind::File {
                    sources,
                    media_type: long.media_type,
                    filename: long.filename,
                })
            }
        }
    }
}

impl<'de> Deserialize<'de> for MultipartSpec {
    /// Reads a YAML mapping, keeping the order it was written in.
    ///
    /// A [`BTreeMap`] would be four characters and would silently sort the form
    /// alphabetically. Most parsers do not care, right up to the one that does,
    /// and "the parts went out in a different order than the file lists them" is
    /// a bad afternoon in a tool whose entire promise is that the request is
    /// written down.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Fields;

        impl<'de> serde::de::Visitor<'de> for Fields {
            type Value = Vec<PartSpec>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a mapping of form field names to parts")
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<Self::Value, A::Error> {
                let mut parts = Vec::new();
                while let Some((field, config)) = map.next_entry::<String, PartConfig>()? {
                    let part = config.into_kind(&field).map_err(serde::de::Error::custom)?;
                    parts.push(PartSpec { field, part });
                }
                Ok(parts)
            }
        }

        deserializer.deserialize_map(Fields).map(Self)
    }
}

impl Serialize for MultipartSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;

        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for spec in &self.0 {
            match &spec.part {
                PartKind::Text {
                    template,
                    media_type,
                } => map.serialize_entry(
                    &spec.field,
                    &LongPart {
                        text: Some(Scalar(template.clone())),
                        upload: None,
                        media_type: media_type.clone(),
                        filename: None,
                    },
                )?,
                PartKind::File {
                    sources,
                    media_type,
                    filename,
                } => map.serialize_entry(
                    &spec.field,
                    &LongPart {
                        text: None,
                        upload: Some(OneOrMany::Many(sources.clone())),
                        media_type: media_type.clone(),
                        filename: filename.clone(),
                    },
                )?,
            }
        }
        map.end()
    }
}

impl JsonSchema for MultipartSpec {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "MultipartSpec".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let part = generator.subschema_for::<PartConfig>();
        schemars::json_schema!({
            "type": "object",
            "description": "Form fields, in the order they are written. A scalar is a text part; `upload:` makes it a file part.",
            "additionalProperties": part,
        })
    }
}

fn exactly_one_request_source(spec: &RequestSpec) -> Result<(), ValidationError> {
    let declared = u8::from(spec.template.is_some())
        + u8::from(spec.script.is_some())
        + u8::from(spec.multipart.is_some());

    if declared > 1 {
        return Err(ValidationError::new("ambiguous_request").with_message(
            "set one of `request.template`, `request.script` or `request.multipart` — a request is one body".into(),
        ));
    }

    match (&spec.template, &spec.script, &spec.multipart) {
        (Some(template), ..) if template.is_empty() => Err(ValidationError::new("empty_template")
            .with_message("`request.template` must not be empty".into())),
        (_, _, Some(multipart)) if multipart.parts().is_empty() => {
            Err(ValidationError::new("empty_multipart")
                .with_message("`request.multipart` declares no field".into()))
        }
        (None, None, None) => Err(ValidationError::new("missing_request")
            .with_message("`request` needs a `template`, a `script` or a `multipart`".into())),
        _ => Ok(()),
    }
}

/// Where each normalised field lives in the endpoint's response.
///
/// Every field is a *cascade*: paths are tried in order and the first one that
/// resolves wins. A field with no configured path, or whose paths all miss, is
/// simply absent from the decoded output — the raw response and the decode trace
/// are what you look at then.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = paths_or_script))]
pub struct DecodeSpec {
    /// Rhai script replacing the cascades entirely, for a response no set of
    /// paths can describe. It receives `raw`, `status` and `headers`, and returns
    /// a map: `content` / `tool_calls` / `finish_reason` / `usage` for a chat
    /// profile, `vectors` / `usage` for an embedding one.
    #[serde(default)]
    pub script: Option<ScriptSource>,
    /// Assistant text. `kind: chat`.
    #[serde(default)]
    pub content: Vec<JsonPathExpr>,
    /// Assistant text inside **one chunk** of a streamed response. `kind: chat`.
    ///
    /// Streaming needs its own cascade because the chunk shape is not the whole
    /// response's: `OpenAI` moves the text from `message.content` to
    /// `delta.content`, and Ollama's native API keeps `message.content` but sends
    /// one object per line. Without this, a profile streams and decodes nothing.
    ///
    /// A `decode.script` replaces the cascades for a whole body; it is not run
    /// per chunk, so a scripted profile streams without text deltas.
    #[serde(default)]
    pub delta: Vec<JsonPathExpr>,
    /// Tool calls emitted by the model. `kind: chat`.
    #[serde(default)]
    pub tool_calls: Vec<JsonPathExpr>,
    /// Why generation stopped. `kind: chat`.
    #[serde(default)]
    pub finish_reason: Vec<JsonPathExpr>,
    /// Token accounting. Both kinds.
    #[serde(default)]
    pub usage: Vec<JsonPathExpr>,
    /// What the endpoint says went wrong. Both kinds.
    ///
    /// Point it at the node carrying the complaint — `$.error` for almost
    /// everything, `$.detail` for a gateway in front of it — and a refusal comes
    /// back with the sentence normalised out of it instead of only as raw JSON.
    /// The status is not consulted: an endpoint answering `200` with an error in
    /// the body is precisely what this is for.
    #[serde(default)]
    pub error: Vec<JsonPathExpr>,
    /// The vectors themselves. `kind: embedding`.
    #[serde(default)]
    pub vectors: Vec<JsonPathExpr>,
}

impl DecodeSpec {
    /// Returns `true` when nothing is configured at all — the state of a profile
    /// you have not taught to decode yet, which is valid.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.script.is_none() && !self.has_paths()
    }

    /// Returns `true` when at least one cascade is configured.
    #[must_use]
    pub fn has_paths(&self) -> bool {
        !(self.content.is_empty()
            && self.delta.is_empty()
            && self.tool_calls.is_empty()
            && self.finish_reason.is_empty()
            && self.usage.is_empty()
            && self.error.is_empty()
            && self.vectors.is_empty())
    }
}

/// A script takes over the whole decode, so declaring both is a mistake worth
/// naming rather than a precedence rule to remember.
fn paths_or_script(spec: &DecodeSpec) -> Result<(), ValidationError> {
    if spec.script.is_some() && spec.has_paths() {
        return Err(ValidationError::new("ambiguous_decode").with_message(
            "set either `decode` paths or `decode.script`, not both — a script replaces the cascades".into(),
        ));
    }
    Ok(())
}

/// Predicates that end an agent loop. Combined with OR; an empty spec means
/// "stop when there are no tool calls".
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct StopWhen {
    /// Stop as soon as a turn produces no tool calls.
    #[serde(default = "default_true")]
    pub no_tool_calls: bool,
    /// Stop when `finish_reason` is one of these values.
    #[serde(default)]
    pub finish_reason_in: Vec<String>,
    /// Stop when the model asks for the same tool with the same arguments
    /// twice. Off unless asked for: a model that re-reads a tool it already
    /// called is often working, not looping, and `max_iterations` already
    /// bounds the run.
    #[serde(default)]
    pub repeated_call: bool,
}

impl Default for StopWhen {
    fn default() -> Self {
        Self {
            no_tool_calls: true,
            finish_reason_in: Vec::new(),
            repeated_call: false,
        }
    }
}

/// Agent-loop configuration. Only meaningful for `kind: chat`.
///
/// Absent means the defaults: stop when there are no tool calls, ten turns at most.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct AgentSpec {
    /// When to stop looping.
    #[serde(default)]
    pub stop_when: StopWhen,
    /// Hard cap on turns.
    #[serde(default = "default_max_iterations")]
    #[validate(range(min = 1, max = 100))]
    pub max_iterations: u32,
    /// Hard cap on wall-clock time for the whole loop.
    #[serde(default)]
    pub max_duration_ms: Option<u64>,
}

/// What a simulated tool answers with.
#[derive(Debug, Clone, Copy)]
pub enum ToolResponse<'a> {
    /// A fixed string.
    Static(&'a str),
    /// A Rhai script, seeing the call's `arguments`, `name` and `turn`.
    Script(&'a ScriptSource),
}

/// A simulated tool: the model may call it, and gets a canned result back.
///
/// Nothing is ever executed. The point is to check that the model emits calls
/// matching the declared schema and knows what to do with a result — not to do
/// any actual work.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = exactly_one_tool_response))]
pub struct ToolSpec {
    /// Tool name, as advertised to the model.
    #[validate(length(min = 1))]
    pub name: String,
    /// What the tool is for. Passed to the model, which is what makes it call
    /// the tool at the right moment.
    #[serde(default)]
    pub description: Option<String>,
    /// JSON Schema of the arguments, used both to advertise the tool and to
    /// check what the model sends back.
    pub schema: serde_json::Value,
    /// Canned result handed back to the model.
    #[serde(default)]
    pub response: Option<String>,
    /// Rhai script producing the result, for a tool whose answer should depend
    /// on its arguments.
    #[serde(default)]
    pub script: Option<ScriptSource>,
}

impl ToolSpec {
    /// Which of the two was declared.
    #[must_use]
    pub fn answer(&self) -> Option<ToolResponse<'_>> {
        match (&self.response, &self.script) {
            (Some(response), _) => Some(ToolResponse::Static(response)),
            (None, Some(script)) => Some(ToolResponse::Script(script)),
            (None, None) => None,
        }
    }
}

fn exactly_one_tool_response(spec: &ToolSpec) -> Result<(), ValidationError> {
    match (&spec.response, &spec.script) {
        (Some(_), None) | (None, Some(_)) => Ok(()),
        (Some(_), Some(_)) => Err(ValidationError::new("ambiguous_tool").with_message(
            format!(
                "tool `{}`: set either `response` or `script`, not both",
                spec.name
            )
            .into(),
        )),
        (None, None) => Err(ValidationError::new("missing_tool_response")
            .with_message(format!("tool `{}` needs a `response` or a `script`", spec.name).into())),
    }
}

/// Shape the endpoint is expected to produce, checked by assertions.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExpectSpec {
    /// Expected vector width. `kind: embedding`.
    #[serde(default)]
    pub dimensions: Option<usize>,
}

/// One model endpoint, as declared in one YAML file.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    /// Identifier used in the API and the UI. Must be unique across the directory.
    #[validate(length(min = 1, message = "a profile needs a name"))]
    pub name: String,
    /// What the endpoint does.
    pub kind: ProfileKind,
    /// Full endpoint URL. Pointing anywhere is the feature, not an oversight.
    pub url: Url,
    /// HTTP method.
    #[serde(default)]
    pub method: HttpMethod,
    /// Name of an entry in the auth registry. Absent means anonymous.
    #[serde(default)]
    pub auth: Option<String>,
    /// Extra headers sent verbatim. Never put a credential here.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Request timeout in milliseconds.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
    /// How the body is built.
    #[validate(nested)]
    pub request: RequestSpec,
    /// How the response is read.
    #[serde(default)]
    #[validate(nested)]
    pub decode: DecodeSpec,
    /// Agent-loop configuration.
    #[serde(default)]
    #[validate(nested)]
    pub agent: Option<AgentSpec>,
    /// Simulated tools offered to the model. Nothing is executed.
    #[serde(default)]
    #[validate(nested)]
    pub tools: Vec<ToolSpec>,
    /// Expected response shape.
    #[serde(default)]
    pub expect: ExpectSpec,
    /// File this profile was read from. Set by the loader, never present in YAML.
    #[serde(skip_deserializing, default)]
    pub source: PathBuf,
}

impl Profile {
    /// The configured timeout.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

fn default_timeout_ms() -> u64 {
    DEFAULT_TIMEOUT_MS
}

fn default_max_iterations() -> u32 {
    10
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHAT_YAML: &str = r#"
name: mistral-small
kind: chat
url: https://models.internal/mistral-small/v1/chat/completions
auth: oidc-workload
timeout_ms: 30000
request:
  template: '{"model": "mistral-small", "messages": {{ messages | tojson }}}'
decode:
  content: ["$.choices[0].message.content", "$.output.text"]
  finish_reason: ["$.choices[0].finish_reason"]
  error: ["$.error", "$.detail"]
agent:
  stop_when:
    no_tool_calls: true
    finish_reason_in: [stop, end_turn]
  max_iterations: 10
tools:
  - name: get_weather
    schema:
      type: object
      properties:
        city:
          type: string
      required: [city]
    response: '{"temp": 21}'
"#;

    #[test]
    fn parses_a_chat_profile() {
        let profile: Profile = serde_yaml_ng::from_str(CHAT_YAML).unwrap();
        assert_eq!(profile.kind, ProfileKind::Chat);
        assert_eq!(profile.method, HttpMethod::Post);
        assert_eq!(profile.timeout(), Duration::from_secs(30));
        assert_eq!(profile.decode.content.len(), 2);
        assert_eq!(
            profile.decode.content[0].source(),
            "$.choices[0].message.content"
        );
        assert_eq!(profile.decode.error[1].source(), "$.detail");
        assert_eq!(profile.tools[0].name, "get_weather");
        // Declared without `repeated_call`, so the loop does not watch for one.
        assert!(!profile.agent.as_ref().unwrap().stop_when.repeated_call);
        profile.validate().unwrap();
    }

    #[test]
    fn watching_for_a_repeated_call_is_opt_in() {
        let yaml = CHAT_YAML.replace(
            "    finish_reason_in: [stop, end_turn]",
            "    finish_reason_in: [stop, end_turn]\n    repeated_call: true",
        );

        let profile: Profile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(profile.agent.unwrap().stop_when.repeated_call);
    }

    #[test]
    fn a_profile_without_decode_is_valid() {
        let yaml = r#"
name: unknown-shape
kind: chat
url: https://models.internal/whatever
request:
  template: '{"prompt": {{ messages | tojson }}}'
"#;
        let profile: Profile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(profile.decode.is_empty());
        assert!(profile.auth.is_none());
    }

    #[test]
    fn an_unknown_key_is_rejected_by_name() {
        let yaml = r"
name: typo
kind: chat
url: https://models.internal/whatever
timout_ms: 1000
request:
  template: '{}'
";
        let error = serde_yaml_ng::from_str::<Profile>(yaml).unwrap_err();
        assert!(error.to_string().contains("timout_ms"), "{error}");
    }

    #[test]
    fn a_bad_jsonpath_is_rejected_at_load() {
        let yaml = r#"
name: bad-path
kind: chat
url: https://models.internal/whatever
request:
  template: '{}'
decode:
  content: ["not a json path"]
"#;
        let error = serde_yaml_ng::from_str::<Profile>(yaml).unwrap_err();
        // The point is that the failure names the offending field, so the loader can
        // point at a file and a key rather than at "somewhere in your YAML".
        assert!(error.to_string().contains("decode.content"), "{error}");
    }

    /// A profile whose `request.multipart:` is the given YAML fragment, parsed
    /// and validated the way the loader does it.
    fn with_multipart(fields: &str) -> Result<Profile, String> {
        let yaml = format!(
            "name: transcribe\nkind: chat\nurl: https://models.internal/v1/audio/transcriptions\nrequest:\n  multipart:\n{fields}"
        );
        let profile: Profile = serde_yaml_ng::from_str(&yaml).map_err(|error| error.to_string())?;
        profile.validate().map_err(|error| error.to_string())?;
        Ok(profile)
    }

    #[test]
    fn a_scalar_field_is_a_text_part_and_upload_makes_it_a_file() {
        let profile =
            with_multipart("    model: whisper-1\n    file:\n      upload: '{{ uploads[0] }}'\n")
                .expect("it loads");

        let RequestSource::Multipart(spec) = profile.request.source().unwrap() else {
            panic!("a multipart source");
        };
        let parts = spec.parts();
        assert_eq!(parts.len(), 2);

        assert_eq!(parts[0].field, "model");
        assert!(matches!(
            &parts[0].part,
            PartKind::Text { template, media_type }
                if template == "whisper-1" && media_type.is_none()
        ));

        assert_eq!(parts[1].field, "file");
        assert!(matches!(
            &parts[1].part,
            PartKind::File { sources, .. } if sources == &["{{ uploads[0] }}"]
        ));
    }

    /// The one thing a map cannot be trusted with, checked: the order survives
    /// the parse.
    #[test]
    fn the_field_order_survives_loading() {
        let profile = with_multipart("    z: 1\n    a: 2\n    m: 3\n").expect("it loads");
        let RequestSource::Multipart(spec) = profile.request.source().unwrap() else {
            panic!("a multipart source");
        };

        let fields: Vec<&str> = spec.parts().iter().map(|p| p.field.as_str()).collect();
        assert_eq!(fields, vec!["z", "a", "m"]);
    }

    /// `temperature: 0` is how anybody writes a knob, and a form sends text
    /// either way. Refusing the number would be refusing the natural spelling.
    #[test]
    fn a_knob_can_be_written_as_a_number_or_a_boolean() {
        let profile =
            with_multipart("    temperature: 0.2\n    speakers: 2\n    translate: false\n")
                .expect("it loads");
        let RequestSource::Multipart(spec) = profile.request.source().unwrap() else {
            panic!("a multipart source");
        };

        let rendered: Vec<&str> = spec
            .parts()
            .iter()
            .map(|part| match &part.part {
                PartKind::Text { template, .. } => template.as_str(),
                PartKind::File { .. } => panic!("a text part"),
            })
            .collect();
        assert_eq!(rendered, vec!["0.2", "2", "false"]);
    }

    /// A typo inside the long form is reported as a typo. The reason this is a
    /// test and not a shrug: untagged, it would come back as "data did not match
    /// any variant", which names nothing and sends the reader hunting.
    #[test]
    fn a_misspelled_key_in_the_long_form_is_named() {
        let error = with_multipart("    file:\n      uploads: '{{ uploads[0] }}'\n")
            .expect_err("`uploads` is not `upload`");
        assert!(error.contains("uploads"), "{error}");
        assert!(!error.contains("did not match any variant"), "{error}");
    }

    #[test]
    fn a_field_that_is_both_text_and_upload_is_refused_at_load() {
        let error =
            with_multipart("    file:\n      text: hello\n      upload: '{{ uploads[0] }}'\n")
                .expect_err("a part carries one or the other");
        assert!(error.contains("both `text` and `upload`"), "{error}");
    }

    #[test]
    fn a_field_that_carries_nothing_is_refused_at_load() {
        let error = with_multipart("    file:\n      type: audio/wav\n")
            .expect_err("a `type` is not a payload");
        assert!(error.contains("neither `text` nor `upload`"), "{error}");
    }

    /// A `filename:` on a text part is a line whose author expected something
    /// else to happen, which is worth saying at startup rather than ignoring.
    #[test]
    fn a_filename_on_a_text_part_is_refused_at_load() {
        let error = with_multipart("    model:\n      text: whisper-1\n      filename: x.wav\n")
            .expect_err("a text part has no file to name");
        assert!(error.contains("`filename`"), "{error}");
    }

    #[test]
    fn a_multipart_beside_a_template_is_refused_at_load() {
        let yaml = "name: t\nkind: chat\nurl: https://models.internal/v1\nrequest:\n  template: '{}'\n  multipart:\n    model: whisper-1\n";
        let profile: Profile = serde_yaml_ng::from_str(yaml).unwrap();
        let error = profile.validate().unwrap_err().to_string();
        assert!(error.contains("a request is one body"), "{error}");
    }

    #[test]
    fn an_empty_multipart_is_refused_at_load() {
        let yaml =
            "name: t\nkind: chat\nurl: https://models.internal/v1\nrequest:\n  multipart: {}\n";
        let profile: Profile = serde_yaml_ng::from_str(yaml).unwrap();
        let error = profile.validate().unwrap_err().to_string();
        assert!(error.contains("declares no field"), "{error}");
    }
}
