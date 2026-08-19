//! Turning a profile plus some input into an actual HTTP request.
//!
//! The body comes from a `MiniJinja` template, which is the declarative level: you
//! get `messages`, `input`, `tools`, `model`, `params` and `uploads`, and you emit
//! whatever JSON your endpoint wants.
//!
//! Rendering validates that the result is JSON. A template with a stray comma —
//! the classic `{% if tools %}"tools": …,{% endif %}` — fails here with the
//! rendered text attached, rather than as a bewildering `400` from the endpoint.
//!
//! Not every model endpoint reads a JSON document, though. A transcriber or a
//! diariser takes a `multipart/form-data`: the audio as bytes, and a handful of
//! knobs as ordinary form fields beside it. That is [`RequestSource::Multipart`],
//! and it renders to [`RenderedBody::Multipart`] — a list of parts rather than a
//! string, because a form is not text and pretending otherwise would put a
//! boundary somebody has to keep consistent into a config file.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use minijinja::Environment;
use minijinja::value::Value as Rendered;
use reqwest::header::HeaderMap;
use rhai::Scope;
use serde::Serialize;
use serde_json::{Map, Value};
use url::Url;

use crate::script::to_dynamic;

use crate::message::Message;
use crate::profile::{HttpMethod, MultipartSpec, PartKind, Profile, RequestSource, ToolSpec};
use crate::redact::Redactor;
use crate::script::{ScriptError, ScriptSource};
use crate::uploads::{UploadRef, carrying, mime_of, resolve};

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
    /// Files the caller attached, in the order they were asked for.
    ///
    /// Whole, and already read: `uploads[0].base64` is the file, not a promise of
    /// it. Eager because this same context is handed to a Rhai request script as
    /// plain JSON, and a value that only materialises when a template touches it
    /// would be a value one of the two request sources could not see.
    ///
    /// Only what the call asked for. A directory holding forty files does not
    /// put forty files in a request body — the caller names the ones it wants,
    /// which is also what keeps last week's attachment out of today's call.
    pub uploads: Vec<UploadRef>,
    /// Whether this call was asked to stream.
    ///
    /// Exposed to the template because the endpoint has to be *told* — nothing
    /// `mire` does client-side makes a response arrive in chunks. Write
    /// `"stream": {{ stream | tojson }}` and one profile serves both shapes;
    /// hard-code it and the profile is whichever shape you wrote.
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

/// What a rendered request carries.
///
/// A description rather than bytes on the wire, and deliberately so: a request
/// is sent more than once here — replayed after a `401`, and repeated by the
/// determinism check — so the body has to survive being sent. A `multipart` is
/// encoded fresh by [`crate::transport`] on each attempt, boundary and all.
#[derive(Debug, Clone)]
pub enum RenderedBody {
    /// A JSON document, exactly as the template or script produced it,
    /// whitespace and all.
    Json(String),
    /// A form, one entry per part, in the order the profile declared them.
    Multipart(Vec<RenderedPart>),
}

impl RenderedBody {
    /// The body as text, when it is text. `None` for a form.
    #[must_use]
    pub fn as_json(&self) -> Option<&str> {
        match self {
            Self::Json(body) => Some(body),
            Self::Multipart(_) => None,
        }
    }

    /// The parts, when it is a form. Empty otherwise.
    #[must_use]
    pub fn parts(&self) -> &[RenderedPart] {
        match self {
            Self::Json(_) => &[],
            Self::Multipart(parts) => parts,
        }
    }
}

/// One part of a rendered form.
#[derive(Debug, Clone)]
pub struct RenderedPart {
    /// The form field it goes out under, as the profile named it.
    pub field: String,
    /// What it carries.
    pub content: PartContent,
}

/// The two things a part can be.
#[derive(Debug, Clone)]
pub enum PartContent {
    /// A text field, rendered.
    Text {
        /// The rendered value.
        value: String,
        /// `content-type` the profile declared for it, if any.
        media_type: Option<String>,
    },
    /// A file, read and decoded once so that a replay does not go back to the
    /// disk for a second answer to a question already answered.
    File(AttachedFile),
}

