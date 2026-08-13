//! Running one call: render, authenticate, send, decode.
//!
//! Everything a caller needs to reproduce or paste elsewhere comes back in the
//! outcome — the rendered body, the headers, the `curl` equivalent — with
//! credentials already masked.

use std::collections::BTreeMap;
use std::sync::Arc;

use reqwest::Client;
use reqwest::header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};

use crate::auth::{ANONYMOUS, AuthError, AuthProvider, Retry};
use crate::config::ConfigStore;
use crate::decode::embedding::{CheckOutcome, EmbeddingChecks, Vectors};
use crate::decode::stream::{Frame, FrameParser, Framing, StreamView};
use crate::decode::{
    Completion, DecodeTrace, Decoded, EmbeddingResult, HttpMeta, chat, embedding, script, stream,
};
use crate::message::Message;
use crate::profile::{DecodeSpec, HttpMethod, Profile, ProfileKind};
use crate::redact::{Redactor, Secret};
use crate::render::{RenderContext, RenderError, RenderedRequest, render_body};
use crate::transport::{self, TransportError};

/// Everything one call needs, already validated.
#[derive(Debug, Default)]
pub struct CallInput {
    /// Profile name.
    pub profile: String,
    /// Auth provider to use, overriding the profile's own. Defaults to the
    /// profile's, then to [`ANONYMOUS`].
    pub auth: Option<String>,
    /// Conversation. `kind: chat`.
    pub messages: Vec<Message>,
    /// Text to embed. `kind: embedding`.
    pub input: Vec<String>,
    /// Template knobs.
    pub params: Map<String, Value>,
    /// Model override handed to the template.
    pub model: Option<String>,
    /// Credential typed in the UI, for a provider that declares no source.
    pub token: Option<Secret>,
    /// Attach the full vectors to an embedding response. Off by default, and it
    /// has to stay that way: nobody wants 1024 floats they did not ask for.
    pub include_vectors: bool,
    /// How many times to send the request. Above one, and for `kind: embedding`,
    /// the extra runs feed the determinism check and are otherwise discarded.
    pub repeat: u8,
    /// Largest absolute difference two runs may show and still count as
    /// deterministic.
    pub tolerance: f32,
    /// Extra tool declarations, in wire shape, appended to the profile's own.
    /// Agent mode puts the live MCP tools here.
    pub extra_tools: Vec<Value>,
    /// Ask the endpoint to stream, and read the answer chunk by chunk.
    ///
    /// Only reaches the wire if the template uses it — see
    /// [`RenderContext::stream`](crate::render::RenderContext::stream).
    pub stream: bool,
}

/// The request as it would go on the wire, with credentials masked.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequestView {
    /// HTTP method.
    pub method: HttpMethod,
    /// Target URL.
    pub url: String,
    /// Headers, masked.
    pub headers: BTreeMap<String, String>,
    /// Body as rendered.
    pub body: String,
}

/// What came back.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResponseView {
    /// Status, headers and latency.
    pub http: HttpMeta,
    /// Body as text, masked.
    ///
    /// Absent for an embedding response, where it would be a wall of floats.
    /// Ask for `includeVectors` to get it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body_text: Option<String>,
    /// Body parsed as JSON, masked. `None` when the body is not JSON.
    ///
    /// For an embedding response, bulk vector payloads are replaced by a
    /// placeholder unless `includeVectors` was set — see
    /// [`crate::decode::embedding::elide`].
    pub raw: Option<Value>,
    /// `true` when vector payloads were elided from `raw` and `bodyText`.
    pub elided: bool,
    /// Why the body could not be parsed, when it could not.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_error: Option<String>,
    /// Normalised output, when the profile's kind has a decoder.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoded: Option<Decoded>,
    /// Which paths matched, which missed, and what went wrong.
    pub decode: DecodeTrace,
    /// What the stream did, when the call streamed.
    ///
    /// Its presence is what tells a streamed response from an ordinary one. When
    /// it is there, `raw` holds the **final chunk** rather than a whole body —
    /// there is no whole body — and `bodyText` holds the stream as it arrived,
    /// framing included.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream: Option<StreamView>,
}

/// The full result of a call.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CallOutcome {
    /// Profile that ran.
    pub profile: String,
    /// Auth provider that ran.
    pub auth: String,
    /// The rendered request, as it went on the wire — this is the half you paste
    /// into a ticket, and it comes back whatever the endpoint answered.
    pub request: RequestView,
    /// The `curl` equivalent, ready to paste into a ticket.
    pub curl: String,
    /// What came back.
    pub response: ResponseView,
    /// `true` when a `401` triggered one credential refresh and replay.
    pub retried_after_unauthorized: bool,
}

