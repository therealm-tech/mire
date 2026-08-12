//! Turning a profile plus some input into an actual HTTP request.
//!
//! The body comes from a `MiniJinja` template, which is the declarative level: you
//! get `messages`, `input`, `tools`, `model` and `params`, and you emit whatever
//! JSON your endpoint wants.
//!
//! Rendering validates that the result is JSON. A template with a stray comma —
//! the classic `{% if tools %}"tools": …,{% endif %}` — fails here with the
//! rendered text attached, rather than as a bewildering `400` from the endpoint.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use minijinja::Environment;
use reqwest::header::HeaderMap;
use rhai::Scope;
use serde::Serialize;
use serde_json::{Map, Value};
use url::Url;

use crate::script::to_dynamic;

use crate::message::Message;
use crate::profile::{HttpMethod, Profile, RequestSource, ToolSpec};
use crate::redact::Redactor;
use crate::script::{ScriptError, ScriptSource};

/// One `MiniJinja` environment for the whole process: templates are rendered from
/// source strings, so there is nothing per-profile to register.
static ENVIRONMENT: LazyLock<Environment<'static>> = LazyLock::new(Environment::new);

/// What a template can see.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RenderContext {
    /// Conversation so far. `kind: chat`.
    pub messages: Vec<Message>,
    /// Text to embed. `kind: embedding`. A single string still arrives as a list;
    /// use `input[0]` in the template if your endpoint refuses arrays.
    pub input: Vec<String>,
    /// Simulated tools, in `OpenAI` function shape so `{{ tools | tojson }}` works
    /// as-is. Remap it in the template for an endpoint that wants something else.
    pub tools: Vec<Value>,
    /// Model identifier, when the caller overrides the one baked into the template.
    pub model: Option<String>,
    /// Free-form knobs (`max_tokens`, `temperature`, …), reachable as `params.x`.
    pub params: Map<String, Value>,
    /// Whether this call was asked to stream.
    ///
    /// Exposed to the template because the endpoint has to be *told* — nothing
    /// `mire` does client-side makes a response arrive in chunks. Write
    /// `"stream": {{ stream | tojson }}` and one profile serves both modes;
    /// hard-code it and the profile is whichever mode you wrote.
    ///
    /// The `| tojson` is not optional and not decoration: `MiniJinja` renders a
    /// bare boolean as `True`, which is Python and is not JSON. Rendering
    /// catches it — the error carries the body and points at the character — but
    /// it is a nicer trap to avoid than to diagnose.
    pub stream: bool,
}

impl RenderContext {
    /// Appends declarations already in wire shape, as MCP tools arrive.
    ///
    /// Kept separate from [`Self::with_tools`] so [`crate::exec`] never has to
    /// learn what MCP is: the agent loop resolves the live tools and hands them
    /// over as plain values.
    #[must_use]
    pub fn and_tools(mut self, extra: Vec<Value>) -> Self {
        self.tools.extend(extra);
        self
    }

    /// Declares `tools` in `OpenAI` function shape.
    #[must_use]
    pub fn with_tools(mut self, tools: &[ToolSpec]) -> Self {
        self.tools = tools
            .iter()
            .map(|tool| {
                serde_json::json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "parameters": tool.schema,
                    }
                })
            })
            .collect();
        self
    }
}

/// A request ready to be sent, and to be shown to a human.
#[derive(Debug, Clone)]
pub struct RenderedRequest {
    /// HTTP method.
    pub method: HttpMethod,
    /// Target URL.
    pub url: Url,
    /// All headers, credentials included. Never rendered without a [`Redactor`].
    pub headers: HeaderMap,
    /// Body exactly as the template produced it, whitespace and all.
    pub body: String,
}