/// One file on its way out as a part.
#[derive(Debug, Clone)]
pub struct AttachedFile {
    /// The upload's handle, as `POST /api/uploads` answered it.
    pub id: String,
    /// The part's `filename`: the stored name, or what the profile overrode it
    /// with.
    pub filename: String,
    /// The part's `content-type`.
    pub media_type: String,
    /// Where the file is on disk, so the `curl` export is one somebody can run.
    pub path: String,
    /// The bytes.
    pub bytes: Vec<u8>,
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
    /// What it carries.
    pub body: RenderedBody,
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
    ///
    /// A form comes out as `-F` flags, files included by their path on disk
    /// rather than inlined — which is both the only readable option and the one
    /// that still runs, since the file is right there where `--uploads` put it.
    #[must_use]
    pub fn to_curl(&self, redactor: &Redactor) -> String {
        let mut lines = vec![
            format!("curl -sS -X {}", method_name(self.method)),
            format!("  {}", shell_quote(self.url.as_str())),
        ];

        for (name, value) in self.display_headers(redactor) {
            lines.push(format!("  -H {}", shell_quote(&format!("{name}: {value}"))));
        }

        match &self.body {
            RenderedBody::Json(body) if !body.is_empty() => lines.push(format!(
                "  --data-raw {}",
                shell_quote(&redactor.text(body))
            )),
            RenderedBody::Json(_) => {}
            RenderedBody::Multipart(parts) => {
                for part in parts {
                    lines.push(format!("  -F {}", shell_quote(&curl_form(part, redactor))));
                }
            }
        }

        lines.join(" \\\n")
    }
}