/// Holds what every call needs: the configuration directory and the HTTP client.
#[derive(Debug, Clone)]
pub struct Runner {
    config: Arc<ConfigStore>,
    client: Client,
}

impl Runner {
    /// Assembles a runner.
    #[must_use]
    pub fn new(config: Arc<ConfigStore>, client: Client) -> Self {
        Self { config, client }
    }

    /// The configuration store, for the API's read endpoints.
    #[must_use]
    pub fn config(&self) -> &Arc<ConfigStore> {
        &self.config
    }

    /// Runs one call.
    ///
    /// # Errors
    ///
    /// Fails when the profile or auth provider is unknown, the credential cannot
    /// be produced, the template does not render, or the exchange itself fails. A
    /// `4xx`/`5xx` from the endpoint is **not** an error: it is the answer.
    pub async fn call(&self, input: CallInput) -> Result<CallOutcome, ExecError> {
        // One snapshot for the whole call: the profile and the auth registry it
        // refers to must come from the same view of the directory.
        let config = self.config.snapshot();
        let profile = config
            .profiles
            .get(&input.profile)
            .ok_or_else(|| ExecError::UnknownProfile(input.profile.clone()))?;

        let auth_name = input
            .auth
            .clone()
            .or_else(|| profile.auth.clone())
            .unwrap_or_else(|| ANONYMOUS.to_owned());
        let provider = config
            .registry
            .get(&auth_name)
            .ok_or_else(|| AuthError::UnknownProvider(auth_name.clone()))?;

        // Built before rendering: a template error carries the rendered body back to
        // the user, and `params` is caller-supplied, so it could hold the token.
        let mut redactor = Redactor::new();
        if let Some(token) = &input.token {
            redactor.add(token);
        }

        let body = render_body(profile, &render_context(profile, &input))
            .map_err(|error| ExecError::Render(redact_render_error(error, &redactor)))?;
        let base_headers = base_headers(profile)?;

        let mut headers = base_headers.clone();
        redactor.merge(
            &provider
                .apply(&mut headers, &profile.url, input.token.as_ref())
                .await?,
        );

        let request = RenderedRequest {
            method: profile.method,
            url: profile.url.clone(),
            headers,
            body,
        };
        let view = request_view(&request, &redactor);
        let curl = request.to_curl(&redactor);

        let mut raw = transport::send(&self.client, &request, profile.timeout()).await?;
        let mut retried = false;

        if raw.status == 401 && provider.invalidate().await == Retry::Once {
            warn!(profile = %profile.name, auth = %auth_name, "401, refreshing the credential and replaying once");
            let mut headers = base_headers;
            redactor.merge(
                &provider
                    .apply(&mut headers, &profile.url, input.token.as_ref())
                    .await?,
            );
            let replay = RenderedRequest {
                headers,
                ..request.clone()
            };
            raw = transport::send(&self.client, &replay, profile.timeout()).await?;
            retried = true;
        }

        info!(
            profile = %profile.name,
            auth = %auth_name,
            status = raw.status,
            latency_ms = raw.latency.as_millis(),
            "call completed"
        );
        log_refusal(&profile.name, raw.status, &redactor.text(&raw.body));

        let (mut response, first_vectors) = response_view(
            profile,
            &raw,
            &redactor,
            input.include_vectors,
            input.input.len(),
        );

        if profile.kind == ProfileKind::Embedding && input.repeat > 1 {
            let outcome = self
                .check_determinism(profile, &request, first_vectors.as_ref(), &input, &redactor)
                .await?;
            if let Some(Decoded::Embedding(result)) = response.decoded.as_mut() {
                result.checks.determinism = outcome;
            }
        }

        Ok(CallOutcome {
            profile: profile.name.clone(),
            auth: auth_name,
            request: view,
            curl,
            response,
            retried_after_unauthorized: retried,
        })
    }