impl RenderedRequest {
    /// Headers as a displayable map, with credentials masked.
    #[must_use]
    pub fn display_headers(&self, redactor: &Redactor) -> BTreeMap<String, String> {
        let raw: BTreeMap<String, String> = self
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or("<non-utf8>").to_owned(),
                )
            })
            .collect();
        redactor.headers(&raw)
    }

    /// The equivalent `curl` command, with credentials masked.
    ///
    /// This is the direct replacement for the copy-pasted `curl` this tool exists
    /// to retire, so it is meant to be pasted into a ticket as-is.
    #[must_use]
    pub fn to_curl(&self, redactor: &Redactor) -> String {
        let mut parts = vec![
            format!("curl -sS -X {}", method_name(self.method)),
            format!("  {}", shell_quote(self.url.as_str())),
        ];

        for (name, value) in self.display_headers(redactor) {
            parts.push(format!("  -H {}", shell_quote(&format!("{name}: {value}"))));
        }

        if !self.body.is_empty() {
            parts.push(format!(
                "  --data-raw {}",
                shell_quote(&redactor.text(&self.body))
            ));
        }

        parts.join(" \\\n")
    }
}

/// Why a request could not be rendered.
#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    /// The template itself is broken: unknown filter, syntax error, bad expression.
    #[error("template error: {0}")]
    Template(#[from] Box<minijinja::Error>),

    /// The request script failed, or returned something unusable.
    #[error("request script: {0}")]
    Script(#[from] Box<ScriptError>),

    /// No `template` and no `script`. Validation normally catches this at load,
    /// so reaching it means a profile got in another way.
    #[error("the profile declares neither `request.template` nor `request.script`")]
    NoSource,

    /// The template ran, but produced something that is not JSON.
    #[error("the template rendered invalid JSON at line {line}, column {column}: {message}")]
    InvalidJson {
        /// Parser message.
        message: String,
        /// 1-based line in the rendered body.
        line: usize,
        /// 1-based column in the rendered body.
        column: usize,
        /// The rendered body, so the user can see the comma they left behind.
        rendered: String,
    },
}

/// Renders a profile's request body against `context`.
///
/// # Errors
///
/// Returns [`RenderError::Template`] for a broken template and
/// [`RenderError::InvalidJson`] when the output is not valid JSON.
pub fn render_body(profile: &Profile, context: &RenderContext) -> Result<String, RenderError> {
    let body = match profile.request.source().ok_or(RenderError::NoSource)? {
        RequestSource::Template(template) => ENVIRONMENT
            .render_str(template, context)
            .map_err(|error| RenderError::Template(Box::new(error)))?,
        RequestSource::Script(script) => run_request_script(script, context)?,
    };

    if let Err(error) = serde_json::from_str::<Value>(&body) {
        return Err(RenderError::InvalidJson {
            message: error.to_string(),
            line: error.line(),
            column: error.column(),
            rendered: body,
        });
    }

    Ok(body)
}

/// Runs a request script and turns whatever it returned into a body.
///
/// A string is used verbatim — that is the escape hatch for a format that is not
/// JSON-shaped at all. A map or an array is serialised, which is what you want
/// most of the time and saves the script from doing its own quoting.
fn run_request_script(
    script: &ScriptSource,
    context: &RenderContext,
) -> Result<String, RenderError> {
    let bound = serde_json::to_value(context).map_err(|error| {
        RenderError::Script(Box::new(ScriptError::Runtime {
            message: format!("cannot hand the render context to the script: {error}"),
        }))
    })?;

    let mut scope = Scope::new();
    if let serde_json::Value::Object(fields) = bound {
        for (name, value) in fields {
            let value = to_dynamic(&value).map_err(|error| RenderError::Script(Box::new(error)))?;
            scope.push_dynamic(name, value);
        }
    }

    let returned = script
        .run(&mut scope)
        .map_err(|error| RenderError::Script(Box::new(error)))?;

    // A string is used verbatim, which is the escape hatch for a body that is
    // not JSON-shaped at all.
    if returned.is_string() {
        return returned.into_string().map_err(|found| {
            RenderError::Script(Box::new(ScriptError::WrongShape {
                found: found.to_owned(),
                expected: "a string, a map or an array",
            }))
        });
    }

    let value: serde_json::Value = crate::script::from_dynamic(&returned, "a map or an array")
        .map_err(|error| RenderError::Script(Box::new(error)))?;
    serde_json::to_string(&value).map_err(|error| {
        RenderError::Script(Box::new(ScriptError::Runtime {
            message: error.to_string(),
        }))
    })
}

fn method_name(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Post => "POST",
        HttpMethod::Get => "GET",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
    }
}