/// One `-F` argument: `field=value`, or `field=@path` for a file.
///
/// `;type=` is only appended when the profile said something about the type. For
/// a file left to the extension's guess, `curl` makes the same guess, and a
/// command carrying a type nobody wrote reads like a decision that was made.
fn curl_form(part: &RenderedPart, redactor: &Redactor) -> String {
    match &part.content {
        PartContent::Text { value, media_type } => {
            let value = redactor.text(value);
            match media_type {
                Some(media) => format!("{}={value};type={media}", part.field),
                None => format!("{}={value}", part.field),
            }
        }
        PartContent::File(file) => format!(
            "{}=@{};type={};filename={}",
            part.field, file.path, file.media_type, file.filename
        ),
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

    /// A `multipart:` field could not be filled. The message names the field.
    #[error("request.multipart: {message}")]
    Multipart {
        /// What went wrong, field name included.
        message: String,
    },

    /// No `template`, no `script` and no `multipart`. Validation normally catches
    /// this at load, so reaching it means a profile got in another way.
    #[error("the profile declares no `request.template`, `request.script` or `request.multipart`")]
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
/// Returns [`RenderError::Template`] for a broken template,
/// [`RenderError::InvalidJson`] when a JSON body renders to something that is
/// not JSON, and [`RenderError::Multipart`] when a form field names a file the
/// call is not carrying.
pub fn render_body(
    profile: &Profile,
    context: &RenderContext,
) -> Result<RenderedBody, RenderError> {
    let body = match profile.request.source().ok_or(RenderError::NoSource)? {
        RequestSource::Template(template) => ENVIRONMENT
            .render_str(template, context)
            .map_err(|error| RenderError::Template(Box::new(error)))?,
        RequestSource::Script(script) => run_request_script(script, context)?,
        // Not JSON, and not checked as if it were: a form is fields and bytes.
        RequestSource::Multipart(spec) => {
            return render_multipart(spec, context).map(RenderedBody::Multipart);
        }
    };

    if let Err(error) = serde_json::from_str::<Value>(&body) {
        return Err(RenderError::InvalidJson {
            message: error.to_string(),
            line: error.line(),
            column: error.column(),
            rendered: body,
        });
    }

    Ok(RenderedBody::Json(body))
}

/// The parts of a form, resolved against what the call is carrying.
fn render_multipart(
    spec: &MultipartSpec,
    context: &RenderContext,
) -> Result<Vec<RenderedPart>, RenderError> {
    // One serialisation of the context for every file field of the form: a
    // template naming an upload is *evaluated* rather than rendered, and that
    // needs the context as MiniJinja values rather than as a struct.
    let bound = LazyLock::new(|| Rendered::from_serialize(context));
    let mut parts = Vec::with_capacity(spec.parts().len());

    for declared in spec.parts() {
        let field = declared.field.as_str();
        match &declared.part {
            PartKind::Text {
                template,
                media_type,
            } => {
                let value = ENVIRONMENT
                    .render_str(template, context)
                    .map_err(|error| RenderError::Template(Box::new(error)))?;
                parts.push(RenderedPart {
                    field: field.to_owned(),
                    content: PartContent::Text {
                        value,
                        media_type: media_type.clone(),
                    },
                });
            }
            PartKind::File {
                sources,
                media_type,
                filename,
            } => {
                let files = name_files(sources, field, &bound, &context.uploads)?;

                // Said here rather than left to the endpoint. A `filename:` on a
                // field carrying two files would name both of them the same
                // thing, and the endpoint would be the one to notice.
                if filename.is_some() && files.len() > 1 {
                    return Err(RenderError::Multipart {
                        message: format!(
                            "`{field}` sets `filename` but named {} files",
                            files.len()
                        ),
                    });
                }

                for upload in files {
                    parts.push(RenderedPart {
                        field: field.to_owned(),
                        content: PartContent::File(AttachedFile {
                            id: upload.id.clone(),
                            filename: filename.clone().unwrap_or_else(|| upload.name.clone()),
                            media_type: media_type
                                .clone()
                                .unwrap_or_else(|| mime_of(upload).to_owned()),
                            path: upload.path.clone(),
                            // Decoded rather than re-read off the disk: the call
                            // already holds the file, and going back to the
                            // filesystem is a second answer to a question already
                            // answered — one that can differ if the file moved.
                            bytes: BASE64.decode(&upload.base64).map_err(|error| {
                                RenderError::Multipart {
                                    message: format!(
                                        "`{field}`: `{}` could not be decoded: {error}",
                                        upload.name
                                    ),
                                }
                            })?,
                        }),
                    });
                }
            }
        }
    }

    Ok(parts)
}

/// The uploads one file field names, across all of its templates.
fn name_files<'a>(
    sources: &[String],
    field: &str,
    bound: &LazyLock<Rendered, impl FnOnce() -> Rendered>,
    uploads: &'a [UploadRef],
) -> Result<Vec<&'a UploadRef>, RenderError> {
    // Worth saying up front. Left to `resolve`, a call with nothing attached
    // fails with "that is not a file", which is true and unhelpful — the file
    // field is fine, there is simply nothing to put in it.
    if uploads.is_empty() {
        return Err(RenderError::Multipart {
            message: format!("`{field}` names a file, and nothing was attached to this call"),
        });
    }

    let mut found: Vec<&UploadRef> = Vec::new();

    for source in sources {
        let value = match lone_expression(source) {
            Some(expression) => ENVIRONMENT
                .compile_expression(expression)
                .and_then(|compiled| compiled.eval(&**bound))
                .map_err(|error| RenderError::Template(Box::new(error)))?,
            None => Rendered::from(
                ENVIRONMENT
                    .render_str(source, &**bound)
                    .map_err(|error| RenderError::Template(Box::new(error)))?,
            ),
        };

        found.extend(
            resolve(&value, uploads, field)
                .map_err(|message| RenderError::Multipart { message })?,
        );
    }

    // A field that named nothing is the failure this whole shape exists to make
    // loud. A form missing the one part the endpoint asked for goes out looking
    // perfectly well-formed and comes back a `422` about a field nobody in the
    // profile ever mentioned.
    if found.is_empty() {
        return Err(RenderError::Multipart {
            message: format!("`{field}` named no file ({})", carrying(uploads)),
        });
    }

    Ok(found)
}