    /// Runs one call, streaming.
    ///
    /// `on_event` is called as things happen: once when the response head is in,
    /// then once per text delta. The returned outcome is the same shape a
    /// non-streamed call produces, so everything downstream — the decode trace,
    /// the curl equivalent, the UI — works unchanged.
    ///
    /// # Errors
    ///
    /// Same as [`Self::call`]. A stream that dies halfway is **not** an error:
    /// what arrived is decoded and `stream.terminated` says it ended badly.
    pub async fn call_streaming(
        &self,
        input: CallInput,
        mut on_event: impl FnMut(CallEvent),
    ) -> Result<CallOutcome, ExecError> {
        let config = self.config.snapshot();
        let profile = config
            .profiles
            .get(&input.profile)
            .ok_or_else(|| ExecError::UnknownProfile(input.profile.clone()))?;

        let auth_name = input
            .auth
            .clone()
            .or_else(|| profile.auth.clone())
            .unwrap_or_else(|| ANONYMOUS.to_owned());
        let provider = config
            .registry
            .get(&auth_name)
            .ok_or_else(|| AuthError::UnknownProvider(auth_name.clone()))?;

        let mut redactor = Redactor::new();
        if let Some(token) = &input.token {
            redactor.add(token);
        }

        let body = render_body(profile, &render_context(profile, &input))
            .map_err(|error| ExecError::Render(redact_render_error(error, &redactor)))?;
        let base_headers = base_headers(profile)?;

        let mut headers = base_headers.clone();
        redactor.merge(
            &provider
                .apply(&mut headers, &profile.url, input.token.as_ref())
                .await?,
        );

        let request = RenderedRequest {
            method: profile.method,
            url: profile.url.clone(),
            headers,
            body,
        };
        let view = request_view(&request, &redactor);
        let curl = request.to_curl(&redactor);

        let mut open = transport::open(&self.client, &request, profile.timeout()).await?;
        let mut retried = false;

        // The head is in before a single token is, which is exactly why the
        // replay still works here: a `401` is known immediately, and the body we
        // drop is an error page nobody wanted.
        if open.status == 401 && provider.invalidate().await == Retry::Once {
            warn!(profile = %profile.name, auth = %auth_name, "401, refreshing the credential and replaying once");
            let mut headers = base_headers;
            redactor.merge(
                &provider
                    .apply(&mut headers, &profile.url, input.token.as_ref())
                    .await?,
            );
            let replay = RenderedRequest {
                headers,
                ..request.clone()
            };
            open = transport::open(&self.client, &replay, profile.timeout()).await?;
            retried = true;
        }

        on_event(CallEvent::Open {
            status: open.status,
            headers: redactor.headers(&open.headers),
        });

        let status = open.status;
        let response_headers = redactor.headers(&open.headers);
        let started = open.started;
        let mut accumulator = StreamAccumulator::new(
            &profile.decode,
            &redactor,
            Framing::detect(open.content_type.as_deref()),
            started,
        );

        let read = open
            .read(|text, at| accumulator.push(text, at, &mut on_event))
            .await;

        // A read that failed still delivered chunks, and those chunks are the
        // evidence. The failure is reported through `terminated`, not by throwing
        // away what arrived.
        if let Err(error) = &read {
            debug!(profile = %profile.name, error = %error, "the stream ended badly");
        }

        let streamed = accumulator.finish();

        info!(
            profile = %profile.name,
            auth = %auth_name,
            status,
            latency_ms = started.elapsed().as_millis(),
            ttft_ms = streamed.ttft_ms,
            chunks = streamed.view.chunks,
            "streamed call completed"
        );
        // Already redacted by the accumulator, and for a refusal it is the whole
        // body: an endpoint that says no says it in one shot, not in frames.
        log_refusal(&profile.name, status, &streamed.body_text);

        let response = streamed_response(status, response_headers, started, streamed);

        Ok(CallOutcome {
            profile: profile.name.clone(),
            auth: auth_name,
            request: view,
            curl,
            response,
            retried_after_unauthorized: retried,
        })
    }