/// Wraps a value in single quotes, escaping any it contains, so the result is safe
/// to paste into a POSIX shell.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redact::{MASK, Secret};

    fn profile(template: &str) -> Profile {
        let yaml = format!(
            "name: t\nkind: chat\nurl: https://models.internal/v1/chat/completions\nrequest:\n  template: {}\n",
            serde_json::to_string(template).unwrap()
        );
        serde_yaml_ng::from_str(&yaml).unwrap()
    }

    #[test]
    fn renders_messages_as_json() {
        let profile = profile(r#"{"model": "m", "messages": {{ messages | tojson }}}"#);
        let context = RenderContext {
            messages: vec![Message::user("ping")],
            ..RenderContext::default()
        };

        let body = render_body(&profile, &context).unwrap();
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["messages"][0]["content"], "ping");
    }

    #[test]
    fn params_support_defaults() {
        let profile = profile(r#"{"max_tokens": {{ params.max_tokens | default(512) }}}"#);
        let body = render_body(&profile, &RenderContext::default()).unwrap();
        assert_eq!(body, r#"{"max_tokens": 512}"#);

        let context = RenderContext {
            params: serde_json::from_value(serde_json::json!({"max_tokens": 32})).unwrap(),
            ..RenderContext::default()
        };
        let body = render_body(&profile, &context).unwrap();
        assert_eq!(body, r#"{"max_tokens": 32}"#);
    }

    #[test]
    fn a_trailing_comma_is_caught_here_with_the_rendered_body_attached() {
        let profile =
            profile(r#"{"a": 1,{% if tools %}"tools": {{ tools | tojson }},{% endif %}}"#);
        let error = render_body(&profile, &RenderContext::default()).unwrap_err();

        let RenderError::InvalidJson { rendered, line, .. } = error else {
            panic!("expected an InvalidJson error, got {error:?}");
        };
        assert_eq!(rendered, r#"{"a": 1,}"#);
        assert_eq!(line, 1);
    }

    #[test]
    fn an_unknown_filter_is_a_template_error() {
        let profile = profile("{{ messages | no_such_filter }}");
        assert!(matches!(
            render_body(&profile, &RenderContext::default()),
            Err(RenderError::Template(_))
        ));
    }

    #[test]
    fn embedding_input_is_available_as_a_list() {
        let profile = profile(r#"{"input": {{ input | tojson }}}"#);
        let context = RenderContext {
            input: vec!["a".to_owned(), "b".to_owned()],
            ..RenderContext::default()
        };
        assert_eq!(
            render_body(&profile, &context).unwrap(),
            r#"{"input": ["a","b"]}"#
        );
    }

    #[test]
    fn curl_export_masks_credentials_and_quotes_safely() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer s3cr3t-token".parse().unwrap());
        headers.insert("content-type", "application/json".parse().unwrap());

        let request = RenderedRequest {
            method: HttpMethod::Post,
            url: Url::parse("https://models.internal/v1/chat/completions").unwrap(),
            headers,
            body: r#"{"q": "it's fine"}"#.to_owned(),
        };
        let redactor = Redactor::new().with(&Secret::new("s3cr3t-token"));

        let curl = request.to_curl(&redactor);
        assert!(!curl.contains("s3cr3t-token"), "{curl}");
        assert!(curl.contains(&format!("authorization: {MASK}")), "{curl}");
        assert!(curl.contains(r"'\''"), "{curl}");
        assert!(curl.starts_with("curl -sS -X POST"), "{curl}");
    }
}