/// The expression a template is made of, when it is made of nothing else.
///
/// `{{ uploads[0] }}` has to hand back the upload itself, not `MiniJinja`'s
/// rendering of one — and `'{{ uploads }}'`, the list. Anything with text around
/// the expression is a name, because that is what interpolation is for.
fn lone_expression(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    let inner = trimmed.strip_prefix("{{")?.strip_suffix("}}")?;
    (!inner.contains("{{") && !inner.contains("}}")).then_some(inner)
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

    /// The rendered body of a JSON profile, as text. Panics on a form, which no
    /// caller of it declares.
    fn json_body(profile: &Profile, context: &RenderContext) -> String {
        render_body(profile, context)
            .unwrap()
            .as_json()
            .expect("a JSON body")
            .to_owned()
    }

    #[test]
    fn renders_messages_as_json() {
        let profile = profile(r#"{"model": "m", "messages": {{ messages | tojson }}}"#);
        let context = RenderContext {
            messages: vec![Message::user("ping")],
            ..RenderContext::default()
        };

        let body = json_body(&profile, &context);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["messages"][0]["content"], "ping");
    }

    #[test]
    fn params_support_defaults() {
        let profile = profile(r#"{"max_tokens": {{ params.max_tokens | default(512) }}}"#);
        let body = json_body(&profile, &RenderContext::default());
        assert_eq!(body, r#"{"max_tokens": 512}"#);

        let context = RenderContext {
            params: serde_json::from_value(serde_json::json!({"max_tokens": 32})).unwrap(),
            ..RenderContext::default()
        };
        let body = json_body(&profile, &context);
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
        assert_eq!(json_body(&profile, &context), r#"{"input": ["a","b"]}"#);
    }

    fn upload(name: &str, media: Option<&str>, bytes: &[u8]) -> UploadRef {
        use base64::Engine;
        let base64 = base64::engine::general_purpose::STANDARD.encode(bytes);
        UploadRef {
            id: "aB3dE5gH7jK9".to_owned(),
            name: name.to_owned(),
            stored_as: format!("aB3dE5gH7jK9-{name}"),
            path: format!("/uploads/aB3dE5gH7jK9-{name}"),
            size: bytes.len() as u64,
            content_type: media.map(str::to_owned),
            data_url: format!(
                "data:{};base64,{base64}",
                media.unwrap_or("application/octet-stream")
            ),
            base64,
            text: String::from_utf8(bytes.to_vec()).ok(),
        }
    }

    /// The shape a vision endpoint actually reads, written in a template rather
    /// than built in Rust: what an attachment turns into is the profile's call.
    #[test]
    fn an_upload_reaches_the_body_as_a_data_url() {
        let profile = profile(
            r#"{"content": [{% for file in uploads %}{"type": "image_url", "image_url": {"url": "{{ file.dataUrl }}"}}{% endfor %}]}"#,
        );
        let context = RenderContext {
            uploads: vec![upload("shot.png", Some("image/png"), &[0x89, b'P'])],
            ..RenderContext::default()
        };

        let body = json_body(&profile, &context);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            parsed["content"][0]["image_url"]["url"],
            "data:image/png;base64,iVA="
        );
    }

    /// The other half of the feature: a text file inlined into the prompt. Both
    /// are one `uploads` entry seen two different ways, which is why the entry
    /// carries both rather than the caller choosing at upload time.
    #[test]
    fn a_text_file_can_be_inlined_as_text() {
        let profile = profile(
            r#"{"prompt": {{ uploads[0].text | tojson }}, "name": {{ uploads[0].name | tojson }}}"#,
        );
        let context = RenderContext {
            uploads: vec![upload("notes.txt", Some("text/plain"), b"ping")],
            ..RenderContext::default()
        };

        let body = json_body(&profile, &context);
        let parsed: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["prompt"], "ping");
        assert_eq!(parsed["name"], "notes.txt");
    }

    /// The pattern the README hands out for inlining a log or a CSV, run rather
    /// than asserted: a snippet nobody executes is a snippet with a stray comma
    /// in it, and this one is fiddly precisely where `MiniJinja` is unforgiving.
    #[test]
    fn the_documented_text_inlining_pattern_renders() {
        let profile = profile(concat!(
            r#"{"messages": ["#,
            r#"{% for file in uploads %}{% if file.text %}"#,
            r#"{"role": "user", "content": {{ ("Contents of " ~ file.name ~ ":\n" ~ file.text) | tojson }}},"#,
            r#"{% endif %}{% endfor %}"#,
            r#"{% for message in messages %}{{ message | tojson }}{% if not loop.last %},{% endif %}{% endfor %}"#,
            r#"]}"#,
        ));
        let context = RenderContext {
            messages: vec![Message::user("what is in this")],
            uploads: vec![
                upload("notes.txt", Some("text/plain"), b"ping"),
                // Skipped by the `if`, which is the half that actually matters:
                // a PNG inlined as text would be mojibake in a prompt.
                upload("shot.png", Some("image/png"), &[0x89, 0xff]),
            ],
            ..RenderContext::default()
        };

        let parsed: Value = serde_json::from_str(&json_body(&profile, &context)).unwrap();
        assert_eq!(parsed["messages"].as_array().unwrap().len(), 2);
        assert_eq!(
            parsed["messages"][0]["content"],
            "Contents of notes.txt:\nping"
        );
        assert_eq!(parsed["messages"][1]["content"], "what is in this");
    }

    /// `text` is `null` for anything that is not UTF-8, so a template can ask
    /// rather than guess from the name.
    #[test]
    fn a_template_can_tell_text_from_binary() {
        let profile =
            profile(r#"{"kind": "{% if uploads[0].text %}text{% else %}binary{% endif %}"}"#);

        let text = RenderContext {
            uploads: vec![upload("notes.txt", Some("text/plain"), b"ping")],
            ..RenderContext::default()
        };
        assert_eq!(json_body(&profile, &text), r#"{"kind": "text"}"#);

        let binary = RenderContext {
            uploads: vec![upload("shot.png", Some("image/png"), &[0x89, 0xff])],
            ..RenderContext::default()
        };
        assert_eq!(json_body(&profile, &binary), r#"{"kind": "binary"}"#);
    }

    /// A profile that never mentions `uploads` sends what it always sent.
    /// Attaching a file is not a thing that happens *to* a template.
    #[test]
    fn a_template_that_ignores_uploads_is_unaffected_by_one() {
        let profile = profile(r#"{"messages": {{ messages | tojson }}}"#);
        let context = RenderContext {
            messages: vec![Message::user("ping")],
            uploads: vec![upload("shot.png", Some("image/png"), &[0x89, b'P'])],
            ..RenderContext::default()
        };

        assert_eq!(
            json_body(&profile, &context),
            r#"{"messages": [{"content":"ping","role":"user"}]}"#
        );
    }

    /// The request script sees the same context the template does — it is the
    /// same struct, serialised. A field only one of them could reach would be a
    /// trap for whoever switched.
    #[test]
    fn a_request_script_sees_the_uploads_too() {
        let yaml = "name: t\nkind: chat\nurl: https://models.internal/v1/chat/completions\nrequest:\n  script: |\n    #{ \"file\": uploads[0].name, \"bytes\": uploads[0].size }\n";
        let profile: Profile = serde_yaml_ng::from_str(yaml).unwrap();
        let context = RenderContext {
            uploads: vec![upload("notes.txt", Some("text/plain"), b"ping")],
            ..RenderContext::default()
        };

        let parsed: Value = serde_json::from_str(&json_body(&profile, &context)).unwrap();
        assert_eq!(parsed["file"], "notes.txt");
        assert_eq!(parsed["bytes"], 4);
    }

    /// A profile whose `request:` is the given YAML fragment.
    fn multipart_profile(fields: &str) -> Profile {
        let yaml = format!(
            "name: t\nkind: chat\nurl: https://models.internal/v1/audio/transcriptions\nrequest:\n  multipart:\n{fields}"
        );
        serde_yaml_ng::from_str(&yaml).unwrap_or_else(|error| panic!("{error}\n{yaml}"))
    }

    fn text_part<'a>(parts: &'a [RenderedPart], field: &str) -> &'a str {
        let Some(part) = parts.iter().find(|part| part.field == field) else {
            panic!("no `{field}` part");
        };
        match &part.content {
            PartContent::Text { value, .. } => value.as_str(),
            PartContent::File(_) => panic!("`{field}` is a file"),
        }
    }

    fn file_parts(parts: &[RenderedPart]) -> Vec<(&str, &AttachedFile)> {
        parts
            .iter()
            .filter_map(|part| match &part.content {
                PartContent::File(file) => Some((part.field.as_str(), file)),
                PartContent::Text { .. } => None,
            })
            .collect()
    }

    /// The shape a transcription endpoint actually reads, written in a profile:
    /// the audio as bytes, the knobs as ordinary fields beside it.
    #[test]
    fn a_form_carries_the_file_and_the_knobs_beside_it() {
        let profile = multipart_profile(
            "    file:\n      upload: '{{ uploads[0] }}'\n    model: whisper-1\n    language: '{{ params.language | default(\"en\") }}'\n",
        );
        let context = RenderContext {
            uploads: vec![upload("meeting.mp3", Some("audio/mpeg"), b"ID3\x04")],
            params: serde_json::from_value(serde_json::json!({"language": "fr"})).unwrap(),
            ..RenderContext::default()
        };

        let body = render_body(&profile, &context).unwrap();
        assert!(body.as_json().is_none(), "a form is not text");

        let parts = body.parts();
        assert_eq!(text_part(parts, "model"), "whisper-1");
        assert_eq!(text_part(parts, "language"), "fr");

        let files = file_parts(parts);
        assert_eq!(files.len(), 1);
        let (field, file) = files[0];
        assert_eq!(field, "file");
        assert_eq!(file.filename, "meeting.mp3");
        assert_eq!(file.media_type, "audio/mpeg");
        assert_eq!(file.bytes, b"ID3\x04");
    }

    /// Order is the file's, not the alphabet's. A `BTreeMap` would have sorted
    /// this form to `file, model, response_format` without anybody noticing.
    #[test]
    fn the_parts_go_out_in_the_order_the_profile_wrote_them() {
        let profile = multipart_profile(
            "    response_format: json\n    file:\n      upload: '{{ uploads[0] }}'\n    model: whisper-1\n",
        );
        let context = RenderContext {
            uploads: vec![upload("a.mp3", Some("audio/mpeg"), b"x")],
            ..RenderContext::default()
        };

        let body = render_body(&profile, &context).unwrap();
        let fields: Vec<&str> = body.parts().iter().map(|p| p.field.as_str()).collect();
        assert_eq!(fields, vec!["response_format", "file", "model"]);
    }

    /// `'{{ uploads }}'` is the whole list, and several files under one field go
    /// out as several parts under that name — which is what every upload handler
    /// on the other side already reads.
    #[test]
    fn one_field_can_carry_several_files() {
        let profile = multipart_profile("    file:\n      upload: '{{ uploads }}'\n");
        let context = RenderContext {
            uploads: vec![
                upload("one.wav", Some("audio/wav"), b"a"),
                upload("two.wav", Some("audio/wav"), b"b"),
            ],
            ..RenderContext::default()
        };

        let parts = render_body(&profile, &context).unwrap();
        let files = file_parts(parts.parts());
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|(field, _)| *field == "file"));
        assert_eq!(files[0].1.filename, "one.wav");
        assert_eq!(files[1].1.filename, "two.wav");
    }

    /// A file can also be named by its `path`, `name` or `id` rather than handed
    /// over whole — the same three forms a hook's `multipart:` accepts, because
    /// it is the same resolver.
    #[test]
    fn a_file_can_be_named_rather_than_handed_over() {
        let profile = multipart_profile("    file:\n      upload: '{{ uploads[1].name }}'\n");
        let context = RenderContext {
            uploads: vec![
                upload("one.wav", Some("audio/wav"), b"a"),
                upload("two.wav", Some("audio/wav"), b"b"),
            ],
            ..RenderContext::default()
        };

        let parts = render_body(&profile, &context).unwrap();
        let files = file_parts(parts.parts());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1.filename, "two.wav");
    }

    /// The two overrides that exist because the guess is only an extension
    /// lookup, and because some transcribers refuse a name they cannot classify.
    #[test]
    fn a_part_can_override_the_type_and_the_filename() {
        let profile = multipart_profile(
            "    file:\n      upload: '{{ uploads[0] }}'\n      type: audio/wav\n      filename: recording.wav\n    config:\n      text: '{\"speakers\": 2}'\n      type: application/json\n",
        );
        let context = RenderContext {
            uploads: vec![upload("blob", None, b"x")],
            ..RenderContext::default()
        };

        let body = render_body(&profile, &context).unwrap();
        let files = file_parts(body.parts());
        assert_eq!(files[0].1.filename, "recording.wav");
        assert_eq!(files[0].1.media_type, "audio/wav");

        let config = body
            .parts()
            .iter()
            .find(|part| part.field == "config")
            .unwrap();
        let PartContent::Text { value, media_type } = &config.content else {
            panic!("a text part");
        };
        assert_eq!(value, r#"{"speakers": 2}"#);
        assert_eq!(media_type.as_deref(), Some("application/json"));
    }

    /// Without a `type:`, an extension nobody recognises is `octet-stream`
    /// rather than a guess dressed up as a fact.
    #[test]
    fn a_file_with_no_recognisable_extension_says_octet_stream() {
        let profile = multipart_profile("    file:\n      upload: '{{ uploads[0] }}'\n");
        let context = RenderContext {
            uploads: vec![upload("recording", None, b"x")],
            ..RenderContext::default()
        };

        let body = render_body(&profile, &context).unwrap();
        assert_eq!(
            file_parts(body.parts())[0].1.media_type,
            "application/octet-stream"
        );
    }

    /// The failure the whole shape exists to make loud: a form that would go out
    /// looking perfectly well-formed and come back a `422` about a field nobody
    /// in the profile ever mentioned.
    #[test]
    fn a_file_field_with_nothing_attached_says_so_rather_than_sending_an_empty_form() {
        let profile = multipart_profile("    file:\n      upload: '{{ uploads[0] }}'\n");
        let error = render_body(&profile, &RenderContext::default()).unwrap_err();

        let RenderError::Multipart { message } = error else {
            panic!("expected a Multipart error, got {error:?}");
        };
        assert!(message.contains("nothing was attached"), "{message}");
        assert!(message.contains("`file`"), "{message}");
    }

    /// And when something *is* attached but not the thing named, the message
    /// says what the call is actually carrying.
    #[test]
    fn naming_a_file_the_call_does_not_carry_lists_the_ones_it_does() {
        let profile = multipart_profile("    file:\n      upload: absent.wav\n");
        let context = RenderContext {
            uploads: vec![upload("present.wav", Some("audio/wav"), b"a")],
            ..RenderContext::default()
        };

        let RenderError::Multipart { message } = render_body(&profile, &context).unwrap_err()
        else {
            panic!("expected a Multipart error");
        };
        assert!(message.contains("present.wav"), "{message}");
    }

    /// One `filename:` over two files would name both of them the same thing,
    /// and the endpoint would be the one to notice.
    #[test]
    fn a_filename_override_is_refused_on_a_field_carrying_several_files() {
        let profile = multipart_profile(
            "    file:\n      upload: '{{ uploads }}'\n      filename: only-one.wav\n",
        );
        let context = RenderContext {
            uploads: vec![
                upload("one.wav", Some("audio/wav"), b"a"),
                upload("two.wav", Some("audio/wav"), b"b"),
            ],
            ..RenderContext::default()
        };

        let RenderError::Multipart { message } = render_body(&profile, &context).unwrap_err()
        else {
            panic!("expected a Multipart error");
        };
        assert!(message.contains("named 2 files"), "{message}");
    }

    /// A form is not JSON and is not checked as if it were. A text field holding
    /// a stray brace is a text field, not a broken body.
    #[test]
    fn a_form_is_not_validated_as_json() {
        let profile = multipart_profile("    prompt: 'not { json at all'\n");
        let body = render_body(&profile, &RenderContext::default()).unwrap();
        assert_eq!(text_part(body.parts(), "prompt"), "not { json at all");
    }

    /// The `curl` a form exports to is one somebody can actually run: the file
    /// comes in by path, from where `--uploads` put it.
    #[test]
    fn curl_export_of_a_form_uses_flags_and_the_file_on_disk() {
        let profile = multipart_profile(
            "    file:\n      upload: '{{ uploads[0] }}'\n    model: whisper-1\n",
        );
        let context = RenderContext {
            uploads: vec![upload("meeting.mp3", Some("audio/mpeg"), b"ID3")],
            ..RenderContext::default()
        };

        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer s3cr3t-token".parse().unwrap());
        let request = RenderedRequest {
            method: HttpMethod::Post,
            url: Url::parse("https://models.internal/v1/audio/transcriptions").unwrap(),
            headers,
            body: render_body(&profile, &context).unwrap(),
        };

        let curl = request.to_curl(&Redactor::new().with(&Secret::new("s3cr3t-token")));
        assert!(curl.contains("-F 'model=whisper-1'"), "{curl}");
        assert!(
            curl.contains(
                "-F 'file=@/uploads/aB3dE5gH7jK9-meeting.mp3;type=audio/mpeg;filename=meeting.mp3'"
            ),
            "{curl}"
        );
        assert!(!curl.contains("--data-raw"), "{curl}");
        assert!(!curl.contains("s3cr3t-token"), "{curl}");
    }

    /// A credential that reached a text field is masked there too. `params` is
    /// caller-supplied, so a form field reading one is a place a token can land.
    #[test]
    fn a_text_part_is_masked_like_a_body_is() {
        let profile = multipart_profile("    key: '{{ params.key }}'\n");
        let context = RenderContext {
            params: serde_json::from_value(serde_json::json!({"key": "s3cr3t-token"})).unwrap(),
            ..RenderContext::default()
        };

        let request = RenderedRequest {
            method: HttpMethod::Post,
            url: Url::parse("https://models.internal/v1/audio/transcriptions").unwrap(),
            headers: HeaderMap::new(),
            body: render_body(&profile, &context).unwrap(),
        };

        let curl = request.to_curl(&Redactor::new().with(&Secret::new("s3cr3t-token")));
        assert!(!curl.contains("s3cr3t-token"), "{curl}");
        assert!(curl.contains(MASK), "{curl}");
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
            body: RenderedBody::Json(r#"{"q": "it's fine"}"#.to_owned()),
        };
        let redactor = Redactor::new().with(&Secret::new("s3cr3t-token"));

        let curl = request.to_curl(&redactor);
        assert!(!curl.contains("s3cr3t-token"), "{curl}");
        assert!(curl.contains(&format!("authorization: {MASK}")), "{curl}");
        assert!(curl.contains(r"'\''"), "{curl}");
        assert!(curl.starts_with("curl -sS -X POST"), "{curl}");
    }
}