    /// Sends the same request again and compares the vectors.
    ///
    /// This is the check that catches a replica quietly serving a different model
    /// from its siblings: everything else about its answer looks perfectly fine.
    ///
    /// # Errors
    ///
    /// Only if an extra exchange itself fails. A second response that decodes
    /// differently is a failed check, not an error.
    async fn check_determinism(
        &self,
        profile: &Profile,
        request: &RenderedRequest,
        first: Option<&Vectors>,
        input: &CallInput,
        redactor: &Redactor,
    ) -> Result<CheckOutcome, ExecError> {
        let Some(first) = first else {
            return Ok(CheckOutcome::Fail {
                detail: "the first response produced no vector to compare against".to_owned(),
            });
        };

        let mut worst = 0.0_f32;
        for run in 2..=input.repeat {
            let raw = transport::send(&self.client, request, profile.timeout()).await?;
            if raw.status != 200 {
                return Ok(CheckOutcome::Fail {
                    detail: format!("run {run} answered {} instead of 200", raw.status),
                });
            }

            let (_, vectors) = response_view(profile, &raw, redactor, false, input.input.len());
            let Some(deviation) = vectors.as_ref().and_then(|v| first.max_deviation(v)) else {
                return Ok(CheckOutcome::Fail {
                    detail: format!(
                        "run {run} returned a different number of vectors, or different widths"
                    ),
                });
            };
            worst = worst.max(deviation);
        }

        debug!(profile = %profile.name, runs = input.repeat, deviation = worst, "determinism checked");
        Ok(CheckOutcome::from(worst <= input.tolerance, || {
            format!(
                "the same input produced vectors differing by up to {worst:e}, above the {:e} tolerance",
                input.tolerance
            )
        }))
    }
}

/// What happens while a streamed call is in flight.
///
/// Only the two things a caller cannot wait for. Everything else — the decode
/// trace, the counters, the curl — is in the [`CallOutcome`] at the end, because
/// none of it is knowable before the stream closes.
#[derive(Debug, Clone)]
pub enum CallEvent {
    /// The response head arrived. A `401` is known here, long before any body.
    Open {
        /// HTTP status.
        status: u16,
        /// Response headers, masked.
        headers: BTreeMap<String, String>,
    },
    /// A chunk carried text.
    Delta {
        /// The text, masked.
        text: String,
    },
}

/// Everything a finished stream produced.
struct Streamed {
    completion: Completion,
    decode: DecodeTrace,
    view: StreamView,
    ttft_ms: Option<u64>,
    body_text: String,
    last: Option<Value>,
}

/// Turns a stream of bytes into a completion, as it arrives.
struct StreamAccumulator<'a> {
    spec: &'a DecodeSpec,
    redactor: &'a Redactor,
    parser: FrameParser,
    trace: DecodeTrace,
    view: StreamView,
    started: std::time::Instant,
    ttft_ms: Option<u64>,
    text: String,
    body: String,
    last: Option<Value>,
    /// Set by the `[DONE]` sentinel. A stop reason in the final chunk also counts
    /// as a clean end, but that is only knowable once decoding runs.
    sentinel: bool,
}

impl<'a> StreamAccumulator<'a> {
    fn new(
        spec: &'a DecodeSpec,
        redactor: &'a Redactor,
        framing: Framing,
        started: std::time::Instant,
    ) -> Self {
        Self {
            spec,
            redactor,
            parser: FrameParser::new(framing),
            trace: DecodeTrace::default(),
            view: StreamView {
                framing: Some(framing),
                ..StreamView::default()
            },
            started,
            ttft_ms: None,
            text: String::new(),
            body: String::new(),
            last: None,
            sentinel: false,
        }
    }

    /// Takes one network read: buffer it, and handle whatever frames it completed.
    fn push(&mut self, text: &str, at: std::time::Instant, on_event: &mut impl FnMut(CallEvent)) {
        self.view.bytes += text.len() as u64;
        self.body.push_str(text);
        if self.view.first_chunk_ms.is_none() {
            self.view.first_chunk_ms = Some(millis(self.started, at));
        }

        for frame in self.parser.push(text) {
            self.handle(frame, at, on_event);
        }
    }

    fn handle(
        &mut self,
        frame: Frame,
        at: std::time::Instant,
        on_event: &mut impl FnMut(CallEvent),
    ) {
        match frame {
            Frame::Chunk(value) => {
                self.view.chunks += 1;
                if let Some(delta) = stream::delta(&value, self.spec, &mut self.trace) {
                    self.view.deltas += 1;
                    // Time to first *token*, not to first chunk: a role-only
                    // preamble is not an answer starting to arrive.
                    if self.ttft_ms.is_none() {
                        self.ttft_ms = Some(millis(self.started, at));
                    }
                    let masked = self.redactor.text(&delta);
                    self.text.push_str(&masked);
                    on_event(CallEvent::Delta { text: masked });
                }
                self.last = Some(*value);
            }
            Frame::Done => self.sentinel = true,
            Frame::Unparsable(_) => self.view.unparsable += 1,
        }
    }

