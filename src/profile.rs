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
}

/// How the request body is produced.
///
/// Exactly one of the two, and the template is the one to reach for: a script is
/// code in a config file, and it earns its place only when the template cannot
/// express the shape.
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
}

impl RequestSpec {
    /// Which of the two was declared.
    #[must_use]
    pub fn source(&self) -> Option<RequestSource<'_>> {
        match (&self.template, &self.script) {
            (Some(template), _) => Some(RequestSource::Template(template)),
            (None, Some(script)) => Some(RequestSource::Script(script)),
            (None, None) => None,
        }
    }
}

fn exactly_one_request_source(spec: &RequestSpec) -> Result<(), ValidationError> {
    match (&spec.template, &spec.script) {
        (Some(template), None) if !template.is_empty() => Ok(()),
        (Some(_), None) => Err(ValidationError::new("empty_template")
            .with_message("`request.template` must not be empty".into())),
        (None, Some(_)) => Ok(()),
        (Some(_), Some(_)) => Err(ValidationError::new("ambiguous_request")
            .with_message("set either `request.template` or `request.script`, not both".into())),
        (None, None) => Err(ValidationError::new("missing_request")
            .with_message("`request` needs a `template` or a `script`".into())),
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
    /// MCP servers whose tools are offered to the model **and really called**.
    ///
    /// Names refer to `mcp.yaml`. This is the one place in `mire` where a run has
    /// effects outside this process, so it is opt-in per profile and never
    /// implied. A simulated tool of the same name wins, which is how you stub one
    /// tool of an otherwise live server.
    #[serde(default)]
    pub mcp: Vec<String>,
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
}