    fn finish(mut self) -> Streamed {
        for frame in self.parser.finish() {
            let at = std::time::Instant::now();
            self.handle(frame, at, &mut |_| {});
        }

        // Masked *before* it is decoded, not after. `usage` is commonly pointed
        // at `$` — Ollama puts its counters at the top level — and `Usage` keeps
        // the object it read verbatim, so decoding the unmasked chunk would
        // carry a credential the endpoint quoted back at us straight into the
        // response. The non-streaming path decodes the masked value for the same
        // reason.
        let last = self.last.as_ref().map(|value| self.redactor.json(value));

        let mut completion = match &last {
            Some(last) => chat::decode_tail(last, self.spec, &mut self.trace),
            None => Completion::default(),
        };
        // The aggregate is the answer. It never comes from a path, so nothing in
        // the trace claims it did.
        completion.content = (!self.text.is_empty()).then(|| self.text.clone());

        stream::record_miss(self.spec, &mut self.trace);

        // Two ways to end on purpose: the sentinel, or a final chunk that says
        // why it stopped. Anything else and the connection merely went quiet,
        // which is what a proxy cutting a long generation looks like.
        self.view.terminated = self.sentinel || completion.finish_reason.is_some();

        Streamed {
            completion,
            decode: self.trace,
            view: self.view,
            ttft_ms: self.ttft_ms,
            body_text: self.redactor.text(&self.body),
            last,
        }
    }
}

/// Shapes a finished stream like any other response.
///
/// Everything downstream — the UI, the decode trace panel, the assertions — then
/// works on a streamed answer without knowing it was one.
fn streamed_response(
    status: u16,
    headers: BTreeMap<String, String>,
    started: std::time::Instant,
    streamed: Streamed,
) -> ResponseView {
    ResponseView {
        http: HttpMeta {
            status,
            headers,
            latency_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            ttft_ms: streamed.ttft_ms,
        },
        body_text: Some(streamed.body_text),
        raw: streamed.last,
        // Vectors are the only thing that gets elided, and there are none here.
        elided: false,
        // A stream has no single body to fail to parse; a frame that did not
        // parse is counted in `stream.unparsable` instead.
        json_error: None,
        decoded: Some(Decoded::Completion(streamed.completion)),
        decode: streamed.decode,
        stream: Some(streamed.view),
    }
}

/// How much of a refused body reaches the log.
///
/// Long enough for the endpoint's own sentence — "maximum context length is
/// 32768 tokens, however you requested 61402" — short enough that a gateway
/// answering an HTML error page does not take the journal with it.
const REFUSAL_EXCERPT: usize = 512;

/// Puts the endpoint's own words in the log when it says no.
///
/// The body is on the trace either way, but a `status=400` alone is a question,
/// not an answer, and whoever is reading the log is reading it precisely because
/// they do not have the trace open. Not an error: a refusal is still an answer,
/// so this is a `warn`, and the run carries on.
fn log_refusal(profile: &str, status: u16, body: &str) {
    if status < 400 {
        return;
    }
    warn!(
        %profile,
        status,
        body = %excerpt(body, REFUSAL_EXCERPT),
        "the endpoint refused the call"
    );
}

/// First `limit` characters of `text`, on one line, with an ellipsis when there
/// was more. Counts characters, not bytes: a body is not always ASCII, and
/// slicing one mid-codepoint would panic.
fn excerpt(text: &str, limit: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(limit) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text,
    }
}

fn millis(from: std::time::Instant, to: std::time::Instant) -> u64 {
    u64::try_from(to.saturating_duration_since(from).as_millis()).unwrap_or(u64::MAX)
}

fn render_context(profile: &Profile, input: &CallInput) -> RenderContext {
    RenderContext {
        messages: input.messages.clone(),
        input: input.input.clone(),
        model: input.model.clone(),
        params: input.params.clone(),
        stream: input.stream,
        ..RenderContext::default()
    }
    .with_tools(&profile.tools)
    .and_tools(input.extra_tools.clone())
}

/// Headers common to every attempt: `content-type`, then whatever the profile adds.
fn base_headers(profile: &Profile) -> Result<HeaderMap, ExecError> {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));

    for (name, value) in &profile.headers {
        let name = HeaderName::try_from(name.to_ascii_lowercase()).map_err(|_| {
            ExecError::InvalidHeader {
                profile: profile.name.clone(),
                header: name.clone(),
            }
        })?;
        let value = HeaderValue::from_str(value).map_err(|_| ExecError::InvalidHeader {
            profile: profile.name.clone(),
            header: name.as_str().to_owned(),
        })?;
        headers.insert(name, value);
    }

    Ok(headers)
}

/// Scrubs the rendered body carried by a template error before it reaches the
/// user: `params` comes from the caller and could hold anything.
fn redact_render_error(error: RenderError, redactor: &Redactor) -> RenderError {
    match error {
        RenderError::InvalidJson {
            message,
            line,
            column,
            rendered,
        } => RenderError::InvalidJson {
            message: redactor.text(&message),
            line,
            column,
            rendered: redactor.text(&rendered),
        },
        other => other,
    }
}

fn request_view(request: &RenderedRequest, redactor: &Redactor) -> RequestView {
    RequestView {
        method: request.method,
        url: request.url.to_string(),
        headers: request.display_headers(redactor),
        body: redactor.text(&request.body),
    }
}

/// Builds the response view, and hands back the raw vectors for an embedding
/// response so the determinism check has something to compare.
fn response_view(
    profile: &Profile,
    raw: &transport::RawResponse,
    redactor: &Redactor,
    include_vectors: bool,
    inputs: usize,
) -> (ResponseView, Option<Vectors>) {
    let http = HttpMeta {
        status: raw.status,
        headers: redactor.headers(&raw.headers),
        latency_ms: u64::try_from(raw.latency.as_millis()).unwrap_or(u64::MAX),
        ttft_ms: None,
    };

    let (parsed, json_error) = match serde_json::from_str::<Value>(&raw.body) {
        Ok(value) => (Some(redactor.json(&value)), None),
        Err(error) => (None, Some(error.to_string())),
    };

    // The "never render a whole vector" rule has to bite here too, or `raw` hands
    // over everything the summaries were careful not to.
    let elided = profile.kind == ProfileKind::Embedding && !include_vectors;

    // Nothing to decode out of a body that is not JSON. A `decode.script`
    // replaces the cascades entirely — validation rejects declaring both, so
    // there is no precedence rule here, just two paths.
    let (decoded, decode, vectors) = match (&parsed, profile.kind) {
        (Some(value), ProfileKind::Chat) => {
            let (completion, trace) = match &profile.decode.script {
                Some(source) => script::decode_chat(value, raw.status, &http.headers, source),
                None => chat::decode(value, &profile.decode),
            };
            (Some(Decoded::Completion(completion)), trace, None)
        }
        (Some(value), ProfileKind::Embedding) => {
            let (embedding, vectors, trace) = match &profile.decode.script {
                Some(source) => script::decode_embedding(
                    value,
                    raw.status,
                    &http.headers,
                    source,
                    include_vectors,
                ),
                None => embedding::decode(value, &profile.decode, include_vectors),
            };
            let checks = EmbeddingChecks::evaluate(&embedding, inputs, profile.expect.dimensions);
            let result = EmbeddingResult { embedding, checks };
            (
                Some(Decoded::Embedding(Box::new(result))),
                trace,
                Some(vectors),
            )
        }
        _ => (None, DecodeTrace::default(), None),
    };

    let view = ResponseView {
        http,
        body_text: (!elided).then(|| redactor.text(&raw.body)),
        raw: if elided {
            parsed.as_ref().map(embedding::elide)
        } else {
            parsed
        },
        elided,
        json_error,
        decoded,
        decode,
        stream: None,
    };
    (view, vectors)
}

/// Why a call could not be run.
///
/// A response from the endpoint is never one of these, whatever its status.
#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    /// No such profile in the directory.
    #[error("unknown profile `{0}`")]
    UnknownProfile(String),

    /// A header declared in the profile is not usable.
    #[error("profile `{profile}`: header `{header}` is not a valid HTTP header")]
    InvalidHeader {
        /// Profile that declared it.
        profile: String,
        /// The offending header.
        header: String,
    },

    /// The credential could not be produced.
    #[error(transparent)]
    Auth(#[from] AuthError),

    /// The request body could not be rendered.
    #[error(transparent)]
    Render(#[from] RenderError),

    /// The exchange itself failed.
    #[error(transparent)]
    Transport(#[from] TransportError),
}

#[cfg(test)]
mod tests {
    use super::{REFUSAL_EXCERPT, excerpt};

    #[test]
    fn excerpt_collapses_whitespace_and_keeps_short_bodies_whole() {
        assert_eq!(
            excerpt("{\n  \"error\": \"too long\"\n}", REFUSAL_EXCERPT),
            "{ \"error\": \"too long\" }"
        );
    }

    #[test]
    fn excerpt_truncates_on_a_character_boundary() {
        let body = "é".repeat(10);
        assert_eq!(excerpt(&body, 4), "éééé…");
    }
}
