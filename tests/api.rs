//! End-to-end tests through the real HTTP API against a mock endpoint.
//!
//! Everything goes over the wire, on purpose: a credential leak or a broken
//! serialisation only shows up once the response has actually been rendered.

use std::time::Duration;

use mire::api::{AppState, router};
use mire::config::ConfigStore;
use mire::exec::Runner;
use mire::transport::{self, TransportOptions};
use mire::uploads::UploadStore;
use serde_json::{Value, json};
use tempfile::TempDir;
use wiremock::matchers::{body_string_contains, header, header_exists, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

/// A running `mire` with a throwaway configuration directory.
struct Harness {
    /// Where the API lives, base path included.
    base: String,
    /// The server root, whatever the base path is.
    root: String,
    client: reqwest::Client,
    /// Every watched configuration directory, in precedence order. Usually one;
    /// [`Harness::start_layered`] is what makes it more.
    dirs: Vec<TempDir>,
    /// Where `POST /api/uploads` writes. Held so it outlives the server.
    uploads: TempDir,
    /// Dropping this stops the file watcher, so it has to be held.
    _watcher: notify::RecommendedWatcher,
}

impl Harness {
    /// Starts `mire` on an ephemeral port with the given files.
    async fn start(files: &[(&str, String)]) -> Self {
        Self::start_at(files, "").await
    }

    /// Same, mounted under a base path — what a notebook proxy does to you.
    async fn start_at(files: &[(&str, String)], base_path: &str) -> Self {
        Self::start_layered(&[files], base_path).await
    }

    /// Starts `mire` over several configuration directories, in the order given.
    async fn start_layered(layers: &[&[(&str, String)]], base_path: &str) -> Self {
        let dirs: Vec<TempDir> = layers
            .iter()
            .map(|files| {
                let dir = TempDir::new().expect("temp dir");
                for (name, body) in *files {
                    std::fs::write(dir.path().join(name), body).expect("write file");
                }
                dir
            })
            .collect();
        let paths: Vec<&std::path::Path> = dirs.iter().map(TempDir::path).collect();

        let http = transport::build_client(&TransportOptions::default()).expect("http client");
        let config = ConfigStore::load(&paths, http.clone()).expect("load configuration");
        let watcher = mire::config::watch(std::sync::Arc::clone(&config)).expect("watch");

        // Its own directory, not a corner of the watched one: an upload landing
        // in the configuration directory would fire the file watcher and get read
        // as a profile that failed to parse.
        let uploads = TempDir::new().expect("uploads dir");

        let state = AppState {
            runner: Runner::new(config, http),
            uploads: std::sync::Arc::new(UploadStore::new(uploads.path())),
            base_path: base_path.into(),
            // Unset on purpose: the tests exercise the path a browser actually
            // takes, where the callback comes from the request.
            public_url: None,
        };

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, router(state)).await.expect("serve");
        });

        Self {
            base: format!("http://{address}{base_path}"),
            root: format!("http://{address}"),
            client: reqwest::Client::new(),
            dirs,
            uploads,
            _watcher: watcher,
        }
    }

    /// `POST /api/uploads`, with one file part.
    async fn upload(&self, filename: &str, body: &[u8]) -> (u16, Value) {
        let part = reqwest::multipart::Part::bytes(body.to_vec())
            .file_name(filename.to_owned())
            .mime_str("application/octet-stream")
            .expect("mime");
        let response = self
            .client
            .post(format!("{}/api/uploads", self.base))
            .multipart(reqwest::multipart::Form::new().part("file", part))
            .send()
            .await
            .expect("upload");

        let status = response.status().as_u16();
        (status, response.json().await.expect("json"))
    }

    /// Everything sitting in the upload directory, sorted.
    fn stored(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.uploads.path())
            .map(|entries| {
                entries
                    .map(|entry| entry.expect("entry").file_name().to_string_lossy().into())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// `POST /api/agent`, collecting the whole event stream.
    ///
    /// Returns `(event name, payload)` in order, so a test can assert on what
    /// arrived *and* when.
    async fn agent(&self, body: Value) -> (u16, Vec<(String, Value)>) {
        let response = self
            .client
            .post(format!("{}/api/agent", self.base))
            .json(&body)
            .send()
            .await
            .expect("call mire");
        let status = response.status().as_u16();
        let text = response.text().await.expect("read stream");

        (status, events(&text))
    }

    /// `POST /api/call/stream`, collecting the whole event stream.
    async fn stream(&self, body: Value) -> (u16, Vec<(String, Value)>) {
        let response = self
            .client
            .post(format!("{}/api/call/stream", self.base))
            .json(&body)
            .send()
            .await
            .expect("call mire");
        let status = response.status().as_u16();
        let text = response.text().await.expect("read stream");
        (status, events(&text))
    }

    /// Fetches a path from the server root, ignoring the base path.
    async fn raw(&self, route: &str) -> reqwest::Response {
        self.client
            .get(format!("{}{route}", self.root))
            .send()
            .await
            .expect("get")
    }

    /// Writes a file into the first watched directory.
    fn write(&self, name: &str, body: &str) {
        self.write_in(0, name, body);
    }

    /// Writes a file into the `index`-th watched directory.
    fn write_in(&self, index: usize, name: &str, body: &str) {
        std::fs::write(self.dirs[index].path().join(name), body).expect("write file");
    }

    /// Polls `route` until `ready` accepts the response, or gives up.
    ///
    /// The watcher debounces, so "it reloaded" is only observable by waiting for
    /// it; a fixed sleep would be either flaky or slow.
    async fn wait_for(&self, route: &str, ready: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..60 {
            let body = self.get(route).await;
            if ready(&body) {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{route} never reached the expected state");
    }

    /// `POST /api/call`, returning the status and the body as text and JSON.
    async fn call(&self, body: Value) -> (u16, String, Value) {
        let response = self
            .client
            .post(format!("{}/api/call", self.base))
            .json(&body)
            .send()
            .await
            .expect("call mire");
        let status = response.status().as_u16();
        let text = response.text().await.expect("read body");
        let json = serde_json::from_str(&text).unwrap_or(Value::Null);
        (status, text, json)
    }

    async fn get(&self, route: &str) -> Value {
        self.client
            .get(format!("{}{route}", self.base))
            .send()
            .await
            .expect("get")
            .json()
            .await
            .expect("json")
    }

    /// `POST` to any route under the base path.
    async fn post(&self, route: &str, body: Value) -> (u16, Value) {
        let response = self
            .client
            .post(format!("{}{route}", self.base))
            .json(&body)
            .send()
            .await
            .expect("post");
        let status = response.status().as_u16();
        let text = response.text().await.expect("read body");
        (status, serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// `GET` any route under the base path, as text. For the pages a browser
    /// lands on, which are HTML rather than JSON.
    async fn get_text(&self, route: &str) -> (u16, String) {
        let response = self
            .client
            .get(format!("{}{route}", self.base))
            .send()
            .await
            .expect("get");
        let status = response.status().as_u16();
        (status, response.text().await.expect("read body"))
    }
}

/// Splits a server-sent event stream into `(name, payload)` pairs, in order.
///
/// Shared by both streaming endpoints so there is exactly one hand-rolled SSE
/// parser on the test side — a second one is a second thing that can be subtly
/// wrong while the assertions still pass.
fn events(text: &str) -> Vec<(String, Value)> {
    let mut events = Vec::new();
    let mut name = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("event: ") {
            rest.trim().clone_into(&mut name);
        } else if let Some(rest) = line.strip_prefix("data: ") {
            if let Ok(payload) = serde_json::from_str(rest) {
                events.push((name.clone(), payload));
            }
        }
    }
    events
}

/// An OpenAI-shaped chat profile pointing at `url`.
fn openai_profile(url: &str) -> String {
    format!(
        r#"
name: chat
kind: chat
url: {url}
timeout_ms: 5000
request:
  template: |
    {{"model": "m", "messages": {{{{ messages | tojson }}}}}}
decode:
  content: ["$.choices[0].message.content", "$.output.text"]
  tool_calls: ["$.choices[0].message.tool_calls"]
  finish_reason: ["$.choices[0].finish_reason", "$.stop_reason"]
  usage: ["$.usage"]
  error: ["$.error", "$.detail"]
"#
    )
}

/// An embedding profile pointing at `url`.
///
/// Built by substitution rather than `format!`: a `MiniJinja` template inside a
/// format string needs four braces to mean two, and that way lies madness.
fn embedding_profile(url: &str, expect_dimensions: Option<usize>) -> String {
    const TEMPLATE: &str = r#"
name: embed
kind: embedding
url: __URL__
timeout_ms: 5000
request:
  template: |
    {"model": "e", "input": {{ input | tojson }}}
decode:
  vectors: ["$.data[*].embedding", "$.embeddings"]
  usage: ["$.usage"]
"#;

    let mut profile = TEMPLATE.replace("__URL__", url);
    if let Some(value) = expect_dimensions {
        use std::fmt::Write;
        let _ = writeln!(profile, "expect:\n  dimensions: {value}");
    }
    profile
}

/// An OpenAI-shaped embeddings response with `count` vectors of `width`.
fn embedding_response(count: usize, width: usize) -> Value {
    let data: Vec<Value> = (0..count)
        .map(|i| {
            let base = f32::from(u8::try_from(i).unwrap()) + 1.0;
            let vector: Vec<f32> = (0..width)
                .map(|j| base * (f32::from(u16::try_from(j).unwrap()) + 1.0) / 1000.0)
                .collect();
            json!({"object": "embedding", "index": i, "embedding": vector})
        })
        .collect();
    json!({"object": "list", "model": "e", "data": data, "usage": {"prompt_tokens": 3}})
}

fn openai_response() -> Value {
    json!({
        "choices": [{
            "message": {"role": "assistant", "content": "pong"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 4, "completion_tokens": 1}
    })
}

#[tokio::test]
async fn a_call_hands_back_the_request_it_sent_and_its_curl() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    let rendered = body["request"]["body"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(rendered).unwrap(),
        json!({"model": "m", "messages": [{"role": "user", "content": "ping"}]})
    );
    // The body is returned exactly as the template produced it, spacing included.
    assert!(
        rendered.starts_with(r#"{"model": "m", "messages": "#),
        "{rendered}"
    );
    assert!(
        body["curl"].as_str().unwrap().contains("curl -sS -X POST"),
        "{}",
        body["curl"]
    );
    // What the endpoint actually received is what was handed back.
    let received = &server.received_requests().await.unwrap()[0];
    assert_eq!(std::str::from_utf8(&received.body).unwrap(), rendered);
}

#[tokio::test]
async fn an_expected_401_is_a_result_not_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "unauthorized"})))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "anonymous", "prompt": "ping"}))
        .await;

    // The API call succeeded. Whether a 401 is good news is the caller's business.
    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 401);
    assert_eq!(body["retriedAfterUnauthorized"], false);
    assert_eq!(body["auth"], "anonymous");
}

#[tokio::test]
async fn a_token_provider_authenticates_and_the_response_decodes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer s3cr3t-token-value"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", server.uri())),
        ),
        (
            "auth.yaml",
            "providers:\n  - name: pasted\n    kind: token\n".to_owned(),
        ),
    ])
    .await;

    let (status, text, body) = harness
        .call(json!({
            "profile": "chat",
            "auth": "pasted",
            "prompt": "ping",
            "token": "s3cr3t-token-value"
        }))
        .await;

    assert_eq!(status, 200, "{text}");
    assert_eq!(body["response"]["http"]["status"], 200);
    assert_eq!(body["response"]["decoded"]["content"], "pong");
    assert_eq!(body["response"]["decoded"]["finishReason"], "stop");
    assert_eq!(body["response"]["decoded"]["usage"]["totalTokens"], 5);
    assert_eq!(
        body["response"]["decode"]["matched"]["content"],
        "$.choices[0].message.content"
    );
}

#[tokio::test]
async fn a_credential_never_appears_anywhere_in_the_response() {
    const TOKEN: &str = "s3cr3t-token-value";

    let server = MockServer::start().await;
    // A hostile-but-realistic endpoint: it echoes the credential back in its error.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(403).set_body_json(json!({
            "error": format!("token {TOKEN} is not allowed here"),
            "seen_header": format!("Bearer {TOKEN}"),
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", server.uri())),
        ),
        (
            "auth.yaml",
            "providers:\n  - name: pasted\n    kind: token\n".to_owned(),
        ),
    ])
    .await;

    let (status, text, body) = harness
        .call(json!({
            "profile": "chat",
            "auth": "pasted",
            "prompt": "ping",
            "token": TOKEN
        }))
        .await;

    assert_eq!(status, 200);
    assert!(
        !text.contains(TOKEN),
        "the credential leaked into the API response: {text}"
    );
    assert_eq!(body["request"]["headers"]["authorization"], "***");
    assert!(!body["curl"].as_str().unwrap().contains(TOKEN));
    assert_eq!(body["response"]["http"]["status"], 403);
}

#[tokio::test]
async fn authorization_does_not_survive_a_cross_host_redirect() {
    let destination = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/elsewhere"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&destination)
        .await;

    let entry = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            // 307 keeps the method and the body, so only the headers differ.
            ResponseTemplate::new(307).insert_header(
                "location",
                format!("{}/elsewhere", destination.uri()).as_str(),
            ),
        )
        .mount(&entry)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", entry.uri())),
        ),
        (
            "auth.yaml",
            "providers:\n  - name: pasted\n    kind: token\n".to_owned(),
        ),
    ])
    .await;

    let (status, text, _) = harness
        .call(json!({
            "profile": "chat",
            "auth": "pasted",
            "prompt": "ping",
            "token": "s3cr3t-token-value"
        }))
        .await;
    assert_eq!(status, 200, "{text}");

    let forwarded: Vec<Request> = destination.received_requests().await.unwrap();
    assert_eq!(forwarded.len(), 1, "the redirect should have been followed");
    assert!(
        forwarded[0].headers.get("authorization").is_none(),
        "the credential followed the redirect to another host"
    );

    let original: Vec<Request> = entry.received_requests().await.unwrap();
    assert!(
        original[0].headers.get("authorization").is_some(),
        "the credential should still reach the profile's own host"
    );
}

#[tokio::test]
async fn a_timeout_is_reported_as_a_gateway_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(5)))
        .mount(&server)
        .await;

    let profile = openai_profile(&format!("{}/v1/chat/completions", server.uri()))
        .replace("timeout_ms: 5000", "timeout_ms: 150");
    let harness = Harness::start(&[("chat.yaml", profile)]).await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    assert_eq!(status, 504);
    assert_eq!(body["code"], "endpoint_timeout");
    assert!(body["message"].as_str().unwrap().contains("150"));
}

#[tokio::test]
async fn a_body_that_is_not_json_comes_back_raw_with_the_parse_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502).set_body_string("<html>gateway exploded</html>"))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 502);
    assert_eq!(
        body["response"]["bodyText"],
        "<html>gateway exploded</html>"
    );
    assert!(body["response"]["raw"].is_null());
    assert!(body["response"]["jsonError"].is_string());
    assert!(body["response"]["decoded"].is_null());
}

#[tokio::test]
async fn an_empty_body_is_survivable() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 204);
    assert_eq!(body["response"]["bodyText"], "");
    assert!(body["response"]["jsonError"].is_string());
}

#[tokio::test]
async fn every_decode_path_missing_is_reported_rather_than_hidden() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"totally": {"other": 1}})))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    assert!(body["response"]["decoded"]["content"].is_null());
    // The raw JSON is still there, and so is the list of paths that were tried —
    // which is exactly what you need to fix the profile.
    assert_eq!(body["response"]["raw"]["totally"]["other"], 1);
    assert_eq!(
        body["response"]["decode"]["missed"]["content"],
        json!(["$.choices[0].message.content", "$.output.text"])
    );
}

#[tokio::test]
async fn a_refusal_comes_back_with_the_endpoints_own_sentence() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": {
                "message": "This model's maximum context length is 32768 tokens.",
                "type": "invalid_request_error",
                "code": "context_length_exceeded",
                "param": "messages"
            }
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    // The call worked. The endpoint is the one that said no.
    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 400);
    let error = &body["response"]["error"];
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("maximum context length"),
        "{error}"
    );
    assert_eq!(error["type"], "invalid_request_error");
    assert_eq!(error["code"], "context_length_exceeded");
    // Whatever normalisation did not understand is still there.
    assert_eq!(error["raw"]["param"], "messages");
    assert_eq!(body["response"]["decode"]["matched"]["error"], "$.error");
}

/// The case that makes this worth decoding at all: a gateway that swallows the
/// upstream failure, answers `200`, and puts the complaint in the body — where
/// nothing watching the status would ever look.
#[tokio::test]
async fn an_error_reported_under_a_two_hundred_is_still_found() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"detail": "no capacity upstream"})),
        )
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 200);
    assert_eq!(body["response"]["error"]["message"], "no capacity upstream");
    assert_eq!(body["response"]["decode"]["matched"]["error"], "$.detail");
}

/// The other half of the rule: a good answer carries no error, and the paths
/// that went looking for one do not clutter the trace.
#[tokio::test]
async fn a_good_answer_reports_no_error_and_no_error_miss() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    assert!(body["response"]["error"].is_null());
    assert!(body["response"]["decode"]["missed"]["error"].is_null());
}

/// A refusal no path reaches is a blind spot in the profile, and saying so is
/// the whole point of the trace.
#[tokio::test]
async fn a_refusal_no_path_reaches_lists_what_was_tried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({"failure": "overloaded"})))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    assert!(body["response"]["error"].is_null());
    assert_eq!(
        body["response"]["decode"]["missed"]["error"],
        json!(["$.error", "$.detail"])
    );
}

#[tokio::test]
async fn a_non_openai_shape_decodes_through_the_fallback_cascade() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "output": {"text": "answer from an endpoint that never heard of OpenAI"},
            "stop_reason": "end_turn"
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    assert_eq!(
        body["response"]["decoded"]["content"],
        "answer from an endpoint that never heard of OpenAI"
    );
    assert_eq!(body["response"]["decoded"]["finishReason"], "end_turn");
    assert_eq!(
        body["response"]["decode"]["matched"]["content"],
        "$.output.text"
    );
}

#[tokio::test]
async fn an_unknown_profile_is_a_404_and_an_unknown_provider_too() {
    let harness =
        Harness::start(&[("chat.yaml", openai_profile("https://models.internal/v1"))]).await;

    let (status, _, body) = harness.call(json!({"profile": "nope"})).await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "unknown_profile");

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "nope"}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "unknown_auth_provider");
}

#[tokio::test]
async fn a_template_that_renders_broken_json_says_what_it_produced() {
    let profile = r#"
name: broken
kind: chat
url: https://models.internal/v1
request:
  template: '{"a": 1,{% if tools %}"tools": [],{% endif %}}'
"#;
    let harness = Harness::start(&[("broken.yaml", profile.to_owned())]).await;

    let (status, _, body) = harness
        .call(json!({"profile": "broken", "prompt": "ping"}))
        .await;

    assert_eq!(status, 422);
    assert_eq!(body["code"], "rendered_body_is_not_json");
    assert_eq!(body["detail"]["rendered"], r#"{"a": 1,}"#);
}

/// The point of listing several directories: a base somebody else maintains,
/// and yours on top, without copying theirs to change one line.
#[tokio::test]
async fn a_later_directory_overrides_a_profile_the_earlier_one_declared() {
    let harness = Harness::start_layered(
        &[
            &[("chat.yaml", openai_profile("https://models.internal/v1"))],
            &[("chat.yaml", openai_profile("https://staging.internal/v1"))],
        ],
        "",
    )
    .await;

    let body = harness.get("/api/profiles").await;

    assert_eq!(body["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(body["profiles"][0]["url"], "https://staging.internal/v1");
    // An override is not a load failure, so it belongs in the log and not here.
    assert!(body["issues"].as_array().unwrap().is_empty());
}

/// Every listed directory is watched, not just the first: an override you have
/// to restart for is an override you stop using.
#[tokio::test]
async fn editing_the_second_directory_reloads_too() {
    let harness = Harness::start_layered(
        &[
            &[("chat.yaml", openai_profile("https://models.internal/v1"))],
            &[],
        ],
        "",
    )
    .await;

    harness.write_in(
        1,
        "chat.yaml",
        &openai_profile("https://staging.internal/v1"),
    );

    let body = harness
        .wait_for("/api/profiles", |body| {
            body["profiles"][0]["url"] == "https://staging.internal/v1"
        })
        .await;
    assert_eq!(body["profiles"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn the_profile_listing_reports_broken_files_without_hiding_the_good_ones() {
    let harness = Harness::start(&[
        ("chat.yaml", openai_profile("https://models.internal/v1")),
        ("broken.yaml", "name: broken\nkind: not-a-kind\n".to_owned()),
    ])
    .await;

    let body = harness.get("/api/profiles").await;
    assert_eq!(body["profiles"].as_array().unwrap().len(), 1);
    assert_eq!(body["profiles"][0]["name"], "chat");
    assert_eq!(body["profiles"][0]["hasDecode"], true);
    assert_eq!(body["issues"].as_array().unwrap().len(), 1);
    assert!(
        body["issues"][0]["file"]
            .as_str()
            .unwrap()
            .ends_with("broken.yaml")
    );
}

#[tokio::test]
async fn the_prompt_listing_keeps_the_file_order_and_names_the_entry_it_dropped() {
    let harness = Harness::start(&[
        ("chat.yaml", openai_profile("https://models.internal/v1")),
        (
            "prompts.yaml",
            // Second one has no text, so it puts nothing in the box and is not a
            // prompt. The other two still are, in the order written.
            "prompts:\n  - name: zebra\n    text: ping\n  - name: hollow\n    text: ''\n  - name: alpha\n    text: |\n      one\n      two\n"
                .to_owned(),
        ),
    ])
    .await;

    let body = harness.get("/api/prompts").await;
    let prompts = body["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 2);
    assert_eq!(prompts[0]["name"], "zebra", "the file's order, not sorted");
    assert_eq!(prompts[1]["text"], "one\ntwo\n");
    assert!(
        body["issues"][0]["message"]
            .as_str()
            .unwrap()
            .contains("hollow")
    );

    // And the library is not a profile, however much it looks like one from the
    // outside: it lives in the same directory and ends in `.yaml`.
    let profiles = harness.get("/api/profiles").await;
    assert_eq!(profiles["profiles"].as_array().unwrap().len(), 1);
    assert!(profiles["issues"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn editing_the_prompt_library_takes_effect_without_a_restart() {
    let harness =
        Harness::start(&[("chat.yaml", openai_profile("https://models.internal/v1"))]).await;

    let body = harness.get("/api/prompts").await;
    assert!(
        body["prompts"].as_array().unwrap().is_empty(),
        "no file is no prompts and no complaint"
    );
    assert!(body["issues"].as_array().unwrap().is_empty());

    harness.write("prompts.yaml", "prompts:\n  - name: ping\n    text: ping\n");

    let body = harness
        .wait_for("/api/prompts", |body| {
            !body["prompts"].as_array().unwrap().is_empty()
        })
        .await;
    assert_eq!(body["prompts"][0]["name"], "ping");
}

#[tokio::test]
async fn the_auth_listing_always_offers_anonymous() {
    let harness = Harness::start(&[(
        "auth.yaml",
        "providers:\n  - name: gateway\n    kind: token\n    value:\n      env: MODEL_TOKEN\n    allowed_hosts:\n      - models.internal\n"
            .to_owned(),
    )])
    .await;

    let body = harness.get("/api/auth").await;
    let providers = body["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 2);
    assert_eq!(providers[0]["name"], "anonymous");
    assert_eq!(providers[1]["name"], "gateway");
    assert_eq!(providers[1]["needsValue"], false);
    // Where the credential may go, said on the wire: the UI stops offering it
    // against a profile pointing anywhere else.
    assert_eq!(providers[1]["allowedHosts"][0], "models.internal");
    assert_eq!(providers[0]["allowedHosts"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn a_profile_whose_provider_has_no_credential_fails_clearly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header_exists("authorization"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", server.uri())),
        ),
        (
            "auth.yaml",
            "providers:\n  - name: pasted\n    kind: token\n".to_owned(),
        ),
    ])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "pasted", "prompt": "ping"}))
        .await;

    assert_eq!(status, 400);
    assert_eq!(body["code"], "credential_unavailable");
    assert!(body["message"].as_str().unwrap().contains("pasted"));
}

#[tokio::test]
async fn the_openapi_document_is_served_and_describes_the_call_endpoint() {
    let harness = Harness::start(&[]).await;

    let spec = harness.get("/openapi.json").await;
    assert_eq!(spec["info"]["title"], "mire");
    assert!(spec["paths"]["/api/call"]["post"].is_object());
    assert!(spec["paths"]["/api/profiles/{name}"]["get"].is_object());
    // Ops plumbing stays out of the product surface.
    assert!(spec["paths"]["/healthz"].is_null());
}

#[tokio::test]
async fn editing_the_auth_registry_takes_effect_without_a_restart() {
    let harness =
        Harness::start(&[("chat.yaml", openai_profile("https://models.internal/v1"))]).await;

    let body = harness.get("/api/auth").await;
    assert_eq!(
        body["providers"].as_array().unwrap().len(),
        1,
        "anonymous only to begin with"
    );

    harness.write(
        "auth.yaml",
        "providers:\n  - name: gateway\n    kind: token\n    value:\n      env: MODEL_TOKEN\n",
    );

    let body = harness
        .wait_for("/api/auth", |body| {
            body["providers"].as_array().unwrap().len() == 2
        })
        .await;
    assert_eq!(body["providers"][1]["name"], "gateway");
    assert!(body["issues"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_reloaded_provider_is_immediately_usable_on_a_call() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer reloaded-token-value"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    // Before the edit, the provider does not exist.
    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "pasted"}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "unknown_auth_provider");

    harness.write(
        "auth.yaml",
        "providers:\n  - name: pasted\n    kind: token\n",
    );
    harness
        .wait_for("/api/auth", |body| {
            body["providers"].as_array().unwrap().len() == 2
        })
        .await;

    let (status, text, body) = harness
        .call(json!({
            "profile": "chat",
            "auth": "pasted",
            "prompt": "ping",
            "token": "reloaded-token-value"
        }))
        .await;
    assert_eq!(status, 200, "{text}");
    assert_eq!(body["response"]["decoded"]["content"], "pong");
}

#[tokio::test]
async fn a_broken_auth_registry_is_reported_and_leaves_anonymous_working() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", server.uri())),
        ),
        ("auth.yaml", "providers: [unclosed\n".to_owned()),
    ])
    .await;

    let body = harness.get("/api/auth").await;
    assert_eq!(body["providers"].as_array().unwrap().len(), 1);
    assert_eq!(body["issues"].as_array().unwrap().len(), 1);
    assert!(body["issues"][0]["line"].is_number());

    // mire came up anyway, and the one thing that never needs configuring works.
    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "anonymous", "prompt": "ping"}))
        .await;
    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 401);
}

#[tokio::test]
async fn one_bad_provider_does_not_take_the_others_down() {
    let harness = Harness::start(&[(
        "auth.yaml",
        r#"
providers:
  - name: bad
    kind: token
    header: "not a header"
  - name: good
    kind: token
    value:
      env: MODEL_TOKEN
"#
        .to_owned(),
    )])
    .await;

    let body = harness.get("/api/auth").await;
    let names: Vec<&str> = body["providers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["anonymous", "good"]);
    assert_eq!(body["issues"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_embedding_response_is_summarised_and_checked() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_response(2, 1024)))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), Some(1024)),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "embed", "input": ["one", "two"]}))
        .await;
    assert_eq!(status, 200);

    let decoded = &body["response"]["decoded"];
    assert_eq!(decoded["kind"], "embedding");
    assert_eq!(decoded["count"], 2);
    assert_eq!(
        decoded["dimensions"],
        json!({"kind": "uniform", "value": 1024})
    );
    assert_eq!(decoded["encoding"], "float");
    assert_eq!(decoded["usage"]["promptTokens"], 3);

    let checks = &decoded["checks"];
    assert_eq!(checks["count"]["status"], "pass");
    assert_eq!(checks["dimensions"]["status"], "pass");
    assert_eq!(checks["finite"]["status"], "pass");
    assert_eq!(checks["nonZeroNorm"]["status"], "pass");
    // Determinism needs a second run, and says so instead of silently passing.
    assert_eq!(checks["determinism"]["status"], "skipped");
    assert!(
        checks["determinism"]["reason"]
            .as_str()
            .unwrap()
            .contains("repeat")
    );
}

#[tokio::test]
async fn a_vector_is_never_rendered_whole_unless_asked_for() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_response(1, 1024)))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), None),
    )])
    .await;

    let (_, text, body) = harness
        .call(json!({"profile": "embed", "input": "one"}))
        .await;

    // A 1024-float payload would dwarf everything else in the response.
    assert!(
        text.len() < 4000,
        "the response carries a whole vector: {} bytes",
        text.len()
    );
    assert_eq!(body["response"]["elided"], true);
    assert!(body["response"]["bodyText"].is_null());
    assert!(
        body["response"]["raw"]["data"][0]["embedding"]
            .as_str()
            .unwrap()
            .contains("1024 values elided")
    );
    // The parts of the raw tree worth reading survive.
    assert_eq!(body["response"]["raw"]["model"], "e");
    assert_eq!(body["response"]["raw"]["data"][0]["index"], 0);

    let summary = &body["response"]["decoded"]["vectors"][0];
    assert_eq!(summary["dimensions"], 1024);
    assert_eq!(summary["sample"].as_array().unwrap().len(), 8);
    assert_eq!(
        summary["histogram"]["buckets"].as_array().unwrap().len(),
        24
    );
    assert!(summary["norm"].as_f64().unwrap() > 0.0);
    assert!(body["response"]["decoded"]["full"].is_null());

    // Explicitly asking is the only way to get the payload.
    let (_, text, body) = harness
        .call(json!({"profile": "embed", "input": "one", "includeVectors": true}))
        .await;
    assert!(text.len() > 4000);
    assert_eq!(body["response"]["elided"], false);
    assert_eq!(
        body["response"]["decoded"]["full"][0]
            .as_array()
            .unwrap()
            .len(),
        1024
    );
    assert!(body["response"]["bodyText"].is_string());
}

#[tokio::test]
async fn base64_vectors_decode_to_the_same_shape() {
    let values: Vec<f32> = vec![1.0, 0.0, 0.0, 0.0];
    let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_le_bytes()).collect();
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"embedding": encoded}]
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), Some(4)),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "embed", "input": "one", "includeVectors": true}))
        .await;

    assert_eq!(status, 200);
    let decoded = &body["response"]["decoded"];
    assert_eq!(decoded["encoding"], "base64");
    assert_eq!(
        decoded["dimensions"],
        json!({"kind": "uniform", "value": 4})
    );
    assert_eq!(decoded["checks"]["dimensions"]["status"], "pass");
    assert_eq!(decoded["full"][0], json!([1.0, 0.0, 0.0, 0.0]));
}

#[tokio::test]
async fn a_deterministic_endpoint_passes_the_repeat_check() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_response(1, 8)))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), None),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "embed", "input": "one", "repeat": 2}))
        .await;

    assert_eq!(status, 200);
    assert_eq!(
        body["response"]["decoded"]["checks"]["determinism"]["status"],
        "pass"
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn a_replica_serving_something_else_fails_the_repeat_check() {
    let server = MockServer::start().await;
    // First call answers one thing, the next answers another — which is exactly
    // what a load-balanced replica running a different model looks like.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"embedding": [0.1, 0.2, 0.3]}]
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"embedding": [0.1, 0.2, 0.4]}]
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), None),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "embed", "input": "one", "repeat": 2}))
        .await;

    assert_eq!(status, 200);
    let determinism = &body["response"]["decoded"]["checks"]["determinism"];
    assert_eq!(determinism["status"], "fail");
    assert!(
        determinism["detail"]
            .as_str()
            .unwrap()
            .contains("tolerance"),
        "{determinism}"
    );
}

#[tokio::test]
async fn a_width_that_does_not_match_the_profile_is_reported() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_response(1, 384)))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), Some(1024)),
    )])
    .await;

    let (_, _, body) = harness
        .call(json!({"profile": "embed", "input": ["one", "two"]}))
        .await;

    let checks = &body["response"]["decoded"]["checks"];
    assert_eq!(checks["dimensions"]["status"], "fail");
    assert!(
        checks["dimensions"]["detail"]
            .as_str()
            .unwrap()
            .contains("expected 1024 dimensions, got 384")
    );
    // Two inputs went out, one answer came back.
    assert_eq!(checks["count"]["status"], "fail");
    assert!(
        checks["count"]["detail"]
            .as_str()
            .unwrap()
            .contains("sent 2 input(s), got 1 item(s)")
    );
}

#[tokio::test]
async fn a_hole_in_a_vector_fails_the_finiteness_check() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"embedding": [0.1, null, 0.3]},
                {"embedding": [0.0, 0.0, 0.0]}
            ]
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), None),
    )])
    .await;

    let (_, _, body) = harness
        .call(json!({"profile": "embed", "input": ["one", "two"]}))
        .await;

    let checks = &body["response"]["decoded"]["checks"];
    assert_eq!(checks["count"]["status"], "pass");
    assert_eq!(checks["finite"]["status"], "fail");
    assert_eq!(checks["nonZeroNorm"]["status"], "fail");
    assert!(
        checks["nonZeroNorm"]["detail"]
            .as_str()
            .unwrap()
            .contains("[1]")
    );
}

#[tokio::test]
async fn a_multi_vector_endpoint_answers_one_item_per_input() {
    let server = MockServer::start().await;
    // One vector per token rather than one per input — what a late-interaction
    // model, or a server with pooling turned off, answers.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                {"index": 0, "embedding": [[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]]},
                {"index": 1, "embedding": [[0.7, 0.8], [0.9, 1.0]]}
            ]
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), Some(2)),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "embed", "input": ["one", "two"]}))
        .await;
    assert_eq!(status, 200);

    let decoded = &body["response"]["decoded"];
    assert_eq!(decoded["count"], 2);
    assert_eq!(decoded["vectorCount"], 5);
    assert_eq!(decoded["vectorsPerItem"], json!([3, 2]));
    assert_eq!(
        decoded["dimensions"],
        json!({"kind": "uniform", "value": 2})
    );
    // Two inputs, two answers: the count check is about inputs, not vectors.
    assert_eq!(decoded["checks"]["count"]["status"], "pass");
    assert_eq!(decoded["checks"]["dimensions"]["status"], "pass");
    assert_eq!(decoded["checks"]["nonZeroNorm"]["status"], "pass");
    // Each summary says which input it belongs to.
    assert_eq!(decoded["vectors"][3]["item"], 1);
    assert_eq!(decoded["vectors"][3]["position"], 0);
}

#[tokio::test]
async fn one_input_worth_of_token_vectors_is_not_read_as_a_batch() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{"index": 0, "embedding": [[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]]}]
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), None),
    )])
    .await;

    let (_, _, body) = harness
        .call(json!({"profile": "embed", "input": "one"}))
        .await;

    let decoded = &body["response"]["decoded"];
    assert_eq!(decoded["count"], 1);
    assert_eq!(decoded["vectorsPerItem"], json!([3]));
    assert_eq!(decoded["checks"]["count"]["status"], "pass");
}

#[tokio::test]
async fn a_single_string_input_is_accepted_and_rendered_as_a_list() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(embedding_response(1, 4)))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile(&format!("{}/v1/embeddings", server.uri()), None),
    )])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "embed", "input": "just one"}))
        .await;

    assert_eq!(status, 200);
    let rendered: Value = serde_json::from_str(body["request"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(rendered["input"], json!(["just one"]));
}

// ---------------------------------------------------------------------------
// OIDC client_credentials
// ---------------------------------------------------------------------------

/// A mock identity provider: discovery document plus token endpoint.
async fn idp(tokens: &[&str], expires_in: u64) -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/realms/models/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": server.uri(),
            "token_endpoint": format!("{}/realms/models/token", server.uri()),
        })))
        .mount(&server)
        .await;

    // Each mounted mock answers once, in order, so successive exchanges can hand
    // out different tokens.
    for token in tokens {
        Mock::given(method("POST"))
            .and(path("/realms/models/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "access_token": token,
                "token_type": "Bearer",
                "expires_in": expires_in,
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }

    server
}

/// The client secret, on disk.
///
/// A file rather than an environment variable: `unsafe_code` is forbidden, so a
/// test cannot call `set_var` — and a file is what a real deployment mounts anyway.
fn client_secret_file() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("mire-client-secret-{}", std::process::id()));
    std::fs::write(&path, "the-client-secret\n").expect("write client secret");
    path
}

fn oidc_registry(idp_uri: &str, extra: &str) -> String {
    let secret = client_secret_file();
    format!(
        "providers:\n  \
         - name: workload\n    \
         kind: oidc\n    \
         issuer: {idp_uri}/realms/models\n    \
         client_id: mire\n    \
         client_secret:\n      \
         file: {}\n{extra}",
        secret.display()
    )
}

/// Counts the exchanges the identity provider actually served.
async fn exchanges(idp: &MockServer) -> usize {
    idp.received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| request.url.path().ends_with("/token"))
        .count()
}

#[tokio::test]
async fn oidc_discovers_the_token_endpoint_and_authenticates() {
    let idp = idp(&["access-token-1"], 300).await;
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer access-token-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&model)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", model.uri())),
        ),
        ("auth.yaml", oidc_registry(&idp.uri(), "")),
    ])
    .await;

    let (status, text, body) = harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200, "{text}");
    assert_eq!(body["response"]["decoded"]["content"], "pong");
    assert_eq!(body["request"]["headers"]["authorization"], "***");
    assert!(!text.contains("access-token-1"), "the access token leaked");
    assert!(
        !text.contains("the-client-secret"),
        "the client secret leaked"
    );

    // Discovery ran, then exactly one exchange.
    assert_eq!(exchanges(&idp).await, 1);
}

#[tokio::test]
async fn oidc_caches_the_token_across_calls_and_refetches_once_it_expires() {
    // A long-lived token: the second call must reuse it.
    let long = idp(&["cached-token", "second-token"], 3600).await;
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&model)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", model.uri())),
        ),
        ("auth.yaml", oidc_registry(&long.uri(), "")),
    ])
    .await;

    for _ in 0..3 {
        let (status, _, _) = harness
            .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
            .await;
        assert_eq!(status, 200);
    }
    assert_eq!(
        exchanges(&long).await,
        1,
        "the token should have been cached"
    );

    // `expires_in: 0` means the token is stale the moment it arrives.
    let short = idp(&["one", "two"], 0).await;
    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", model.uri())),
        ),
        ("auth.yaml", oidc_registry(&short.uri(), "")),
    ])
    .await;

    for _ in 0..2 {
        harness
            .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
            .await;
    }
    assert_eq!(
        exchanges(&short).await,
        2,
        "an expired token must be refetched"
    );
}

#[tokio::test]
async fn a_rejected_cached_token_is_refreshed_and_replayed_exactly_once() {
    let idp = idp(&["stale-token", "fresh-token"], 3600).await;
    let model = MockServer::start().await;
    // The endpoint has decided `stale-token` is no good; `fresh-token` is fine.
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer stale-token"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&model)
        .await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer fresh-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&model)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", model.uri())),
        ),
        ("auth.yaml", oidc_registry(&idp.uri(), "")),
    ])
    .await;

    // First call mints the token. A 401 on a token we just minted is the
    // endpoint's answer, not a stale credential — so no replay.
    let (_, _, body) = harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;
    assert_eq!(body["response"]["http"]["status"], 401);
    assert_eq!(body["retriedAfterUnauthorized"], false);
    assert_eq!(exchanges(&idp).await, 1);

    // Second call reuses the cached token, is rejected, refreshes once, and wins.
    let (_, _, body) = harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;
    assert_eq!(body["retriedAfterUnauthorized"], true);
    assert_eq!(body["response"]["http"]["status"], 200);
    assert_eq!(exchanges(&idp).await, 2);
}

#[tokio::test]
async fn a_persistent_401_gives_up_after_one_replay() {
    let idp = idp(&["t1", "t2", "t3", "t4"], 3600).await;
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "nope"})))
        .mount(&model)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", model.uri())),
        ),
        ("auth.yaml", oidc_registry(&idp.uri(), "")),
    ])
    .await;

    harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;
    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;

    // The reported result is the 401, not an error, and it stopped after one replay.
    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 401);
    assert_eq!(body["retriedAfterUnauthorized"], true);
    assert_eq!(model.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn scope_and_audience_reach_the_token_endpoint() {
    let idp = idp(&["scoped-token"], 3600).await;
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&model)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", model.uri())),
        ),
        (
            "auth.yaml",
            oidc_registry(
                &idp.uri(),
                "    scope: [openid, models:read]\n    audience: https://models.internal\n",
            ),
        ),
    ])
    .await;

    harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;

    let requests = idp.received_requests().await.unwrap();
    let exchange = requests
        .iter()
        .find(|request| request.url.path().ends_with("/token"))
        .expect("the token endpoint should have been called");
    let form = String::from_utf8(exchange.body.clone()).unwrap();

    assert!(form.contains("grant_type=client_credentials"), "{form}");
    assert!(form.contains("client_id=mire"), "{form}");
    assert!(form.contains("scope=openid+models%3Aread"), "{form}");
    assert!(
        form.contains("audience=https%3A%2F%2Fmodels.internal"),
        "{form}"
    );
}

#[tokio::test]
async fn a_projected_service_account_token_is_reread_on_every_exchange() {
    let assertion = std::env::temp_dir().join(format!("mire-sa-{}", std::process::id()));
    std::fs::write(&assertion, "first-projected-token\n").unwrap();

    let idp = idp(&["a", "b"], 0).await;
    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&model)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", model.uri())),
        ),
        (
            "auth.yaml",
            format!(
                "providers:\n  \
                 - name: workload\n    \
                 kind: oidc\n    \
                 issuer: {}/realms/models\n    \
                 client_id: mire\n    \
                 client_assertion:\n      \
                 file: {}\n",
                idp.uri(),
                assertion.display()
            ),
        ),
    ])
    .await;

    harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;

    // The token is rotated under us, as a projected volume does.
    std::fs::write(&assertion, "rotated-projected-token\n").unwrap();
    harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;

    let requests = idp.received_requests().await.unwrap();
    let forms: Vec<String> = requests
        .iter()
        .filter(|request| request.url.path().ends_with("/token"))
        .map(|request| String::from_utf8(request.body.clone()).unwrap())
        .collect();

    assert_eq!(forms.len(), 2);
    assert!(
        forms[0].contains("client_assertion=first-projected-token"),
        "{}",
        forms[0]
    );
    assert!(
        forms[1].contains("client_assertion=rotated-projected-token"),
        "{}",
        forms[1]
    );
    assert!(
        forms[0].contains(
            "client_assertion_type=urn%3Aietf%3Aparams%3Aoauth%3Aclient-assertion-type%3Ajwt-bearer"
        ),
        "{}",
        forms[0]
    );

    std::fs::remove_file(&assertion).unwrap();
}

#[tokio::test]
async fn a_failed_token_exchange_explains_itself_without_leaking_the_secret() {
    let idp = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/realms/models/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "token_endpoint": format!("{}/realms/models/token", idp.uri()),
        })))
        .mount(&idp)
        .await;
    // A realistic hostile answer: the IdP quotes the secret back at us.
    Mock::given(method("POST"))
        .and(path("/realms/models/token"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "error": "invalid_client",
            "error_description": "secret the-client-secret is not valid for client mire",
        })))
        .mount(&idp)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile("https://models.internal/v1/chat/completions"),
        ),
        ("auth.yaml", oidc_registry(&idp.uri(), "")),
    ])
    .await;

    let (status, text, body) = harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;

    assert_eq!(status, 502);
    assert_eq!(body["code"], "oidc_token_exchange_failed");
    assert!(body["message"].as_str().unwrap().contains("invalid_client"));
    assert!(
        !text.contains("the-client-secret"),
        "the client secret leaked into the error: {text}"
    );
}

#[tokio::test]
async fn an_unreachable_issuer_is_a_clear_discovery_error() {
    let idp = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&idp)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile("https://models.internal/v1/chat/completions"),
        ),
        ("auth.yaml", oidc_registry(&idp.uri(), "")),
    ])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;

    assert_eq!(status, 502);
    assert_eq!(body["code"], "oidc_discovery_failed");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains(".well-known/openid-configuration")
    );
}

#[tokio::test]
async fn an_explicit_token_endpoint_skips_discovery() {
    let idp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/realms/models/token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": "direct-token",
            "expires_in": 3600,
        })))
        .mount(&idp)
        .await;

    let model = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer direct-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&model)
        .await;

    let harness = Harness::start(&[
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", model.uri())),
        ),
        (
            "auth.yaml",
            oidc_registry(
                &idp.uri(),
                &format!("    token_endpoint: {}/realms/models/token\n", idp.uri()),
            ),
        ),
    ])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "workload", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["decoded"]["content"], "pong");
    // No discovery request was made at all.
    assert!(
        idp.received_requests()
            .await
            .unwrap()
            .iter()
            .all(|request| request.url.path().ends_with("/token"))
    );
}

// ---------------------------------------------------------------------------
// The embedded UI, and living under a proxy prefix
// ---------------------------------------------------------------------------

#[tokio::test]
async fn without_a_base_path_the_ui_is_served_with_no_base_tag_at_all() {
    let harness = Harness::start(&[]).await;

    let response = harness.raw("/").await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
    // No tag on purpose. `<base href="/">` would be right here and wrong behind
    // a proxy that strips its prefix before forwarding — and the same binary has
    // to serve both. Left alone, the bundle's relative URLs resolve against the
    // document's own URL, which is correct in both places.
    let html = response.text().await.unwrap();
    assert!(!html.contains("<base "), "{html}");
}

#[tokio::test]
async fn an_unknown_path_falls_back_to_the_ui_rather_than_404() {
    let harness = Harness::start(&[]).await;

    // A client-side route the server has never heard of.
    let response = harness.raw("/some/deep/client/route").await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );
}

#[tokio::test]
async fn a_missing_asset_404s_instead_of_being_dressed_up_as_the_ui() {
    let harness = Harness::start(&[]).await;

    // What a document and a bundle that disagree ask for. Answering the SPA
    // fallback would make this a `200 text/html`, which reaches the developer as
    // "expected a JavaScript module, got text/html" and names nothing useful.
    let response = harness.raw("/assets/index-DoesNotExist.js").await;
    assert_eq!(response.status(), 404);
    assert_ne!(
        response.headers()["content-type"],
        "text/html; charset=utf-8"
    );

    // The directory itself is not an asset, so it stays a client-side route.
    assert_eq!(harness.raw("/assets").await.status(), 200);
}

#[tokio::test]
async fn a_base_path_moves_the_api_the_docs_and_the_ui_together() {
    const PREFIX: &str = "/notebook/team/gleroy/proxy/8787";

    let harness = Harness::start_at(
        &[("chat.yaml", openai_profile("https://models.internal/v1"))],
        PREFIX,
    )
    .await;

    // The API answers under the prefix...
    let body = harness.get("/api/profiles").await;
    assert_eq!(body["profiles"][0]["name"], "chat");

    // ...and nowhere else. Outside the prefix, nothing exists.
    assert_eq!(harness.raw("/api/profiles").await.status(), 404);

    // Except at the root, which points you at the prefix rather than 404-ing at
    // you — the mistake everyone makes once after setting `--base-path`.
    let landing = harness
        .client
        .get(&harness.root)
        .send()
        .await
        .expect("get root");
    assert!(
        landing.url().path().starts_with(PREFIX),
        "{}",
        landing.url()
    );

    // The UI is told where it lives, so its relative asset and fetch URLs resolve.
    let html = harness
        .raw(&format!("{PREFIX}/"))
        .await
        .text()
        .await
        .unwrap();
    assert!(
        html.contains(&format!(r#"<base href="{PREFIX}/">"#)),
        "{html}"
    );

    // Without the trailing slash too — that is how a proxy link is usually written.
    let html = harness.raw(PREFIX).await.text().await.unwrap();
    assert!(
        html.contains(&format!(r#"<base href="{PREFIX}/">"#)),
        "{html}"
    );

    // The OpenAPI document and the reference move with everything else.
    assert_eq!(
        harness
            .raw(&format!("{PREFIX}/openapi.json"))
            .await
            .status(),
        200
    );
    let docs = harness
        .raw(&format!("{PREFIX}/docs"))
        .await
        .text()
        .await
        .unwrap();
    // The reference asks for the spec relatively, so it resolves under the prefix.
    assert!(docs.contains("'openapi.json'"), "{docs}");
}

#[tokio::test]
async fn healthz_answers_under_the_base_path() {
    let harness = Harness::start_at(&[], "/mire").await;

    let response = harness.raw("/mire/healthz").await;
    assert_eq!(response.status(), 200);
    assert_eq!(response.text().await.unwrap(), "ok");
}

// ---------------------------------------------------------------------------
// Rhai scripts
// ---------------------------------------------------------------------------

/// A profile that builds its body and reads its answer with scripts.
///
/// The response shape is one no cascade reaches: the answer is split across
/// segments that have to be filtered and joined, and the stop reason is a
/// boolean.
fn scripted_profile(url: &str) -> String {
    const TEMPLATE: &str = r#"
name: scripted
kind: chat
url: __URL__
timeout_ms: 5000
request:
  script: |
    #{
      model: "exotic-1",
      turns: messages.map(|m| `${m.role}: ${m.content}`),
    }
decode:
  script: |
    let text = "";
    for segment in raw.segments {
      if segment.kind == "text" { text += segment.value; }
    }
    #{
      content: text,
      finish_reason: if raw.complete { "stop" } else { "length" },
      usage: #{ prompt_tokens: raw.counters.inbound, completion_tokens: raw.counters.outbound },
    }
"#;
    TEMPLATE.replace("__URL__", url)
}

#[tokio::test]
async fn a_request_script_builds_the_body() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "segments": [{"kind": "text", "value": "pong"}],
            "complete": true,
            "counters": {"inbound": 1, "outbound": 1}
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[("scripted.yaml", scripted_profile(&server.uri()))]).await;

    let (status, text, body) = harness
        .call(json!({"profile": "scripted", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200, "{text}");
    let rendered: Value = serde_json::from_str(body["request"]["body"].as_str().unwrap()).unwrap();
    // A map came back from the script and was serialised for us.
    assert_eq!(rendered["model"], "exotic-1");
    assert_eq!(rendered["turns"], json!(["user: ping"]));
}

#[tokio::test]
async fn a_decode_script_reads_a_shape_no_cascade_could() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "segments": [
                {"kind": "text", "value": "the answer "},
                {"kind": "trace", "value": "IGNORE ME"},
                {"kind": "text", "value": "in two pieces"}
            ],
            "complete": true,
            "counters": {"inbound": 12, "outbound": 5}
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "scripted.yaml",
        scripted_profile(&format!("{}/v1", server.uri())),
    )])
    .await;

    let (status, text, body) = harness
        .call(json!({"profile": "scripted", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200, "{text}");
    let decoded = &body["response"]["decoded"];
    assert_eq!(decoded["content"], "the answer in two pieces");
    assert_eq!(decoded["finishReason"], "stop");
    assert_eq!(decoded["usage"]["totalTokens"], 17);
    // The trace names the script the same way it names a winning path.
    assert_eq!(body["response"]["decode"]["matched"]["content"], "<script>");
}

#[tokio::test]
async fn a_runaway_decode_script_is_traced_rather_than_fatal() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"a": 1})))
        .mount(&server)
        .await;

    let profile = format!(
        r"
name: runaway
kind: chat
url: {}/v1
request:
  template: '{{}}'
decode:
  script: |
    let n = 0;
    loop {{ n += 1; }}
",
        server.uri()
    );
    let harness = Harness::start(&[("runaway.yaml", profile)]).await;

    let (status, _, body) = harness
        .call(json!({"profile": "runaway", "prompt": "ping"}))
        .await;

    // The call succeeded; the script did not, and says so next to the raw body.
    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 200);
    assert_eq!(body["response"]["raw"]["a"], 1);
    let issue = &body["response"]["decode"]["issues"][0];
    assert_eq!(issue["field"], "script");
    assert!(
        issue["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("operation"),
        "{issue}"
    );
}

#[tokio::test]
async fn a_failing_request_script_is_a_422_naming_the_script() {
    let profile = r"
name: broken-script
kind: chat
url: https://models.internal/v1
request:
  script: |
    messages.no_such_method()
";
    let harness = Harness::start(&[("broken.yaml", profile.to_owned())]).await;

    let (status, _, body) = harness
        .call(json!({"profile": "broken-script", "prompt": "ping"}))
        .await;

    assert_eq!(status, 422);
    assert_eq!(body["code"], "request_script_error");
}

#[tokio::test]
async fn declaring_both_a_template_and_a_script_is_rejected_at_load() {
    let both_request = r"
name: ambiguous-request
kind: chat
url: https://models.internal/v1
request:
  template: '{}'
  script: '#{}'
";
    let both_decode = r#"
name: ambiguous-decode
kind: chat
url: https://models.internal/v1
request:
  template: '{}'
decode:
  script: '#{}'
  content: ["$.a"]
"#;
    let no_source = r"
name: no-source
kind: chat
url: https://models.internal/v1
request: {}
";

    let harness = Harness::start(&[
        ("a.yaml", both_request.to_owned()),
        ("b.yaml", both_decode.to_owned()),
        ("c.yaml", no_source.to_owned()),
    ])
    .await;

    let body = harness.get("/api/profiles").await;
    assert!(body["profiles"].as_array().unwrap().is_empty());

    let messages: Vec<String> = body["issues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|issue| issue["message"].as_str().unwrap().to_owned())
        .collect();
    let all = messages.join(" | ");
    assert!(all.contains("a request is one body"), "{all}");
    assert!(all.contains("replaces the cascades"), "{all}");
    assert!(
        all.contains("needs a `template`, a `script` or a `multipart`"),
        "{all}"
    );
}

#[tokio::test]
async fn a_script_that_does_not_compile_names_the_file_at_startup() {
    let profile = r"
name: broken
kind: chat
url: https://models.internal/v1
request:
  script: |
    let x = ;
";
    let harness = Harness::start(&[("broken.yaml", profile.to_owned())]).await;

    let body = harness.get("/api/profiles").await;
    assert!(body["profiles"].as_array().unwrap().is_empty());
    assert!(
        body["issues"][0]["file"]
            .as_str()
            .unwrap()
            .ends_with("broken.yaml"),
        "{body}"
    );
    assert!(
        body["issues"][0]["message"]
            .as_str()
            .unwrap()
            .contains("does not compile"),
        "{body}"
    );
}

// ---------------------------------------------------------------------------
// Agent mode
// ---------------------------------------------------------------------------

/// A chat profile with one simulated tool.
fn agent_profile(url: &str, extra: &str) -> String {
    const TEMPLATE: &str = r#"
name: agent
kind: chat
url: __URL__
timeout_ms: 5000
request:
  template: |
    {"model": "m", "messages": {{ messages | tojson }}, "tools": {{ tools | tojson }}}
decode:
  content: ["$.choices[0].message.content"]
  tool_calls: ["$.choices[0].message.tool_calls"]
  finish_reason: ["$.choices[0].finish_reason"]
tools:
  - name: get_weather
    description: Look up the weather in a city.
    schema:
      type: object
      properties:
        city:
          type: string
      required:
        - city
    response: '{"temp": 21, "conditions": "clear"}'
__EXTRA__
"#;
    TEMPLATE.replace("__URL__", url).replace("__EXTRA__", extra)
}

/// An assistant turn asking for a tool.
fn wants_tool(city: &str) -> Value {
    json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "tool_calls": [{
                    "id": format!("call_{city}"),
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": format!("{{\"city\": \"{city}\"}}")}
                }]
            },
            "finish_reason": "tool_calls"
        }]
    })
}

/// A final assistant turn.
fn final_answer(text: &str) -> Value {
    json!({
        "choices": [{
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }]
    })
}

#[tokio::test]
async fn an_agent_answers_a_tool_call_and_stops_when_the_model_is_done() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wants_tool("Paris")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(final_answer("It is 21 degrees in Paris.")),
        )
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "agent.yaml",
        agent_profile(&format!("{}/v1", server.uri()), ""),
    )])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "agent", "prompt": "weather in Paris?"}))
        .await;

    assert_eq!(status, 200);
    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, vec!["turn", "turn", "done"], "{events:?}");

    // Turn 1: the model asked for the tool, and the tool answered.
    let (_, first) = &events[0];
    assert_eq!(first["index"], 1);
    assert_eq!(first["tools"][0]["call"]["name"], "get_weather");
    assert_eq!(
        first["tools"][0]["call"]["arguments"],
        json!({"city": "Paris"})
    );
    assert!(
        first["tools"][0]["schemaErrors"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        first["tools"][0]["result"],
        r#"{"temp": 21, "conditions": "clear"}"#
    );
    // Nothing left the process, so there is no status to report — which is a
    // different statement from "answered nothing", and the absence makes it.
    assert!(first["tools"][0]["status"].is_null());
    assert_eq!(first["decision"]["decision"], "continue");

    // Turn 2: the result came back in, and the model finished.
    let (_, second) = &events[1];
    assert_eq!(second["index"], 2);
    assert_eq!(
        second["call"]["response"]["decoded"]["content"],
        "It is 21 degrees in Paris."
    );
    assert_eq!(second["decision"]["stop"]["outcome"], "stopped");

    // The tool result really was fed back to the model.
    let sent = second["call"]["request"]["body"].as_str().unwrap();
    assert!(sent.contains(r#""role":"tool""#), "{sent}");
    assert!(sent.contains("21"), "{sent}");

    let (_, done) = &events[2];
    assert_eq!(done["stop"]["outcome"], "stopped");
    assert_eq!(done["stop"]["reason"]["predicate"], "noToolCalls");
    assert_eq!(done["turns"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn a_model_asking_for_the_same_thing_twice_is_stopped_when_the_profile_asks() {
    let server = MockServer::start().await;
    // Always the same call: a loop, not progress.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wants_tool("Paris")))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "agent.yaml",
        agent_profile(
            &format!("{}/v1", server.uri()),
            "agent:\n  stop_when:\n    repeated_call: true\n",
        ),
    )])
    .await;

    let (_, events) = harness
        .agent(json!({"profile": "agent", "prompt": "weather?"}))
        .await;

    let (_, done) = events.last().unwrap();
    assert_eq!(done["stop"]["outcome"], "repeatedCall");
    assert_eq!(done["stop"]["tool"], "get_weather");
    assert_eq!(done["stop"]["atTurn"], 2);
    // Two turns, not ten: it did not burn the budget getting there.
    assert_eq!(done["turns"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn the_same_call_twice_is_not_watched_for_unless_asked() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wants_tool("Paris")))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "agent.yaml",
        agent_profile(
            &format!("{}/v1", server.uri()),
            "agent:\n  max_iterations: 3\n",
        ),
    )])
    .await;

    let (_, events) = harness
        .agent(json!({"profile": "agent", "prompt": "weather?"}))
        .await;

    // The default lets it keep going; only the turn budget ends the run.
    let (_, done) = events.last().unwrap();
    assert_eq!(done["stop"]["outcome"], "maxIterations");
    assert_eq!(done["stop"]["limit"], 3);
    assert_eq!(done["turns"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn arguments_that_do_not_match_the_schema_are_reported_and_still_answered() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "function": {"name": "get_weather", "arguments": "{\"town\": 42}"}
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_answer("sorry")))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "agent.yaml",
        agent_profile(&format!("{}/v1", server.uri()), ""),
    )])
    .await;

    let (_, events) = harness
        .agent(json!({"profile": "agent", "prompt": "weather?"}))
        .await;

    let tool = &events[0].1["tools"][0];
    let errors = tool["schemaErrors"].as_array().unwrap();
    assert!(
        !errors.is_empty(),
        "`city` is required and was not sent: {tool}"
    );
    // The model still got an answer, so it has a chance to correct itself.
    assert!(tool["error"].is_null());
    assert_eq!(events.last().unwrap().1["stop"]["outcome"], "stopped");
}

#[tokio::test]
async fn a_tool_the_profile_never_declared_is_answered_with_an_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{"id": "c1", "function": {"name": "launch_missiles", "arguments": "{}"}}]
                },
                "finish_reason": "tool_calls"
            }]
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_answer("understood")))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "agent.yaml",
        agent_profile(&format!("{}/v1", server.uri()), ""),
    )])
    .await;

    let (_, events) = harness
        .agent(json!({"profile": "agent", "prompt": "go"}))
        .await;

    let tool = &events[0].1["tools"][0];
    assert!(tool["error"].as_str().unwrap().contains("launch_missiles"));
    assert!(tool["result"].as_str().unwrap().contains("error"));
}

#[tokio::test]
async fn a_backend_that_never_reports_a_finish_reason_is_called_out_rather_than_looping_quietly() {
    let server = MockServer::start().await;
    // Content every time, no `finish_reason`, no tool calls.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role": "assistant", "content": "still talking"}}]
        })))
        .mount(&server)
        .await;

    // Stop only on `finish_reason`, which this endpoint never sends.
    let profile = agent_profile(
        &format!("{}/v1", server.uri()),
        "agent:\n  max_iterations: 3\n  stop_when:\n    no_tool_calls: false\n    finish_reason_in: [stop, end_turn]\n",
    );
    let harness = Harness::start(&[("agent.yaml", profile)]).await;

    let (_, events) = harness
        .agent(json!({"profile": "agent", "prompt": "hello"}))
        .await;

    let (_, done) = events.last().unwrap();
    // Not `maxIterations`: the loop was not slow, it was unfalsifiable.
    assert_eq!(done["stop"]["outcome"], "predicateNeverEvaluable");
    assert_eq!(done["stop"]["predicate"], "stop_when.finish_reason_in");
    assert_eq!(done["stop"]["turns"], 3);
}

#[tokio::test]
async fn the_turn_budget_is_honoured_and_overridable_per_run() {
    let server = MockServer::start().await;
    // A different city every time, so the repetition guard never fires.
    for city in ["Paris", "Lyon", "Nantes", "Brest", "Dijon", "Rennes"] {
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(wants_tool(city)))
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }

    let harness = Harness::start(&[(
        "agent.yaml",
        agent_profile(
            &format!("{}/v1", server.uri()),
            "agent:\n  max_iterations: 6\n",
        ),
    )])
    .await;

    let (_, events) = harness
        .agent(json!({"profile": "agent", "prompt": "tour de France", "maxIterations": 3}))
        .await;

    let (_, done) = events.last().unwrap();
    assert_eq!(done["stop"]["outcome"], "maxIterations");
    assert_eq!(done["stop"]["limit"], 3);
    assert_eq!(done["turns"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn a_tool_can_answer_from_a_script_that_reads_its_arguments() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wants_tool("Lyon")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_answer("done")))
        .mount(&server)
        .await;

    let profile = agent_profile(&format!("{}/v1", server.uri()), "").replace(
        r#"    response: '{"temp": 21, "conditions": "clear"}'"#,
        "    script: '`{\"city\": \"${arguments.city}\", \"turn\": ${turn}}`'",
    );
    let harness = Harness::start(&[("agent.yaml", profile)]).await;

    let (_, events) = harness
        .agent(json!({"profile": "agent", "prompt": "weather in Lyon?"}))
        .await;

    assert_eq!(
        events[0].1["tools"][0]["result"],
        r#"{"city": "Lyon", "turn": 1}"#
    );
}

#[tokio::test]
async fn agent_mode_refuses_an_embedding_profile_before_streaming_anything() {
    let harness = Harness::start(&[(
        "embed.yaml",
        embedding_profile("https://models.internal/v1/embeddings", None),
    )])
    .await;

    let response = harness
        .client
        .post(format!("{}/api/agent", harness.base))
        .json(&json!({"profile": "embed", "prompt": "ping"}))
        .send()
        .await
        .expect("call mire");

    // A 422 is more use than a stream whose first event is a failure.
    assert_eq!(response.status(), 422);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["code"], "not_a_chat_profile");
}

#[tokio::test]
async fn a_credential_never_appears_in_an_agent_trace() {
    const TOKEN: &str = "s3cr3t-agent-token";

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"role": "assistant", "content": format!("your token {TOKEN} is fine")},
                "finish_reason": "stop"
            }]
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[
        (
            "agent.yaml",
            agent_profile(&format!("{}/v1", server.uri()), ""),
        ),
        (
            "auth.yaml",
            "providers:\n  - name: pasted\n    kind: token\n".to_owned(),
        ),
    ])
    .await;

    let response = harness
        .client
        .post(format!("{}/api/agent", harness.base))
        .json(&json!({
            "profile": "agent",
            "auth": "pasted",
            "prompt": "ping",
            "token": TOKEN
        }))
        .send()
        .await
        .expect("call mire");
    let stream = response.text().await.unwrap();

    assert!(
        !stream.contains(TOKEN),
        "the credential leaked into the agent stream: {stream}"
    );
}

// ---------------------------------------------------------------------------
// OIDC authorization code + PKCE — the mode where a human signs in
// ---------------------------------------------------------------------------

/// A mock identity provider that advertises the browser flow.
async fn browser_idp() -> MockServer {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/realms/mire/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": server.uri(),
            "authorization_endpoint": format!("{}/realms/mire/auth", server.uri()),
            "token_endpoint": format!("{}/realms/mire/token", server.uri()),
        })))
        .mount(&server)
        .await;

    server
}

/// An unsigned `id_token`. `mire` reads it for a display name and nothing else,
/// which is exactly why an unsigned one is enough to test with.
fn id_token(username: &str) -> String {
    use base64::Engine;
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(format!(r#"{{"preferred_username":"{username}"}}"#));
    format!("header.{payload}.signature")
}

/// Mounts one answer to a token exchange of the given grant.
async fn token_answer(idp: &MockServer, grant: &str, access: &str, expires_in: u64, refresh: bool) {
    let mut body = json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": expires_in,
        "id_token": id_token("gleroy"),
        "scope": "openid profile",
    });
    if refresh {
        body["refresh_token"] = json!("the-refresh-token");
    }

    Mock::given(method("POST"))
        .and(path("/realms/mire/token"))
        .and(body_string_contains(format!("grant_type={grant}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .up_to_n_times(1)
        .mount(idp)
        .await;
}

fn browser_registry(idp_uri: &str) -> String {
    format!(
        "providers:\n  \
         - name: me\n    \
         kind: oidc_browser\n    \
         issuer: {idp_uri}/realms/mire\n    \
         client_id: mire-ui\n    \
         scope:\n      \
         - profile\n"
    )
}

/// Signs in end to end and returns the harness plus the resolved callback.
async fn signed_in(harness: &Harness, redirect_uri: &str) -> Value {
    let (status, login) = harness
        .post("/api/auth/me/login", json!({"redirectUri": redirect_uri}))
        .await;
    assert_eq!(status, 200, "{login}");

    let state = login["state"].as_str().expect("state");
    let (status, page) = harness
        .get_text(&format!("/auth/callback?code=the-code&state={state}"))
        .await;
    assert_eq!(status, 200);
    assert!(page.contains("Signed in"), "{page}");
    login
}

#[tokio::test]
async fn a_browser_login_hands_back_a_pkce_authorization_url() {
    let idp = browser_idp().await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    let (status, body) = harness
        .post(
            "/api/auth/me/login",
            json!({"redirectUri": "http://127.0.0.1:8787/auth/callback"}),
        )
        .await;

    assert_eq!(status, 200, "{body}");
    let url = url::Url::parse(body["authorizationUrl"].as_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().collect();

    assert_eq!(query["response_type"], "code");
    assert_eq!(query["client_id"], "mire-ui");
    assert_eq!(query["code_challenge_method"], "S256");
    // `openid` is added even though the profile only asked for `profile`.
    assert_eq!(query["scope"], "openid profile");
    assert_eq!(query["state"], body["state"].as_str().unwrap());
    // The challenge is a hash, so the verifier itself never leaves the process.
    assert_eq!(query["code_challenge"].len(), 43);
}

#[tokio::test]
async fn the_callback_follows_the_browser_rather_than_the_socket() {
    let idp = browser_idp().await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    // What a Kubeflow notebook looks like: the process binds an ephemeral
    // loopback port, the browser is somewhere else entirely.
    let public = "https://kubeflow.example/notebook/team/gleroy/proxy/8787/auth/callback";
    let (status, body) = harness
        .post("/api/auth/me/login", json!({"redirectUri": public}))
        .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["redirectUri"], public);

    let url = url::Url::parse(body["authorizationUrl"].as_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(query["redirect_uri"], public);
}

#[tokio::test]
async fn without_a_supplied_callback_the_request_headers_are_used() {
    let idp = browser_idp().await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    let (status, body) = harness.post("/api/auth/me/login", json!({})).await;

    assert_eq!(status, 200, "{body}");
    let resolved = body["redirectUri"].as_str().unwrap();
    assert!(resolved.starts_with("http://127.0.0.1:"), "{resolved}");
    assert!(resolved.ends_with("/auth/callback"), "{resolved}");
}

#[tokio::test]
async fn a_callback_that_is_not_a_callback_is_refused() {
    let idp = browser_idp().await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    for bad in [
        "javascript:alert(1)/auth/callback",
        "https://elsewhere.example/not-the-callback",
        "not a url at all",
    ] {
        let (status, body) = harness
            .post("/api/auth/me/login", json!({"redirectUri": bad}))
            .await;
        assert_eq!(status, 400, "{bad} should be refused: {body}");
        assert_eq!(body["code"], "bad_redirect_uri");
    }
}

#[tokio::test]
async fn the_callback_trades_the_code_with_the_verifier_it_started_with() {
    let idp = browser_idp().await;
    token_answer(&idp, "authorization_code", "the-access-token", 300, true).await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    let public = "https://kubeflow.example/notebook/team/gleroy/proxy/8787/auth/callback";
    signed_in(&harness, public).await;

    let exchange = idp
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|request| request.url.path().ends_with("/token"))
        .expect("a token exchange");
    let form = String::from_utf8(exchange.body.clone()).unwrap();

    assert!(form.contains("grant_type=authorization_code"), "{form}");
    assert!(form.contains("code=the-code"), "{form}");
    assert!(form.contains("code_verifier="), "{form}");
    // RFC 6749 wants the same redirect_uri as the authorization request, and it
    // is the browser's URL that has to be repeated — not ours.
    assert!(form.contains("kubeflow.example"), "{form}");

    // And the UI can now see who is signed in, without a token in sight.
    let listing = harness.get("/api/auth").await;
    let provider = listing["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "me")
        .unwrap();
    assert_eq!(provider["kind"], "oidc_browser");
    assert!(provider["needsLogin"].as_bool().unwrap());
    assert_eq!(provider["session"]["subject"], "gleroy");
    assert_eq!(provider["session"]["scope"], "openid profile");
    assert!(provider["session"]["canRefresh"].as_bool().unwrap());
}

#[tokio::test]
async fn a_state_cannot_be_replayed() {
    let idp = browser_idp().await;
    token_answer(&idp, "authorization_code", "the-access-token", 300, true).await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    let login = signed_in(&harness, "http://127.0.0.1:8787/auth/callback").await;
    let state = login["state"].as_str().unwrap();

    let (status, page) = harness
        .get_text(&format!("/auth/callback?code=another-code&state={state}"))
        .await;
    assert_eq!(status, 200);
    assert!(page.contains("Sign-in failed"), "{page}");
    assert!(page.contains("already completed"), "{page}");
}

#[tokio::test]
async fn a_callback_with_no_matching_login_is_refused() {
    let idp = browser_idp().await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    let (status, page) = harness
        .get_text("/auth/callback?code=c&state=never-issued")
        .await;

    assert_eq!(status, 200);
    assert!(page.contains("Sign-in failed"), "{page}");
    // Nothing was exchanged: an unrecognised state stops before the token endpoint.
    assert!(idp.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_refusal_from_the_identity_provider_is_reported_in_its_own_words() {
    let idp = browser_idp().await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    let (_, login) = harness
        .post(
            "/api/auth/me/login",
            json!({"redirectUri": "http://127.0.0.1:8787/auth/callback"}),
        )
        .await;
    let state = login["state"].as_str().unwrap();

    let (status, page) = harness
        .get_text(&format!(
            "/auth/callback?error=access_denied&error_description=User%20said%20no&state={state}"
        ))
        .await;

    assert_eq!(status, 200);
    assert!(page.contains("Sign-in failed"), "{page}");
    assert!(page.contains("access_denied"), "{page}");
    assert!(page.contains("User said no"), "{page}");
}

#[tokio::test]
async fn calling_before_signing_in_says_so_rather_than_failing_obscurely() {
    let idp = browser_idp().await;
    let harness = Harness::start(&[
        ("auth.yaml", browser_registry(&idp.uri())),
        (
            "chat.yaml",
            openai_profile("https://models.internal/v1/chat/completions"),
        ),
    ])
    .await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "me", "prompt": "ping"}))
        .await;

    assert_eq!(status, 409);
    assert_eq!(body["code"], "not_signed_in");
    assert!(body["message"].as_str().unwrap().contains("sign in"));
}

#[tokio::test]
async fn the_session_token_is_what_authenticates_the_call() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer the-access-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "pong"}, "finish_reason": "stop"}],
        })))
        .mount(&endpoint)
        .await;

    let idp = browser_idp().await;
    token_answer(&idp, "authorization_code", "the-access-token", 300, true).await;

    let harness = Harness::start(&[
        ("auth.yaml", browser_registry(&idp.uri())),
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    signed_in(&harness, "http://127.0.0.1:8787/auth/callback").await;

    let (status, text, body) = harness
        .call(json!({"profile": "chat", "auth": "me", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 200);
    assert_eq!(body["response"]["decoded"]["content"], "pong");
    assert!(
        !text.contains("the-access-token"),
        "the session token must never come back out"
    );
}

#[tokio::test]
async fn an_expired_session_refreshes_without_another_trip_through_the_browser() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer the-second-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "pong"}, "finish_reason": "stop"}],
        })))
        .mount(&endpoint)
        .await;

    let idp = browser_idp().await;
    // Expires on arrival, but with a refresh token — the case that must not send
    // anyone back to the identity provider.
    token_answer(&idp, "authorization_code", "the-first-token", 0, true).await;
    token_answer(&idp, "refresh_token", "the-second-token", 300, true).await;

    let harness = Harness::start(&[
        ("auth.yaml", browser_registry(&idp.uri())),
        (
            "chat.yaml",
            openai_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    signed_in(&harness, "http://127.0.0.1:8787/auth/callback").await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "me", "prompt": "ping"}))
        .await;

    assert_eq!(status, 200, "{body}");
    assert_eq!(body["response"]["http"]["status"], 200);

    let refreshes = idp
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|request| {
            String::from_utf8_lossy(&request.body).contains("grant_type=refresh_token")
        })
        .count();
    assert_eq!(refreshes, 1, "exactly one silent refresh");
}

#[tokio::test]
async fn a_dead_refresh_token_ends_the_session_instead_of_failing_forever() {
    let idp = browser_idp().await;
    token_answer(&idp, "authorization_code", "the-first-token", 0, true).await;
    Mock::given(method("POST"))
        .and(path("/realms/mire/token"))
        .and(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Session not active",
        })))
        .mount(&idp)
        .await;

    let harness = Harness::start(&[
        ("auth.yaml", browser_registry(&idp.uri())),
        (
            "chat.yaml",
            openai_profile("https://models.internal/v1/chat/completions"),
        ),
    ])
    .await;

    signed_in(&harness, "http://127.0.0.1:8787/auth/callback").await;

    let (status, _, body) = harness
        .call(json!({"profile": "chat", "auth": "me", "prompt": "ping"}))
        .await;
    assert_eq!(status, 502);
    assert!(body["message"].as_str().unwrap().contains("invalid_grant"));

    // And the UI is told the session is gone, so it offers the button again.
    let listing = harness.get("/api/auth").await;
    let provider = listing["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "me")
        .unwrap();
    assert!(provider.get("session").is_none(), "{provider}");
}

#[tokio::test]
async fn signing_out_forgets_the_session_and_says_whether_there_was_one() {
    let idp = browser_idp().await;
    token_answer(&idp, "authorization_code", "the-access-token", 300, true).await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    signed_in(&harness, "http://127.0.0.1:8787/auth/callback").await;

    let (status, body) = harness.post("/api/auth/me/logout", json!({})).await;
    assert_eq!(status, 200);
    assert!(body["signedOut"].as_bool().unwrap());

    let (status, body) = harness.post("/api/auth/me/logout", json!({})).await;
    assert_eq!(status, 200);
    assert!(!body["signedOut"].as_bool().unwrap());
}

#[tokio::test]
async fn signing_in_is_only_offered_where_it_means_something() {
    let idp = browser_idp().await;
    let registry = format!(
        "{}  - name: static\n    kind: token\n    value:\n      env: MODEL_TOKEN\n",
        browser_registry(&idp.uri())
    );
    let harness = Harness::start(&[("auth.yaml", registry)]).await;

    let (status, body) = harness.post("/api/auth/static/login", json!({})).await;
    assert_eq!(status, 422);
    assert_eq!(body["code"], "not_a_browser_provider");

    let (status, body) = harness.post("/api/auth/nope/login", json!({})).await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "unknown_auth_provider");
}

#[tokio::test]
async fn no_token_reaches_the_api_or_the_page_the_browser_lands_on() {
    let idp = browser_idp().await;
    token_answer(&idp, "authorization_code", "SUPERSECRETACCESS", 300, true).await;
    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    let (_, login) = harness
        .post(
            "/api/auth/me/login",
            json!({"redirectUri": "http://127.0.0.1:8787/auth/callback"}),
        )
        .await;

    // The verifier is a credential too: it must not travel back to the client.
    let login_text = login.to_string();
    assert!(!login_text.contains("code_verifier"), "{login_text}");

    let state = login["state"].as_str().unwrap();
    let (_, page) = harness
        .get_text(&format!("/auth/callback?code=the-code&state={state}"))
        .await;
    assert!(!page.contains("SUPERSECRETACCESS"), "{page}");
    assert!(!page.contains("the-refresh-token"), "{page}");

    let listing = harness.get("/api/auth").await.to_string();
    assert!(!listing.contains("SUPERSECRETACCESS"), "{listing}");
    assert!(!listing.contains("the-refresh-token"), "{listing}");
}

#[tokio::test]
async fn an_identity_provider_without_a_browser_flow_says_so() {
    let server = MockServer::start().await;
    // A machine-to-machine-only document: token endpoint, no authorization one.
    Mock::given(method("GET"))
        .and(path("/realms/mire/.well-known/openid-configuration"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "issuer": server.uri(),
            "token_endpoint": format!("{}/realms/mire/token", server.uri()),
        })))
        .mount(&server)
        .await;

    let harness = Harness::start(&[("auth.yaml", browser_registry(&server.uri()))]).await;

    let (status, body) = harness.post("/api/auth/me/login", json!({})).await;

    assert_eq!(status, 502);
    assert_eq!(body["code"], "oidc_discovery_failed");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("no `authorization_endpoint`"),
        "{body}"
    );
}

#[tokio::test]
async fn a_failed_login_leaves_its_reason_where_the_panel_can_find_it() {
    let idp = browser_idp().await;
    // The exchange fails, which is the case whose only account was a page that
    // closed itself after a second.
    Mock::given(method("POST"))
        .and(path("/realms/mire/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "error": "invalid_grant",
            "error_description": "Code not valid",
        })))
        .mount(&idp)
        .await;

    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;

    let (_, login) = harness
        .post(
            "/api/auth/me/login",
            json!({"redirectUri": "http://127.0.0.1:8787/auth/callback"}),
        )
        .await;
    let state = login["state"].as_str().unwrap();

    let (status, page) = harness
        .get_text(&format!("/auth/callback?code=stale&state={state}"))
        .await;
    assert_eq!(status, 200);
    assert!(page.contains("Sign-in failed"), "{page}");
    // The page has to stay: it is the only place the reason is written down.
    assert!(
        !page.contains("window.close"),
        "a failure page must not close itself: {page}"
    );

    // And the panel gets the same sentence without needing that tab at all.
    let listing = harness.get("/api/auth").await;
    let provider = listing["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "me")
        .unwrap();
    assert!(
        provider["lastError"]
            .as_str()
            .unwrap()
            .contains("invalid_grant"),
        "{provider}"
    );
}

#[tokio::test]
async fn a_new_attempt_clears_the_previous_complaint() {
    let idp = browser_idp().await;
    Mock::given(method("POST"))
        .and(path("/realms/mire/token"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({"error": "invalid_grant"})))
        .up_to_n_times(1)
        .mount(&idp)
        .await;
    token_answer(&idp, "authorization_code", "the-access-token", 300, true).await;

    let harness = Harness::start(&[("auth.yaml", browser_registry(&idp.uri()))]).await;
    let callback = "http://127.0.0.1:8787/auth/callback";

    let (_, first) = harness
        .post("/api/auth/me/login", json!({"redirectUri": callback}))
        .await;
    harness
        .get_text(&format!(
            "/auth/callback?code=stale&state={}",
            first["state"].as_str().unwrap()
        ))
        .await;

    let failed = harness.get("/api/auth").await;
    assert!(failed["providers"].to_string().contains("lastError"));

    // Starting again must not leave the old failure sitting under the button.
    let (_, second) = harness
        .post(
            "/api/auth/me/login",
            json!({"redirectUri": callback, "prompt": "login"}),
        )
        .await;
    let cleared = harness.get("/api/auth").await;
    assert!(
        !cleared["providers"].to_string().contains("lastError"),
        "{cleared}"
    );

    // `prompt` reaches the identity provider, which is how a stuck login is
    // forced to stop reusing its own session.
    let url = url::Url::parse(second["authorizationUrl"].as_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(query["prompt"], "login");

    harness
        .get_text(&format!(
            "/auth/callback?code=good&state={}",
            second["state"].as_str().unwrap()
        ))
        .await;

    let listing = harness.get("/api/auth").await;
    let provider = listing["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["name"] == "me")
        .unwrap();
    assert_eq!(provider["session"]["subject"], "gleroy");
    assert!(provider.get("lastError").is_none(), "{provider}");
}

// ---------------------------------------------------------------------------
// MCP — tools the agent really calls
// ---------------------------------------------------------------------------

/// A mock MCP server speaking revision 2026-07-28.
///
/// `answers` is one JSON-RPC result per `tools/call`, served in order.
async fn mcp_server(tools: Value, answers: Vec<Value>) -> MockServer {
    let server = MockServer::start().await;

    // A server on the newest revision answers discovery, so negotiation settles
    // in one round trip and every test below exercises the same path a real
    // `2026-07-28` server puts the client through.
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("mcp-method", "server/discover"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "resultType": "complete",
                "protocolVersions": ["2026-07-28"],
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "mock", "version": "0.1.0"},
            },
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("mcp-method", "tools/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"resultType": "complete", "tools": tools, "ttlMs": 60000, "cacheScope": "public"},
        })))
        .mount(&server)
        .await;

    for answer in answers {
        Mock::given(method("POST"))
            .and(path("/mcp"))
            .and(header("mcp-method", "tools/call"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0", "id": 1, "result": answer,
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
    }

    server
}

fn weather_tool() -> Value {
    json!([{
        "name": "get_weather",
        "description": "Current weather for a city",
        "inputSchema": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
        },
        "annotations": {"readOnlyHint": true},
    }])
}

/// A chat profile whose tools come from an MCP server rather than its own YAML.
///
/// It says nothing about MCP, and does not have to: every server `mcp.yaml`
/// declares is offered to every `kind: chat` profile.
fn mcp_profile(url: &str) -> String {
    format!(
        r#"
name: chat
kind: chat
url: {url}
timeout_ms: 5000
request:
  template: |
    {{"model": "m", "messages": {{{{ messages | tojson }}}}, "tools": {{{{ tools | tojson }}}}}}
decode:
  content: ["$.choices[0].message.content"]
  tool_calls: ["$.choices[0].message.tool_calls"]
  finish_reason: ["$.choices[0].finish_reason"]
agent:
  stop_when:
    no_tool_calls: true
  max_iterations: 4
"#
    )
}

/// A model that asks for `get_weather` once, then answers.
async fn model_using_a_tool(endpoint: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"content": "", "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "get_weather", "arguments": "{\"city\": \"Paris\"}"},
                }]},
                "finish_reason": "tool_calls",
            }],
        })))
        .up_to_n_times(1)
        .mount(endpoint)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"content": "It is 21 degrees and clear in Paris."},
                "finish_reason": "stop",
            }],
        })))
        .mount(endpoint)
        .await;
}

#[tokio::test]
async fn the_agent_really_calls_an_mcp_tool_and_feeds_the_result_back() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({
            "resultType": "complete",
            "content": [{"type": "text", "text": "{\"temp\": 21, \"conditions\": \"clear\"}"}],
            "isError": false,
        })],
    )
    .await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let turns: Vec<_> = events.iter().filter(|(name, _)| name == "turn").collect();
    assert_eq!(turns.len(), 2, "{events:?}");

    // Turn one: the model asked, and the *server* answered — not a stub.
    let tool = &turns[0].1["tools"][0];
    assert_eq!(tool["call"]["name"], "get_weather");
    assert_eq!(tool["source"], "mcp");
    assert_eq!(tool["server"], "weather");
    assert!(tool["schemaErrors"].as_array().unwrap().is_empty());
    assert!(tool["result"].as_str().unwrap().contains("21"));
    assert!(!tool["reportedError"].as_bool().unwrap());
    assert!(tool["latencyMs"].is_number());
    // What the round trip under it answered, said where the tool call is read
    // rather than only on the protocol exchange beside it.
    assert_eq!(tool["status"], 200);

    let done = events.iter().find(|(name, _)| name == "done").unwrap();
    assert_eq!(done.1["stop"]["outcome"], "stopped");

    // And the tool really was declared to the model, from the live listing.
    let sent: Value =
        serde_json::from_str(turns[0].1["call"]["request"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(sent["tools"][0]["function"]["name"], "get_weather");
    assert_eq!(
        sent["tools"][0]["function"]["description"],
        "Current weather for a city"
    );
}

/// One turn sets nothing up, whatever `mcp.yaml` declares.
///
/// The loop is what discovers a server, lists its tools and calls them; a single
/// call has no second turn to feed a result into, so it opens no connection at
/// all. A server is declared here and both single-turn endpoints are asked to run
/// the profile — a spent credential, a session prompt, or a line in somebody's
/// audit log for a tool that was never going to be called is a side effect nobody
/// asked this endpoint for.
#[tokio::test]
async fn a_single_turn_never_speaks_to_an_mcp_server() {
    let mcp = mcp_server(weather_tool(), vec![]).await;
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "pong"}, "finish_reason": "stop"}],
        })))
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, text, body) = harness
        .call(json!({"profile": "chat", "prompt": "ping"}))
        .await;
    assert_eq!(status, 200, "{text}");

    // And the model was offered nothing either: the tool list a live server
    // would have filled is empty, rather than filled from a listing nobody made.
    let sent: Value = serde_json::from_str(body["request"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(sent["tools"], json!([]));

    let (status, events) = harness
        .stream(json!({"profile": "chat", "prompt": "ping"}))
        .await;
    assert_eq!(status, 200);
    assert!(events.iter().any(|(name, _)| name == "done"), "{events:?}");

    // Not one word, on either route: no discovery, no listing, no tool call.
    assert!(
        mcp.received_requests().await.unwrap_or_default().is_empty(),
        "a single turn spoke to an MCP server"
    );
}

/// A stock tool, so a second server has something of its own to offer.
fn stock_tool() -> Value {
    json!([{
        "name": "get_stock",
        "description": "Last price for a ticker",
        "inputSchema": {
            "type": "object",
            "properties": {"ticker": {"type": "string"}},
            "required": ["ticker"],
        },
    }])
}

/// A model that answers on turn one, asking for nothing.
async fn model_answering_plainly(endpoint: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"content": "pong"}, "finish_reason": "stop"}],
        })))
        .mount(endpoint)
        .await;
}

/// The names offered to the model on turn one, in the request that went out.
fn tools_offered(turn: &Value) -> Vec<String> {
    let sent: Value = serde_json::from_str(turn["call"]["request"]["body"].as_str().unwrap())
        .expect("the rendered body");
    sent["tools"]
        .as_array()
        .expect("the tools array")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().unwrap_or_default().into())
        .collect()
}

/// One run reaching fewer servers than `mcp.yaml` declares.
///
/// The file still declares both — declaring a server is the opt-in, and no
/// request edits it. What a run gets to say is which of them this one reaches, so
/// that "does the model still get there without the weather tool?" and "is that
/// server what has been failing for ten minutes?" stop being a config edit, a
/// restart and a config edit back.
///
/// The one left out is not idle: it is never discovered, never listed, and its
/// tools never reach the model.
#[tokio::test]
async fn a_run_reaches_only_the_servers_it_names() {
    let weather = mcp_server(weather_tool(), vec![]).await;
    let stocks = mcp_server(stock_tool(), vec![]).await;
    let endpoint = MockServer::start().await;
    model_answering_plainly(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n  - name: stocks\n    url: {}/mcp\n",
                weather.uri(),
                stocks.uri()
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "ping", "mcpServers": ["stocks"]}))
        .await;
    assert_eq!(status, 200, "{events:?}");

    let turns: Vec<_> = events.iter().filter(|(name, _)| name == "turn").collect();
    assert_eq!(tools_offered(&turns[0].1), vec!["get_stock".to_owned()]);

    assert!(
        !stocks
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "the server this run asked for was never spoken to"
    );
    assert!(
        weather
            .received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a server left out of the run was spoken to anyway"
    );
}

/// Every server off, which is a list of none rather than a silence.
///
/// The loop still runs — this is the question "what does it do when the tool is
/// not there?", and the answer is the model's, on the profile's own simulated
/// `tools:` and nothing else.
#[tokio::test]
async fn a_run_can_name_no_server_at_all() {
    let mcp = mcp_server(weather_tool(), vec![]).await;
    let endpoint = MockServer::start().await;
    model_answering_plainly(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "ping", "mcpServers": []}))
        .await;
    assert_eq!(status, 200, "{events:?}");

    // No setup event either: there was nothing to set up, and an empty one would
    // claim a listing that never happened.
    assert!(
        !events.iter().any(|(name, _)| name == "setup"),
        "{events:?}"
    );
    let turns: Vec<_> = events.iter().filter(|(name, _)| name == "turn").collect();
    assert!(tools_offered(&turns[0].1).is_empty());
    assert!(
        mcp.received_requests().await.unwrap_or_default().is_empty(),
        "a run that named no server spoke to one"
    );
}

/// Saying nothing reaches every declared server, on a profile that mentions none.
///
/// `mcp.yaml` is the opt-in and the only one: a server declared there is offered
/// to every `kind: chat` profile, so a profile that says nothing about MCP still
/// gets the lot. This is the default the two tests above narrow away from.
#[tokio::test]
async fn a_run_that_names_no_server_reaches_every_declared_one() {
    let weather = mcp_server(weather_tool(), vec![]).await;
    let stocks = mcp_server(stock_tool(), vec![]).await;
    let endpoint = MockServer::start().await;
    model_answering_plainly(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n  - name: stocks\n    url: {}/mcp\n",
                weather.uri(),
                stocks.uri()
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "ping"}))
        .await;
    assert_eq!(status, 200, "{events:?}");

    let turns: Vec<_> = events.iter().filter(|(name, _)| name == "turn").collect();
    // The registry's order, which is alphabetical, not the file's.
    assert_eq!(
        tools_offered(&turns[0].1),
        vec!["get_stock".to_owned(), "get_weather".to_owned()],
        "a profile naming no server should still be offered both"
    );
}

/// A name `mcp.yaml` does not declare is a typo, and gets a status code.
///
/// The request no longer has a per-profile list to overstep, so the only way to
/// get this wrong is to name something that does not exist — which is a `404`
/// before anything is sent, rather than a stream that opens and fails.
#[tokio::test]
async fn a_run_cannot_reach_a_server_nobody_declared() {
    let mcp = mcp_server(weather_tool(), vec![]).await;
    let endpoint = MockServer::start().await;
    model_answering_plainly(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, body) = harness
        .post(
            "/api/agent",
            json!({"profile": "chat", "prompt": "ping", "mcpServers": ["stocks"]}),
        )
        .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["code"], "unknown_mcp_server");
    assert!(body["message"].as_str().unwrap().contains("stocks"));

    assert!(
        mcp.received_requests().await.unwrap_or_default().is_empty(),
        "a run refused before it started spoke to a server anyway"
    );
}

#[tokio::test]
async fn the_required_headers_are_mirrored_from_the_body() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "ok"}]})],
    )
    .await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;

    let call = mcp
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|request| {
            request
                .headers
                .get("mcp-method")
                .is_some_and(|v| v == "tools/call")
        })
        .expect("a tools/call");

    // The revision requires these, and rejects a request whose headers and body
    // disagree — so they are derived from what is actually sent.
    assert_eq!(call.headers["mcp-protocol-version"], "2026-07-28");
    assert_eq!(call.headers["mcp-name"], "get_weather");
    assert!(
        call.headers["accept"]
            .to_str()
            .unwrap()
            .contains("text/event-stream"),
        "both answer shapes have to be acceptable"
    );

    let body: Value = serde_json::from_slice(&call.body).unwrap();
    assert_eq!(body["params"]["name"], "get_weather");
    assert_eq!(body["params"]["arguments"]["city"], "Paris");
    // No handshake happened, so every request carries its own version.
    assert_eq!(
        body["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"],
        "2026-07-28"
    );
    assert_eq!(
        body["params"]["_meta"]["io.modelcontextprotocol/clientInfo"]["name"],
        "mire"
    );
}

#[tokio::test]
async fn every_word_said_to_an_mcp_server_is_reported_not_just_the_tool_calls() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "21"}]})],
    )
    .await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;
    assert_eq!(status, 200);

    // Listing the tools happens before the first prompt is spent, so it arrives
    // before the first turn rather than being folded into it. A run that dies
    // negotiating has no turns at all, and this is then the whole story.
    let (name, setup) = events.first().expect("an event");
    assert_eq!(name, "setup");
    let listing = setup["mcp"].as_array().expect("the setup exchanges");
    let listed = listing
        .iter()
        .find(|exchange| exchange["method"] == "tools/list")
        .expect("a `tools/list`");
    assert_eq!(listed["server"], "weather");
    assert_eq!(listed["status"], 200);
    // Both halves, which is the point: what was asked and what came back.
    assert!(listed["request"].as_str().unwrap().contains("tools/list"));
    assert!(listed["response"].as_str().unwrap().contains("get_weather"));
    assert_eq!(listed["revision"], "2026-07-28");

    // And the tool call itself rides on the turn that made it.
    let turn = events
        .iter()
        .find(|(name, _)| name == "turn")
        .map(|(_, payload)| payload)
        .expect("a turn");
    let called = turn["mcp"]
        .as_array()
        .expect("the turn's exchanges")
        .iter()
        .find(|exchange| exchange["method"] == "tools/call")
        .expect("a `tools/call`");
    assert!(called["request"].as_str().unwrap().contains("Paris"));
    assert!(called["response"].as_str().unwrap().contains("21"));

    // The trace carries the setup too, so a client that missed the event still
    // has it.
    let (_, done) = events
        .iter()
        .find(|(name, _)| name == "done")
        .expect("done");
    assert!(!done["setup"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_credential_sent_to_an_mcp_server_never_comes_back_in_the_journal() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "21"}]})],
    )
    .await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    // `PATH` stands in for a credential variable, as elsewhere in this file: the
    // test cannot set one, and what matters is that whatever went out does not
    // come back.
    let secret = std::env::var("PATH").expect("PATH");

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    headers:\n      \
                 x-api-key: 'k-{{{{ env.PATH }}}}'\n",
                mcp.uri()
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;
    assert_eq!(status, 200);

    // The whole stream, not just the exchange that carried it: a transcript is
    // durable, and a credential printed once is a credential to rotate.
    let whole = serde_json::to_string(&events).expect("serialise");
    assert!(
        !whole.contains(&secret),
        "the journal must never echo a credential back"
    );
    assert!(
        whole.contains("x-api-key"),
        "the header itself is still reported — only its value is not"
    );
}

// ---------------------------------------------------------------------------
// MCP — hooks around a tool call
// ---------------------------------------------------------------------------

/// A hook endpoint that answers `status` at `/hook`.
async fn hook_endpoint(status: u16, body: &str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/hook"))
        .respond_with(ResponseTemplate::new(status).set_body_string(body))
        .mount(&server)
        .await;
    server
}

/// `mcp.yaml` for a server whose tool call is wrapped in one hook.
///
/// `hook_extra` lands among the hook's own fields — `tools:`, `on_error:` — and
/// `action_extra` among its single action's, so a test adds a `json:` document
/// or a `multipart:` form without repeating the scaffolding.
fn mcp_with_hook(
    mcp: &MockServer,
    hook: &str,
    phases: &[&str],
    hook_extra: &str,
    action_extra: &str,
) -> String {
    let on: String = phases
        .iter()
        .map(|phase| ["          - ", phase, "\n"].concat())
        .collect();
    format!(
        "servers:\n  - name: weather\n    url: {}/mcp\n    hooks:\n      - name: audit\n        \
         on:\n{on}{hook_extra}        actions:\n          - http:\n              url: {hook}/hook\n{action_extra}",
        mcp.uri()
    )
}

#[tokio::test]
async fn a_hook_fires_on_both_sides_of_a_tool_call_and_says_what_it_sent() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({
            "resultType": "complete",
            "content": [{"type": "text", "text": "21 and clear"}],
            "isError": false,
        })],
    )
    .await;
    let hook = hook_endpoint(204, "").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            mcp_with_hook(
                &mcp,
                &hook.uri(),
                &["before", "after"],
                "",
                // The call itself, which is now something a file asks for by
                // name rather than something every hook sends by default.
                "              json: '{{ call }}'\n",
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    // Two firings, in the order the names promise.
    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
    let hooks = turn["hooks"].as_array().expect("the hooks that fired");
    assert_eq!(hooks.len(), 2, "{hooks:#?}");
    assert_eq!(hooks[0]["phase"], "before");
    assert_eq!(hooks[1]["phase"], "after");
    for record in hooks {
        assert_eq!(record["hook"], "audit");
        assert_eq!(record["server"], "weather");
        assert_eq!(record["tool"], "get_weather");
        assert_eq!(record["action"], "http");
        assert_eq!(record["method"], "POST");
        assert_eq!(record["status"], 204);
        assert_eq!(record["stoppedTheCall"], false);
        assert!(record["error"].is_null());
    }

    // And what actually went out: the call itself, arguments included, with the
    // result appearing only on the way back.
    let sent = hook.received_requests().await.expect("hook requests");
    assert_eq!(sent.len(), 2);
    let before: Value = serde_json::from_slice(&sent[0].body).expect("before payload");
    assert_eq!(before["phase"], "before");
    assert_eq!(before["tool"], "get_weather");
    assert_eq!(before["arguments"]["city"], "Paris");
    assert!(before.get("result").is_none());

    let after: Value = serde_json::from_slice(&sent[1].body).expect("after payload");
    assert_eq!(after["phase"], "after");
    assert_eq!(after["result"]["text"], "21 and clear");
    assert_eq!(after["result"]["isError"], false);
    assert!(after["result"]["latencyMs"].is_number());
}

/// A chat profile that reaches a server. It says nothing about capturing —
/// that is the server's to declare.
fn capturing_profile(url: &str) -> String {
    format!(
        r#"
name: chat
kind: chat
url: {url}
timeout_ms: 5000
request:
  template: |
    {{"model": "m", "messages": {{{{ messages | tojson }}}}, "tools": {{{{ tools | tojson }}}}}}
decode:
  content: ["$.choices[0].message.content"]
  tool_calls: ["$.choices[0].message.tool_calls"]
  finish_reason: ["$.choices[0].finish_reason"]
agent:
  stop_when:
    no_tool_calls: true
  max_iterations: 4
"#
    )
}

#[tokio::test]
async fn a_hook_url_is_addressed_with_what_the_tool_call_captured() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({
            "resultType": "complete",
            "content": [{"type": "text", "text": "{\"sessionId\": \"abc-123\", \"temp\": 21}"}],
            "isError": false,
        })],
    )
    .await;

    // Mounted on the path the capture has to produce, so a hook that rendered
    // anything else gets a `404` instead of a quiet pass.
    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/sessions/abc-123/audit"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&hook)
        .await;

    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    capture:\n      - tools: \
                 [get_weather]\n        vars:\n          session: [$.sessionId]\n    hooks:\n      \
                 - name: audit\n        on:\n          - after\n        actions:\n          \
                 - http:\n              url: {}/sessions/{{{{ vars.session }}}}/audit\n",
                mcp.uri(),
                hook.uri()
            ),
        ),
        (
            "chat.yaml",
            capturing_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;

    // The tool call says what it captured.
    let tool = &turn["tools"][0];
    assert_eq!(tool["source"], "mcp");
    assert_eq!(tool["captured"]["session"], "abc-123");

    // And the `after` hook of that very call reached the address it produced —
    // which is only possible because the capture happens before it fires.
    let hooks = turn["hooks"].as_array().expect("the hooks that fired");
    assert_eq!(hooks.len(), 1, "{hooks:#?}");
    assert_eq!(hooks[0]["phase"], "after");
    assert_eq!(hooks[0]["status"], 204, "{:#?}", hooks[0]);
    assert!(
        hooks[0]["url"]
            .as_str()
            .expect("the url it used")
            .ends_with("/sessions/abc-123/audit"),
        "{:#?}",
        hooks[0]
    );

    let sent = hook.received_requests().await.expect("hook requests");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].url.path(), "/sessions/abc-123/audit");
}

/// A second chat profile pointed at its own endpoint, capturing nothing of its
/// own — like every profile now.
fn plain_chat_profile(name: &str, url: &str) -> String {
    format!(
        r#"
name: {name}
kind: chat
url: {url}
timeout_ms: 5000
request:
  template: |
    {{"model": "m", "messages": {{{{ messages | tojson }}}}, "tools": {{{{ tools | tojson }}}}}}
decode:
  content: ["$.choices[0].message.content"]
  tool_calls: ["$.choices[0].message.tool_calls"]
  finish_reason: ["$.choices[0].finish_reason"]
agent:
  stop_when:
    no_tool_calls: true
  max_iterations: 4
"#
    )
}

/// The whole point of putting `capture:` on the server: the rule is written
/// once, and comparing two models is not comparing two copies of it that have
/// to stay identical by hand.
#[tokio::test]
async fn two_profiles_reaching_one_server_both_capture_what_it_declares() {
    let answer = json!({
        "resultType": "complete",
        "content": [{"type": "text", "text": "{\"sessionId\": \"abc-123\", \"temp\": 21}"}],
        "isError": false,
    });
    let mcp = mcp_server(weather_tool(), vec![answer.clone(), answer]).await;

    // One endpoint each: the mock that asks for a tool only answers once, and
    // both runs have to get that far.
    let first = MockServer::start().await;
    let second = MockServer::start().await;
    model_using_a_tool(&first).await;
    model_using_a_tool(&second).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    capture:\n      - tools: \
                 [get_weather]\n        vars:\n          session: [$.sessionId]\n",
                mcp.uri()
            ),
        ),
        (
            "one.yaml",
            plain_chat_profile("one", &format!("{}/v1/chat/completions", first.uri())),
        ),
        (
            "two.yaml",
            plain_chat_profile("two", &format!("{}/v1/chat/completions", second.uri())),
        ),
    ])
    .await;

    for profile in ["one", "two"] {
        let (status, events) = harness
            .agent(json!({"profile": profile, "prompt": "weather in Paris?"}))
            .await;
        assert_eq!(status, 200, "{profile}: {events:?}");

        let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
        assert_eq!(
            turn["tools"][0]["captured"]["session"], "abc-123",
            "{profile} captured nothing: {turn:#?}"
        );
    }
}

/// A rule naming a variable no template could write is refused where it was
/// written, with the rest of `mcp.yaml` still loading around it.
#[tokio::test]
async fn a_broken_capture_rule_is_reported_with_the_server_that_declared_it() {
    let harness = Harness::start(&[(
        "mcp.yaml",
        "servers:\n  - name: broken\n    url: https://mcp.internal/mcp\n    capture:\n      \
         - vars:\n          'my id': [$.id]\n  - name: fine\n    url: https://other.internal/mcp\n"
            .to_owned(),
    )])
    .await;

    let body = harness.get("/api/mcp").await;

    let issues = body["issues"].as_array().expect("the issues");
    assert_eq!(issues.len(), 1, "{issues:#?}");
    assert!(
        issues[0]["file"]
            .as_str()
            .expect("the file")
            .ends_with("mcp.yaml"),
        "{issues:#?}"
    );
    let message = issues[0]["message"].as_str().expect("the reason");
    assert!(message.contains("my id"), "{message}");
    assert!(message.contains("broken"), "{message}");
    // The sentence, without validator's `__all__:` in front of it — that is the
    // internal name for "not about one field", and nobody wrote it in mcp.yaml.
    assert!(!message.contains("__all__"), "{message}");

    // Only that server was dropped.
    let servers = body["servers"].as_array().expect("the servers");
    assert_eq!(servers.len(), 1, "{servers:#?}");
    assert_eq!(servers[0]["name"], "fine");
}

/// What a server captures is part of "what is this about to do": a hook's
/// templated URL a few lines above is unreadable without knowing where
/// `vars.session` comes from. Names only — a captured value is a session id as
/// often as not.
#[tokio::test]
async fn a_server_lists_what_it_captures_without_listing_a_single_value() {
    let harness = Harness::start(&[(
        "mcp.yaml",
        "servers:\n  - name: weather\n    url: https://mcp.internal/mcp\n    capture:\n      \
         - tools: [get_weather]\n        vars:\n          session: [$.sessionId, $.session.id]\n"
            .to_owned(),
    )])
    .await;

    let body = harness.get("/api/mcp").await;

    let capture = &body["servers"][0]["capture"];
    assert_eq!(capture[0]["tools"][0], "get_weather");
    assert_eq!(capture[0]["vars"][0], "session");
    // The paths are how it finds the value, and the value is the thing not to
    // put in a listing — neither belongs here.
    assert!(!body.to_string().contains("sessionId"), "{body:#?}");
}

#[tokio::test]
async fn a_hook_url_naming_a_variable_nobody_captured_fails_the_call_rather_than_guessing() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({
            "resultType": "complete",
            // No `sessionId` in it, so the capture has nothing to select.
            "content": [{"type": "text", "text": "{\"temp\": 21}"}],
            "isError": false,
        })],
    )
    .await;
    let hook = MockServer::start().await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    capture:\n      - tools: \
                 [get_weather]\n        vars:\n          session: [$.sessionId]\n    hooks:\n      \
                 - name: audit\n        on:\n          - after\n        actions:\n          \
                 - http:\n              url: {}/sessions/{{{{ vars.session }}}}/audit\n",
                mcp.uri(),
                hook.uri()
            ),
        ),
        (
            "chat.yaml",
            capturing_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
    let hooks = turn["hooks"].as_array().expect("the hooks that fired");
    assert_eq!(hooks.len(), 1, "{hooks:#?}");

    // Loud, and specific about which template and which variable — the loud
    // default, because rendering it away would send the audit to `/sessions//audit`.
    assert_eq!(hooks[0]["stoppedTheCall"], true);
    let error = hooks[0]["error"].as_str().expect("the reason");
    assert!(error.contains("undefined"), "{error}");
    assert!(error.contains("url"), "{error}");

    // Nothing left for it: the request never went out.
    assert!(
        hook.received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a hook that could not address itself must not have sent anything"
    );
}

#[tokio::test]
async fn a_hook_header_carries_what_the_tool_call_captured() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({
            "resultType": "complete",
            "content": [{"type": "text", "text": "{\"sessionId\": \"abc-123\"}"}],
            "isError": false,
        })],
    )
    .await;
    let hook = hook_endpoint(204, "").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    capture:\n      - tools: \
                 [get_weather]\n        vars:\n          session: [$.sessionId]\n    hooks:\n      \
                 - name: audit\n        on:\n          - after\n        actions:\n          \
                 - http:\n              url: {}/hook\n              headers:\n                \
                 x-session: '{{{{ vars.session }}}}'\n",
                mcp.uri(),
                hook.uri()
            ),
        ),
        (
            "chat.yaml",
            capturing_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let sent = hook.received_requests().await.expect("hook requests");
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].headers["x-session"], "abc-123");

    // A hook's headers need no `| default(...)`: it only renders around a call.
    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
    let hooks = turn["hooks"].as_array().expect("the hooks that fired");
    assert_eq!(hooks[0]["status"], 204, "{:#?}", hooks[0]);
    // The name travels; the rendered value is masked like every other header.
    assert!(
        hooks[0]["headers"]["x-session"].is_string(),
        "{:#?}",
        hooks[0]
    );
}

/// A model that asks for `get_weather` twice, then answers.
async fn model_using_a_tool_twice(endpoint: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"content": "", "tool_calls": [{
                    "id": "call_1",
                    "function": {"name": "get_weather", "arguments": "{\"city\": \"Paris\"}"},
                }]},
                "finish_reason": "tool_calls",
            }],
        })))
        .up_to_n_times(2)
        .mount(endpoint)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {"content": "It is 21 degrees and clear in Paris."},
                "finish_reason": "stop",
            }],
        })))
        .mount(endpoint)
        .await;
}

#[tokio::test]
async fn a_server_header_picks_up_a_session_a_tool_opened_on_an_earlier_turn() {
    let mcp = mcp_server(
        weather_tool(),
        vec![
            json!({
                "resultType": "complete",
                "content": [{"type": "text", "text": "{\"sessionId\": \"abc-123\"}"}],
                "isError": false,
            }),
            json!({
                "resultType": "complete",
                "content": [{"type": "text", "text": "{\"sessionId\": \"abc-123\"}"}],
                "isError": false,
            }),
        ],
    )
    .await;
    let endpoint = MockServer::start().await;
    model_using_a_tool_twice(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    capture:\n      - tools: \
                 [get_weather]\n        vars:\n          session: [$.sessionId]\n    headers:\n      \
                 x-session: \"{{{{ vars.session | default('') }}}}\"\n",
                mcp.uri()
            ),
        ),
        (
            "chat.yaml",
            capturing_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, _) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let sent = mcp.received_requests().await.expect("mcp requests");
    let sessions: Vec<&str> = sent
        .iter()
        .map(|request| {
            request
                .headers
                .get("x-session")
                .and_then(|value| value.to_str().ok())
                .unwrap_or("<absent>")
        })
        .collect();

    // Empty on everything up to and including the first `tools/call` — nothing
    // has been captured yet — and carrying the session from the second one on.
    // The `| default('')` is what keeps the setup `tools/list` from dying.
    assert!(
        sessions.first().is_some_and(|first| first.is_empty()),
        "{sessions:?}"
    );
    assert!(
        sessions.contains(&"abc-123"),
        "the session a tool opened never reached a later request: {sessions:?}"
    );
}

#[tokio::test]
async fn a_hook_sits_out_the_calls_its_condition_refuses_then_fires_once_it_holds() {
    let mcp = mcp_server(
        weather_tool(),
        vec![
            // The first call opens nothing: there is no session to audit yet.
            json!({
                "resultType": "complete",
                "content": [{"type": "text", "text": "{\"temp\": 21}"}],
                "isError": false,
            }),
            json!({
                "resultType": "complete",
                "content": [{"type": "text", "text": "{\"sessionId\": \"abc-123\"}"}],
                "isError": false,
            }),
        ],
    )
    .await;

    let hook = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/sessions/abc-123/audit"))
        .respond_with(ResponseTemplate::new(204))
        .mount(&hook)
        .await;

    let endpoint = MockServer::start().await;
    model_using_a_tool_twice(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    capture:\n      - tools: \
                 [get_weather]\n        vars:\n          session: [$.sessionId]\n    hooks:\n      \
                 - name: audit\n        on:\n          - after\n        if: '{{{{ vars.session is \
                 defined }}}}'\n        actions:\n          - http:\n              \
                 url: {}/sessions/{{{{ vars.session }}}}/audit\n",
                mcp.uri(),
                hook.uri()
            ),
        ),
        (
            "chat.yaml",
            capturing_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let turns: Vec<&Value> = events
        .iter()
        .filter(|(name, _)| name == "turn")
        .map(|(_, turn)| turn)
        .collect();
    assert!(turns.len() >= 2, "{turns:#?}");

    // Turn one: nothing captured, so the condition is false and the hook sits it
    // out — recorded, quoting what it was asked, and emphatically not a failure.
    let first = turns[0]["hooks"].as_array().expect("the hooks of turn one");
    assert_eq!(first.len(), 1, "{first:#?}");
    assert_eq!(first[0]["skipped"], "{{ vars.session is defined }}");
    assert_eq!(first[0]["status"], 0);
    assert_eq!(first[0]["stoppedTheCall"], false);
    assert!(first[0]["error"].is_null(), "{:#?}", first[0]);

    // And the tool call it sat out is untouched: the model got the real answer.
    assert!(turns[0]["tools"][0]["error"].is_null(), "{:#?}", turns[0]);
    assert!(
        turns[0]["tools"][0]["captured"]
            .as_object()
            .is_none_or(serde_json::Map::is_empty)
    );

    // Turn two captures a session, so the hook fires — at the address that
    // capture produced.
    let second = turns[1]["hooks"].as_array().expect("the hooks of turn two");
    assert_eq!(second.len(), 1, "{second:#?}");
    assert!(second[0]["skipped"].is_null(), "{:#?}", second[0]);
    assert_eq!(second[0]["status"], 204, "{:#?}", second[0]);
    assert_eq!(turns[1]["tools"][0]["captured"]["session"], "abc-123");

    let sent = hook.received_requests().await.expect("hook requests");
    assert_eq!(
        sent.len(),
        1,
        "the skipped firing must not have sent anything"
    );
    assert_eq!(sent[0].url.path(), "/sessions/abc-123/audit");
}

#[tokio::test]
async fn a_before_hook_that_says_no_stops_the_call_from_happening_at_all() {
    let mcp = mcp_server(weather_tool(), vec![json!({"resultType": "complete"})]).await;
    let hook = hook_endpoint(403, "policy: get_weather is not allowed here").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            mcp_with_hook(&mcp, &hook.uri(), &["before"], "", ""),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    // The model still gets told, because recovering from it is the thing agent
    // mode exists to watch — but the server was never asked.
    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
    let tool = &turn["tools"][0];
    let message = tool["error"].as_str().expect("the hook's refusal");
    assert!(message.contains("audit"), "{message}");
    assert!(message.contains("403"), "{message}");
    assert!(message.contains("not allowed here"), "{message}");

    assert_eq!(turn["hooks"][0]["stoppedTheCall"], true);

    let calls = mcp
        .received_requests()
        .await
        .expect("mcp requests")
        .into_iter()
        .filter(|request| {
            request
                .headers
                .get("mcp-method")
                .is_some_and(|value| value == "tools/call")
        })
        .count();
    assert_eq!(
        calls, 0,
        "a gate that said no must not have let the call out"
    );
}

#[tokio::test]
async fn a_hook_told_to_step_aside_records_its_failure_and_lets_the_tool_answer() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({
            "resultType": "complete",
            "content": [{"type": "text", "text": "21 and clear"}],
        })],
    )
    .await;
    let hook = hook_endpoint(500, "the audit sink is having a day").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            mcp_with_hook(
                &mcp,
                &hook.uri(),
                &["before"],
                "        on_error: continue\n",
                "",
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
    let tool = &turn["tools"][0];
    assert!(tool["error"].is_null(), "{tool:#?}");
    assert!(tool["result"].as_str().unwrap().contains("21 and clear"));

    // Stepped over, not swallowed: the failure is still in the record.
    let record = &turn["hooks"][0];
    assert_eq!(record["status"], 500);
    assert_eq!(record["stoppedTheCall"], false);
    assert!(
        record["error"]
            .as_str()
            .is_some_and(|message| message.contains("having a day"))
    );
}

#[tokio::test]
async fn a_hook_authenticates_with_the_registry_and_never_echoes_the_credential() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "21"}]})],
    )
    .await;
    let hook = hook_endpoint(200, "ok").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    // `PATH` stands in for a credential variable, as elsewhere in this file.
    let secret = std::env::var("PATH").expect("PATH");

    let harness = Harness::start(&[
        (
            "auth.yaml",
            "providers:\n  - name: workload\n    kind: token\n    value:\n      env: PATH\n"
                .to_owned(),
        ),
        (
            "mcp.yaml",
            mcp_with_hook(
                &mcp,
                &hook.uri(),
                &["before"],
                "",
                "              auth: workload\n              headers:\n                \
                 x-api-key: 'k-{{ auth[\"workload\"] }}'\n",
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    // Both routes really carried it: the provider's own place, and the header
    // the template put it in.
    let sent = hook.received_requests().await.expect("hook requests");
    assert_eq!(sent.len(), 1);
    let authorization = sent[0].headers.get("authorization").expect("bearer");
    assert_eq!(authorization, &format!("Bearer {secret}"));
    let api_key = sent[0].headers.get("x-api-key").expect("api key");
    assert_eq!(api_key, &format!("k-{secret}"));

    // And none of it comes back. A transcript is durable.
    let whole = serde_json::to_string(&events).expect("serialise");
    assert!(
        !whole.contains(&secret),
        "a hook's credential must never echo back"
    );
    assert!(
        whole.contains("x-api-key"),
        "the header itself is still reported — only its value is not"
    );
}

#[tokio::test]
async fn a_hook_sends_the_json_document_the_file_wrote() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "21"}]})],
    )
    .await;
    let hook = hook_endpoint(200, "ok").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            mcp_with_hook(
                &mcp,
                &hook.uri(),
                &["before"],
                "",
                "              json:\n                text: '{{ tool }} wants {{ arguments.city }}'\n                \
                 arguments: '{{ arguments }}'\n                attempt: 1\n",
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, _) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let sent = hook.received_requests().await.expect("hook requests");
    let body: Value = serde_json::from_slice(&sent[0].body).expect("payload");
    assert_eq!(body["text"], "get_weather wants Paris");
    // An expression on its own keeps its type: an object stays an object, and a
    // number written as one stays a number. A field that arrived as a quoted
    // rendering of a map would be valid JSON and the wrong type.
    assert_eq!(body["arguments"]["city"], "Paris");
    assert!(body["arguments"].is_object(), "{body:#?}");
    assert_eq!(body["attempt"], 1);
}

/// A hook can send the run's uploaded files, and does so as a real upload.
///
/// The whole point of `multipart:`: the endpoint gets a `multipart/form-data`
/// carrying the bytes under the field *it* asked for, so an endpoint that wants
/// `file` gets `file`. The trace, meanwhile, must describe what went out
/// and repeat none of it.
#[tokio::test]
async fn a_hook_sends_the_uploads_a_field_names_as_multipart_parts() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "21"}]})],
    )
    .await;
    let hook = hook_endpoint(200, "ok").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            mcp_with_hook(
                &mcp,
                &hook.uri(),
                &["before"],
                "",
                "              multipart:\n                file: notes.txt\n",
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, attached) = harness.upload("notes.txt", b"ping").await;
    assert_eq!(status, 200);
    let (status, ignored) = harness.upload("skip.bin", b"\x00\x01").await;
    assert_eq!(status, 200);

    let (status, events) = harness
        .agent(json!({
            "profile": "chat",
            "prompt": "weather in Paris?",
            "uploads": [
                attached["id"].as_str().expect("id"),
                ignored["id"].as_str().expect("id"),
            ],
        }))
        .await;
    assert_eq!(status, 200);

    // What actually went out: one request, multipart, carrying the file the
    // field named and nothing else.
    let sent = hook.received_requests().await.expect("hook requests");
    assert_eq!(sent.len(), 1);
    let content_type = sent[0]
        .headers
        .get("content-type")
        .expect("content-type")
        .to_str()
        .expect("text");
    assert!(
        content_type.starts_with("multipart/form-data; boundary="),
        "{content_type}"
    );

    let body = String::from_utf8_lossy(&sent[0].body);
    assert!(body.contains(r#"name="file""#), "{body}");
    assert!(body.contains(r#"filename="notes.txt""#), "{body}");
    // The bytes themselves, not a description of them.
    assert!(body.contains("ping"), "{body}");
    // No payload part: a multipart is not a JSON body with files stapled to it,
    // and nobody asked for one.
    assert!(!body.contains(r#"name="payload""#), "{body}");
    assert!(!body.contains(r#""tool":"get_weather""#), "{body}");
    // And the upload the field did not name stayed home.
    assert!(!body.contains("skip.bin"), "{body}");

    // The trace names the field, the file and its size, and carries none of its
    // bytes.
    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
    let record = &turn["hooks"][0];
    assert_eq!(record["files"][0]["field"], "file");
    assert_eq!(record["files"][0]["name"], "notes.txt");
    assert_eq!(record["files"][0]["size"], 4);
    assert_eq!(record["files"][0]["contentType"], "text/plain");
    // A multipart has no body text to show, and the parts above are why.
    assert_eq!(record["request"], "");
}

/// A field naming a file nobody attached stops the hook rather than going out
/// half-filled.
///
/// The failure this shape exists to make loud: the `422` that started all this
/// came back from an endpoint asked to validate a form with no file in it.
#[tokio::test]
async fn a_multipart_field_that_names_no_upload_fails_the_hook() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "21"}]})],
    )
    .await;
    let hook = hook_endpoint(200, "ok").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            mcp_with_hook(
                &mcp,
                &hook.uri(),
                &["before"],
                "        on_error: continue\n",
                "              multipart:\n                file: '{{ uploads }}'\n",
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
    let record = &turn["hooks"][0];
    let error = record["error"].as_str().expect("the reason");
    assert!(error.contains("file"), "{error}");
    assert!(error.contains("nothing was attached"), "{error}");

    // Nothing went out: an empty form is what the endpoint would have had to
    // explain back to us.
    assert!(
        hook.received_requests()
            .await
            .unwrap_or_default()
            .is_empty(),
        "a hook with nothing to attach must not send an empty form"
    );
}

/// A `tools:` entry is a regex, and it has to match the whole name.
///
/// Both halves in one run: `get_.*` covers the tool that is about to be called,
/// and `weather` — a substring of `get_weather`, and an exact name under nobody's
/// rules — does not. The second is the one worth a test: a matcher that widened
/// a gate by itself would be a hole nobody opened.
#[tokio::test]
async fn a_tool_pattern_covers_what_it_matches_and_not_what_it_merely_contains() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "21"}]})],
    )
    .await;
    let hook = hook_endpoint(200, "ok").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            mcp_with_hook(
                &mcp,
                &hook.uri(),
                &["before"],
                "        tools:\n          - get_.*\n          - weather\n",
                "",
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    // Once, not twice: `get_.*` matched and `weather` did not, and a tool covered
    // by two patterns is still one hook firing anyway.
    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
    let hooks = turn["hooks"].as_array().expect("the hooks that fired");
    assert_eq!(hooks.len(), 1, "{hooks:#?}");
    assert_eq!(hooks[0]["tool"], "get_weather");
    assert_eq!(hook.received_requests().await.expect("requests").len(), 1);
}

#[tokio::test]
async fn a_hook_scoped_to_one_tool_leaves_the_others_alone() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "21"}]})],
    )
    .await;
    let hook = hook_endpoint(200, "ok").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            mcp_with_hook(
                &mcp,
                &hook.uri(),
                &["before", "after"],
                "        tools:\n          - delete_everything\n",
                "",
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    assert!(
        hook.received_requests().await.expect("requests").is_empty(),
        "a hook that named another tool has nothing to say about this one"
    );
    let turn = &events.iter().find(|(name, _)| name == "turn").unwrap().1;
    assert!(turn["hooks"].as_array().is_none_or(Vec::is_empty));
    assert!(turn["tools"][0]["error"].is_null());
}

#[tokio::test]
async fn a_server_that_answers_a_stream_is_read_the_same_way() {
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("mcp-method", "tools/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0", "id": 1,
            "result": {"resultType": "complete", "tools": weather_tool()},
        })))
        .mount(&mcp)
        .await;

    // Progress first, then the answer — the shape a slow tool produces.
    let stream = concat!(
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{\"progress\":1}}\n",
        "\n",
        "event: message\n",
        "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"resultType\":\"complete\",\"content\":[{\"type\":\"text\",\"text\":\"streamed 21\"}]}}\n",
        "\n",
    );
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("mcp-method", "tools/call"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(stream, "text/event-stream"))
        .mount(&mcp)
        .await;

    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (_, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;
    let turn = events.iter().find(|(name, _)| name == "turn").unwrap();
    assert_eq!(turn.1["tools"][0]["result"], "streamed 21");
}

#[tokio::test]
async fn a_tool_that_reports_a_problem_is_a_result_the_model_gets_to_see() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({
            "resultType": "complete",
            "content": [{"type": "text", "text": "no such city"}],
            "isError": true,
        })],
    )
    .await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;

    assert_eq!(status, 200, "a failing tool is not a failing run");
    let tool = &events.iter().find(|(n, _)| n == "turn").unwrap().1["tools"][0];
    assert!(tool["reportedError"].as_bool().unwrap());
    assert_eq!(tool["result"], "no such city");
    // `error` is for when mire could not get an answer at all. It did.
    assert!(tool.get("error").is_none(), "{tool}");
    // And the loop carried on, so the model had its chance to react.
    assert_eq!(events.iter().filter(|(n, _)| n == "turn").count(), 2);
}

#[tokio::test]
async fn a_tool_call_the_server_refused_reports_the_status_it_was_refused_with() {
    let mcp = mcp_server(weather_tool(), vec![]).await;
    // The answer a tool call gets when the credential is wrong: a status, and a
    // body that is not JSON-RPC because nothing on the server ever saw it.
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("mcp-method", "tools/call"))
        .respond_with(ResponseTemplate::new(401).set_body_string("no credential"))
        .mount(&mcp)
        .await;

    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;
    assert_eq!(status, 200);

    // The call came back an error, which says the loop got nothing — and the
    // status says who refused it, which is the half that names the fix. It is
    // the case the status exists for: an error carries no status of its own, so
    // reading it off the round trip is the only way it reaches the tool call.
    let tool = &events.iter().find(|(n, _)| n == "turn").unwrap().1["tools"][0];
    assert_eq!(tool["status"], 401);
    assert!(
        tool["error"].as_str().is_some_and(|m| m.contains("401")),
        "{tool}"
    );
}

#[tokio::test]
async fn a_server_asking_for_interactive_input_says_so_instead_of_answering_nothing() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({
            "resultType": "input_required",
            "inputRequests": [{"method": "elicitation/create"}],
            "requestState": "opaque",
        })],
    )
    .await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (_, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;

    let tool = &events.iter().find(|(n, _)| n == "turn").unwrap().1["tools"][0];
    let message = tool["error"].as_str().unwrap();
    assert!(message.contains("elicitation/create"), "{message}");
    assert!(message.contains("cannot provide"), "{message}");
}

#[tokio::test]
async fn a_simulated_tool_shadows_a_live_one_of_the_same_name() {
    let mcp = mcp_server(weather_tool(), vec![]).await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let profile = format!(
        "{}tools:\n  - name: get_weather\n    schema:\n      type: object\n    response: 'stubbed'\n",
        mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri()))
    );

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        ("chat.yaml", profile),
    ])
    .await;

    let (_, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;

    let turn = &events.iter().find(|(n, _)| n == "turn").unwrap().1;
    assert_eq!(turn["tools"][0]["source"], "simulated");
    assert_eq!(turn["tools"][0]["result"], "stubbed");

    // The server was listed but never called: stubbing one tool of an otherwise
    // live server is the point.
    let calls = mcp
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| {
            r.headers
                .get("mcp-method")
                .is_some_and(|v| v == "tools/call")
        })
        .count();
    assert_eq!(calls, 0);

    // And it is declared once, not twice, so the model is not offered two schemas.
    let sent: Value =
        serde_json::from_str(turn["call"]["request"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(sent["tools"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn an_unreachable_server_fails_the_run_before_a_prompt_is_spent() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            "servers:\n  - name: weather\n    url: http://127.0.0.1:1/mcp\n    timeout_ms: 500\n"
                .to_owned(),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    // A plain call does not need the MCP server at all.
    let (status, _, _) = harness
        .call(json!({"profile": "chat", "prompt": "go"}))
        .await;
    assert_eq!(status, 200);
    let before = endpoint.received_requests().await.unwrap().len();

    let (_, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;

    // Reachability is a runtime matter, so it arrives as a `failed` event — but
    // it arrives before any turn, and no prompt was spent finding out.
    assert!(
        events.iter().all(|(name, _)| name == "failed"),
        "{events:?}"
    );
    let failed = &events[0].1;
    assert_eq!(failed["code"], "mcp_unreachable");
    assert_eq!(endpoint.received_requests().await.unwrap().len(), before);
}

#[tokio::test]
async fn the_api_lists_servers_and_asks_one_what_it_offers() {
    let mcp = mcp_server(weather_tool(), vec![]).await;
    let harness = Harness::start(&[(
        "mcp.yaml",
        format!(
            "servers:\n  - name: weather\n    url: {}/mcp\n    tools:\n      - get_weather\n",
            mcp.uri()
        ),
    )])
    .await;

    let listing = harness.get("/api/mcp").await;
    assert_eq!(listing["servers"][0]["name"], "weather");
    assert_eq!(listing["servers"][0]["tools"][0], "get_weather");
    assert!(listing["issues"].as_array().unwrap().is_empty());

    // What this build speaks, newest first, so a client offering the choice does
    // not have to keep its own copy of the list.
    assert_eq!(
        listing["revisions"],
        json!(["2026-07-28", "2025-11-25", "2025-06-18", "2025-03-26"])
    );

    let tools = harness.get("/api/mcp/weather/tools").await;
    assert_eq!(tools["server"], "weather");
    assert_eq!(tools["tools"][0]["name"], "get_weather");
    assert_eq!(tools["tools"][0]["annotations"]["readOnlyHint"], true);

    let missing = harness.get("/api/mcp/nope/tools").await;
    assert_eq!(missing["code"], "unknown_mcp_server");
}

/// A server on one of the handshaking revisions.
///
/// It refuses `server/discover` the way a server that predates it would, issues a
/// session from `initialize`, and then demands that session back on everything.
async fn legacy_mcp_server(revision: &str) -> MockServer {
    let server = MockServer::start().await;
    let session = "session-from-initialize";

    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(body_string_contains("\"initialize\""))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("mcp-session-id", session)
                .set_body_json(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {
                        "protocolVersion": revision,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "legacy", "version": "0.1.0"},
                    },
                })),
        )
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(body_string_contains("notifications/initialized"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("mcp-session-id", session))
        .and(body_string_contains("tools/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"tools": weather_tool()},
        })))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/mcp"))
        .and(header("mcp-session-id", session))
        .and(body_string_contains("tools/call"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [{"type": "text", "text": "{\"temp\": 21}"}],
                "isError": false,
            },
        })))
        .mount(&server)
        .await;

    // Anything else, `server/discover` included: this revision never had it.
    // Mounted last on purpose — wiremock takes the first mock that matches.
    Mock::given(method("POST"))
        .and(path("/mcp"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32601, "message": "method not found"},
        })))
        .mount(&server)
        .await;

    server
}

#[tokio::test]
async fn an_older_server_is_reached_by_falling_back_to_the_handshake() {
    // The failure that started all this: `server/discover` is a method of the
    // newest revision, so it cannot be the only probe. When it comes back empty
    // handed, `initialize` is the older revisions' own negotiation.
    let mcp = legacy_mcp_server("2025-06-18").await;
    let harness = Harness::start(&[(
        "mcp.yaml",
        format!("servers:\n  - name: files\n    url: {}/mcp\n", mcp.uri()),
    )])
    .await;

    let answer = harness.get("/api/mcp/files/tools").await;
    assert_eq!(answer["protocol"]["revision"], "2025-06-18");
    assert_eq!(answer["protocol"]["settled"], "handshake");
    assert_eq!(answer["tools"][0]["name"], "get_weather");

    let listing = mcp
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .find(|request| String::from_utf8_lossy(&request.body).contains("tools/list"))
        .expect("a listing went out");

    // The session the handshake issued comes back on the listing...
    assert_eq!(
        listing
            .headers
            .get("mcp-session-id")
            .map(|v| v.to_str().unwrap()),
        Some("session-from-initialize")
    );
    // ...and none of the newest revision's mirrored headers do. They are routing
    // metadata an intermediary may act on, and this server never asked for them.
    assert!(listing.headers.get("mcp-method").is_none());
    assert_eq!(
        listing
            .headers
            .get("mcp-protocol-version")
            .map(|v| v.to_str().unwrap()),
        Some("2025-06-18")
    );
}

#[tokio::test]
async fn a_server_on_the_last_handshaking_revision_is_reached_without_a_downgrade() {
    // What `initialize` proposes, so a server that speaks only this one is
    // reached on the first try rather than refused for sharing nothing.
    let mcp = legacy_mcp_server("2025-11-25").await;
    let harness = Harness::start(&[(
        "mcp.yaml",
        format!("servers:\n  - name: files\n    url: {}/mcp\n", mcp.uri()),
    )])
    .await;

    let answer = harness.get("/api/mcp/files/tools").await;
    assert_eq!(answer["protocol"]["revision"], "2025-11-25");
    assert_eq!(answer["protocol"]["settled"], "handshake");
    assert_eq!(answer["tools"][0]["name"], "get_weather");

    let listing = mcp
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .find(|request| String::from_utf8_lossy(&request.body).contains("tools/list"))
        .expect("a listing went out");

    assert_eq!(
        listing
            .headers
            .get("mcp-protocol-version")
            .map(|v| v.to_str().unwrap()),
        Some("2025-11-25")
    );
    // Handshaking revision: a session, and none of the newest one's mirroring.
    assert_eq!(
        listing
            .headers
            .get("mcp-session-id")
            .map(|v| v.to_str().unwrap()),
        Some("session-from-initialize")
    );
    assert!(listing.headers.get("mcp-method").is_none());
}

#[tokio::test]
async fn the_oldest_revision_is_not_sent_a_header_it_predates() {
    let mcp = legacy_mcp_server("2025-03-26").await;
    let harness = Harness::start(&[(
        "mcp.yaml",
        format!("servers:\n  - name: files\n    url: {}/mcp\n", mcp.uri()),
    )])
    .await;

    // `initialize` proposes the newest handshaking revision; the server answers
    // with an older one, and that is the mechanism working rather than a problem.
    let answer = harness.get("/api/mcp/files/tools").await;
    assert_eq!(answer["protocol"]["revision"], "2025-03-26");

    let listing = mcp
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .find(|request| String::from_utf8_lossy(&request.body).contains("tools/list"))
        .expect("a listing went out");
    assert!(listing.headers.get("mcp-protocol-version").is_none());
}

#[tokio::test]
async fn a_server_sharing_no_revision_says_which_rather_than_answering_400() {
    // Discovery succeeded and the answer was "nothing you speak". That is a
    // finished conversation, and it deserves better than a bare status code.
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"protocolVersions": ["2019-01-01", "2020-02-02"]},
        })))
        .mount(&mcp)
        .await;

    let harness = Harness::start(&[(
        "mcp.yaml",
        format!("servers:\n  - name: files\n    url: {}/mcp\n", mcp.uri()),
    )])
    .await;

    let answer = harness.get("/api/mcp/files/tools").await;
    assert_eq!(answer["code"], "mcp_no_common_revision");
    let message = answer["message"].as_str().unwrap();
    // Both halves, because either one alone leaves you guessing.
    assert!(message.contains("2026-07-28"), "{message}");
    assert!(message.contains("2019-01-01, 2020-02-02"), "{message}");
}

#[tokio::test]
async fn a_pinned_revision_is_used_without_asking_anybody() {
    let mcp = mcp_server(weather_tool(), vec![]).await;
    let harness = Harness::start(&[(
        "mcp.yaml",
        format!(
            "servers:\n  - name: files\n    url: {}/mcp\n    protocol_version: 2026-07-28\n",
            mcp.uri()
        ),
    )])
    .await;

    let answer = harness.get("/api/mcp/files/tools").await;
    assert_eq!(answer["protocol"]["revision"], "2026-07-28");
    assert_eq!(answer["protocol"]["settled"], "pinned");

    // A pin is a statement, not a preference: nothing was probed.
    let probed = mcp
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .any(|request| {
            let body = String::from_utf8_lossy(&request.body).to_string();
            body.contains("server/discover") || body.contains("\"initialize\"")
        });
    assert!(!probed, "a pinned revision must not negotiate");
}

#[tokio::test]
async fn a_run_can_state_its_revision_and_it_beats_both_the_file_and_the_probe() {
    // The point of the knob: this server speaks `2025-06-18` and the file pins
    // the revision it does not speak. Nothing on disk changes — the run says
    // which one it wants, and that is the one that goes out.
    let mcp = legacy_mcp_server("2025-06-18").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    protocol_version: 2026-07-28\n",
                mcp.uri()
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({
            "profile": "chat",
            "prompt": "weather in Paris?",
            "mcpProtocol": "2025-06-18",
        }))
        .await;
    assert_eq!(status, 200);

    // The tool really ran, which it could not have on the pinned revision.
    let turns: Vec<_> = events.iter().filter(|(name, _)| name == "turn").collect();
    assert_eq!(turns[0].1["tools"][0]["source"], "mcp");
    assert!(
        turns[0].1["tools"][0]["result"]
            .as_str()
            .unwrap()
            .contains("21")
    );

    // Everything the run said went out on the stated revision, setup included.
    let setup = events
        .iter()
        .find(|(name, _)| name == "setup")
        .expect("the handshake is traffic worth seeing");
    for exchange in setup.1["mcp"].as_array().unwrap() {
        assert_eq!(exchange["revision"], "2025-06-18", "{exchange}");
    }
    assert!(
        setup.1["mcp"]
            .as_array()
            .unwrap()
            .iter()
            .any(|exchange| exchange["method"] == "initialize"),
    );

    // A stated revision is a statement, like a pin: nothing was probed for it.
    let requests = mcp.received_requests().await.expect("requests");
    assert!(
        !requests
            .iter()
            .any(|request| String::from_utf8_lossy(&request.body).contains("server/discover")),
        "a stated revision must not negotiate"
    );

    // And one handshake covers the run: the listing and the call share a client,
    // rather than each tool call introducing itself again.
    let handshakes = requests
        .iter()
        .filter(|request| String::from_utf8_lossy(&request.body).contains("\"initialize\""))
        .count();
    assert_eq!(handshakes, 1, "one handshake per run, not per call");
}

#[tokio::test]
async fn a_revision_chosen_for_one_run_is_not_chosen_for_the_next() {
    // The choice covers exactly one run. If it leaked into the registry's own
    // client, this listing would come back `pinned` — and every other caller
    // would silently be speaking somebody else's revision.
    let mcp = legacy_mcp_server("2025-06-18").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, _) = harness
        .agent(json!({"profile": "chat", "prompt": "go", "mcpProtocol": "2025-06-18"}))
        .await;
    assert_eq!(status, 200);

    let answer = harness.get("/api/mcp/weather/tools").await;
    assert_eq!(answer["protocol"]["revision"], "2025-06-18");
    assert_eq!(answer["protocol"]["settled"], "handshake");
}

#[tokio::test]
async fn a_revision_this_build_never_heard_of_is_refused_before_anything_is_sent() {
    let mcp = mcp_server(weather_tool(), vec![]).await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go", "mcpProtocol": "1999-01-01"}))
        .await;

    assert_eq!(status, 422);
    assert!(events.is_empty(), "a bad revision should not open a stream");
    assert!(mcp.received_requests().await.expect("requests").is_empty());
}

#[tokio::test]
async fn a_server_that_answers_no_probe_at_all_still_gets_its_listing() {
    // `server/discover` is a method a perfectly good server may not implement.
    // Refusing to proceed would break endpoints that work in order to report a
    // problem they do not have — so the newest revision is assumed, and said.
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .and(header("mcp-method", "tools/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"tools": weather_tool()},
        })))
        .mount(&mcp)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mcp)
        .await;

    let harness = Harness::start(&[(
        "mcp.yaml",
        format!("servers:\n  - name: files\n    url: {}/mcp\n", mcp.uri()),
    )])
    .await;

    let answer = harness.get("/api/mcp/files/tools").await;
    assert_eq!(answer["tools"][0]["name"], "get_weather");
    // Assumed, not discovered: the difference is the whole point of reporting it.
    assert_eq!(answer["protocol"]["settled"], "assumed");
    assert_eq!(answer["protocol"]["revision"], "2026-07-28");
}

#[tokio::test]
async fn a_tool_call_on_an_older_revision_carries_the_session_and_mirrors_nothing() {
    // The listing proves the handshake; this proves the *call*, which is where
    // getting it wrong costs something: `Mcp-Name` and `Mcp-Param-*` are routing
    // metadata an intermediary in front of an older server may act on, and it
    // never asked for them.
    let mcp = legacy_mcp_server("2025-06-18").await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!("servers:\n  - name: weather\n    url: {}/mcp\n", mcp.uri()),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (status, events) = harness
        .agent(json!({"profile": "chat", "prompt": "weather in Paris?"}))
        .await;
    assert_eq!(status, 200);

    let turns: Vec<_> = events.iter().filter(|(name, _)| name == "turn").collect();
    let tool = &turns[0].1["tools"][0];
    assert_eq!(tool["source"], "mcp");
    assert!(tool["result"].as_str().unwrap().contains("21"), "{tool}");

    let call = mcp
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .find(|request| String::from_utf8_lossy(&request.body).contains("tools/call"))
        .expect("the tool was really called");

    assert_eq!(
        call.headers
            .get("mcp-session-id")
            .map(|v| v.to_str().unwrap()),
        Some("session-from-initialize")
    );
    assert!(call.headers.get("mcp-name").is_none());
    assert!(call.headers.get("mcp-method").is_none());
    assert!(call.headers.get("mcp-param-units").is_none());
}

#[tokio::test]
async fn a_session_the_server_has_forgotten_is_re_established_and_the_call_replayed() {
    // A restarted server has forgotten who we are. On these revisions it says so
    // with a `404` to a request that carried a session — which is exactly how it
    // differs from a gateway `404`, and why one is retried and the other is not.
    let mcp = MockServer::start().await;

    Mock::given(method("POST"))
        .and(body_string_contains("\"initialize\""))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("mcp-session-id", "before-the-restart")
                .set_body_json(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"protocolVersion": "2025-06-18", "capabilities": {}},
                })),
        )
        .up_to_n_times(1)
        .mount(&mcp)
        .await;

    Mock::given(method("POST"))
        .and(body_string_contains("\"initialize\""))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("mcp-session-id", "after-the-restart")
                .set_body_json(json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "result": {"protocolVersion": "2025-06-18", "capabilities": {}},
                })),
        )
        .mount(&mcp)
        .await;

    Mock::given(method("POST"))
        .and(body_string_contains("notifications/initialized"))
        .respond_with(ResponseTemplate::new(202))
        .mount(&mcp)
        .await;

    // The session issued before the restart is no longer known.
    Mock::given(method("POST"))
        .and(header("mcp-session-id", "before-the-restart"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&mcp)
        .await;

    Mock::given(method("POST"))
        .and(header("mcp-session-id", "after-the-restart"))
        .and(body_string_contains("tools/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"tools": weather_tool()},
        })))
        .mount(&mcp)
        .await;

    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "error": {"code": -32601, "message": "method not found"},
        })))
        .mount(&mcp)
        .await;

    let harness = Harness::start(&[(
        "mcp.yaml",
        format!("servers:\n  - name: files\n    url: {}/mcp\n", mcp.uri()),
    )])
    .await;

    // The caller sees a listing, not a session problem: recovering from this is
    // not news, and a harness that reported it as a failure would be crying wolf.
    let answer = harness.get("/api/mcp/files/tools").await;
    assert_eq!(answer["tools"][0]["name"], "get_weather");
    assert_eq!(answer["protocol"]["revision"], "2025-06-18");

    let handshakes = mcp
        .received_requests()
        .await
        .expect("requests")
        .into_iter()
        .filter(|request| String::from_utf8_lossy(&request.body).contains("\"initialize\""))
        .count();
    // Twice: once to start, once after being told the session was gone. A third
    // would mean the retry had become a loop.
    assert_eq!(handshakes, 2);
}

#[tokio::test]
async fn an_unknown_pinned_revision_is_a_load_issue_naming_what_we_speak() {
    let harness = Harness::start(&[(
        "mcp.yaml",
        "servers:\n  - name: files\n    url: https://mcp.internal/mcp\n    \
         protocol_version: 1999-01-01\n"
            .to_owned(),
    )])
    .await;

    let listing = harness.get("/api/mcp").await;
    let message = listing["issues"][0]["message"].as_str().unwrap();
    assert!(message.contains("1999-01-01"), "{message}");
    assert!(
        message.contains("2026-07-28, 2025-11-25, 2025-06-18, 2025-03-26"),
        "{message}"
    );
    // The bad entry is skipped; it must not take the file down with it.
    assert!(listing["servers"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn a_gateway_answering_instead_of_the_server_says_so_with_its_status_and_body() {
    // The failure this is about: the MCP server logs nothing and sees nothing,
    // because whatever sits in front of it answered. The message has to carry the
    // status and the body, or there is no way to tell that from a broken server.
    let front = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(404).set_body_json(json!({"message": "no Route matched"})),
        )
        .mount(&front)
        .await;

    let harness = Harness::start(&[(
        "mcp.yaml",
        format!(
            "servers:\n  - name: weather\n    url: {}/mcp\n",
            front.uri()
        ),
    )])
    .await;

    let answer = harness.get("/api/mcp/weather/tools").await;
    let message = answer["message"].as_str().unwrap();
    assert!(message.contains("404"), "{message}");
    assert!(message.contains("no Route matched"), "{message}");
}

#[tokio::test]
async fn templated_headers_reach_the_mcp_server_and_never_come_back_out() {
    let mcp = mcp_server(
        weather_tool(),
        vec![json!({"resultType": "complete", "content": [{"type": "text", "text": "ok"}]})],
    )
    .await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    // `PATH` stands in for a credential variable: the test cannot set one, since
    // `unsafe_code` is forbidden, and what matters is that the value is read from
    // the environment at request time rather than baked in at load.
    let secret = std::env::var("PATH").expect("PATH");

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    headers:\n      \
                 x-api-key: 'k-{{{{ env.PATH }}}}'\n      \
                 x-tenant: '{{{{ env.MIRE_TENANT_UNSET | default(\"dev\") }}}}'\n",
                mcp.uri()
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    // The listing names the headers, and only the names.
    let listing = harness.get("/api/mcp").await.to_string();
    assert!(listing.contains("x-api-key"), "{listing}");
    assert!(!listing.contains(&secret), "a value must never be listed");

    let (_, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;

    let call = mcp
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .find(|r| {
            r.headers
                .get("mcp-method")
                .is_some_and(|v| v == "tools/call")
        })
        .expect("a tools/call");

    assert_eq!(call.headers["x-api-key"], format!("k-{secret}"));
    // An optional header still gets its fallback rather than failing the run.
    assert_eq!(call.headers["x-tenant"], "dev");

    // And nothing rendered leaks into the trace the UI is handed.
    let streamed = format!("{events:?}");
    assert!(!streamed.contains(&secret), "the value must not be traced");
}

#[tokio::test]
async fn a_header_whose_variable_is_missing_fails_loudly_rather_than_sending_an_empty_one() {
    let mcp = mcp_server(weather_tool(), vec![]).await;
    let endpoint = MockServer::start().await;
    model_using_a_tool(&endpoint).await;

    let harness = Harness::start(&[
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: weather\n    url: {}/mcp\n    headers:\n      \
                 authorization: 'Bearer {{{{ env.MIRE_TOKEN_THAT_IS_NOT_SET }}}}'\n",
                mcp.uri()
            ),
        ),
        (
            "chat.yaml",
            mcp_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
        ),
    ])
    .await;

    let (_, events) = harness
        .agent(json!({"profile": "chat", "prompt": "go"}))
        .await;

    let failed = events
        .iter()
        .find(|(name, _)| name == "failed")
        .expect("a failure");
    assert_eq!(failed.1["code"], "mcp_header_error");
    let message = failed.1["message"].as_str().unwrap();
    // Naming the variable is the whole point: `Authorization: Bearer ` would
    // otherwise look present and fail somewhere far less helpful.
    assert!(message.contains("MIRE_TOKEN_THAT_IS_NOT_SET"), "{message}");

    // And nothing was sent with a half-built credential.
    assert!(mcp.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_broken_header_template_is_a_load_issue_rather_than_a_surprise_later() {
    let harness = Harness::start(&[(
        "mcp.yaml",
        "servers:\n  - name: weather\n    url: https://mcp.internal/mcp\n    headers:\n      \
         authorization: '{{ unclosed'\n"
            .to_owned(),
    )])
    .await;

    let listing = harness.get("/api/mcp").await;
    assert!(listing["servers"].as_array().unwrap().is_empty());
    let issue = listing["issues"][0]["message"].as_str().unwrap();
    assert!(issue.contains("weather"), "{issue}");
    assert!(issue.contains("authorization"), "{issue}");
}

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

/// A profile that asks to stream and knows where the text sits in a chunk.
///
/// `"stream": {{ stream }}` is the load-bearing line: nothing `mire` does makes
/// an endpoint chunk its answer, so the template has to pass the flag on.
fn streaming_profile(url: &str) -> String {
    format!(
        r#"
name: chat
kind: chat
url: {url}
timeout_ms: 5000
request:
  template: |
    {{"model": "m", "messages": {{{{ messages | tojson }}}}, "stream": {{{{ stream | tojson }}}}}}
decode:
  content: ["$.choices[0].message.content"]
  delta: ["$.choices[0].delta.content", "$.message.content"]
  finish_reason: ["$.choices[0].finish_reason", "$.done_reason"]
  usage: ["$.usage", "$"]
  error: ["$.error"]
"#
    )
}

/// Serves `body` as an event stream.
///
/// `set_body_raw` rather than `set_body_string`: the latter sets its own
/// `content-type` afterwards, which would undo the framing the test is about.
fn event_stream(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_raw(body.to_owned(), "text/event-stream")
}

#[tokio::test]
async fn a_streamed_call_arrives_in_pieces_and_adds_up_to_the_answer() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        // The flag reached the wire, which is the only reason any of this streams.
        .and(body_string_contains(r#""stream": true"#))
        .respond_with(event_stream(concat!(
            // The first chunk of an OpenAI stream announces the role and carries
            // no text at all.
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n",
        )))
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[(
        "chat.yaml",
        streaming_profile(&format!("{}/v1/chat/completions", endpoint.uri())),
    )])
    .await;

    let (status, events) = harness
        .stream(json!({"profile": "chat", "prompt": "hi"}))
        .await;
    assert_eq!(status, 200);

    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    // The head first, then one event per chunk that carried text, then the
    // whole outcome. The role-only chunk produced nothing, which is the point.
    assert_eq!(names, ["open", "delta", "delta", "done"], "{events:?}");

    assert_eq!(events[0].1["status"], 200);
    assert_eq!(events[1].1["text"], "Hel");
    assert_eq!(events[2].1["text"], "lo");

    let response = &events[3].1["response"];
    assert_eq!(response["decoded"]["content"], "Hello");
    assert_eq!(response["decoded"]["finishReason"], "stop");
    assert_eq!(response["decoded"]["usage"]["completionTokens"], 2);

    let stream = &response["stream"];
    assert_eq!(stream["framing"], "sse");
    assert_eq!(stream["chunks"], 4);
    assert_eq!(stream["deltas"], 2);
    assert_eq!(stream["unparsable"], 0);
    assert_eq!(stream["terminated"], true);
}

/// A refused stream is not a stream: the endpoint answers in one shot, and that
/// single object is the last frame there is. The error still decodes out of it.
#[tokio::test]
async fn a_stream_refused_before_it_started_still_reports_why() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": "model 'qwen3:0.6b' not found, try pulling it first"
        })))
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[("chat.yaml", streaming_profile(&endpoint.uri()))]).await;

    let (status, events) = harness
        .stream(json!({"profile": "chat", "prompt": "hi"}))
        .await;

    assert_eq!(status, 200);
    let done = &events.last().expect("done").1;
    assert_eq!(done["response"]["http"]["status"], 404);
    assert_eq!(
        done["response"]["error"]["message"],
        "model 'qwen3:0.6b' not found, try pulling it first"
    );
    // Nothing was streamed, and the stream view says exactly that.
    assert_eq!(done["response"]["stream"]["deltas"], 0);
}

#[tokio::test]
async fn time_to_first_token_is_measured_from_the_first_chunk_that_had_one() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            event_stream(concat!(
                "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n",
                "data: [DONE]\n\n",
            ))
            // Long enough that a clock reading zero would be a bug rather than a
            // fast machine.
            .set_delay(Duration::from_millis(60)),
        )
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[("chat.yaml", streaming_profile(&endpoint.uri()))]).await;

    let (_, events) = harness
        .stream(json!({"profile": "chat", "prompt": "hi"}))
        .await;
    let http = &events.last().expect("done").1["response"]["http"];

    let ttft = http["ttftMs"].as_u64().expect("a time to first token");
    let latency = http["latencyMs"].as_u64().expect("a latency");
    assert!(ttft >= 50, "ttft {ttft} looks like it was not measured");
    assert!(
        ttft <= latency,
        "ttft {ttft} after the whole call {latency}"
    );
}

#[tokio::test]
async fn an_ndjson_stream_is_read_without_being_told() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            concat!(
                "{\"message\":{\"content\":\"pi\"},\"done\":false}\n",
                "{\"message\":{\"content\":\"ng\"},\"done\":false}\n",
                // Ollama ends without a trailing newline often enough, and this
                // is the object carrying the counters.
                "{\"message\":{\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\",\"eval_count\":7}"
            )
            .to_owned(),
            "application/x-ndjson",
        ))
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[("chat.yaml", streaming_profile(&endpoint.uri()))]).await;

    let (_, events) = harness
        .stream(json!({"profile": "chat", "prompt": "hi"}))
        .await;

    let done = &events.last().expect("done").1;
    let response = &done["response"];
    assert_eq!(response["stream"]["framing"], "ndjson");
    assert_eq!(response["decoded"]["content"], "ping");
    assert_eq!(response["decoded"]["finishReason"], "stop");
    // `usage: ["$"]` reaches Ollama's top-level counters.
    assert_eq!(response["decoded"]["usage"]["completionTokens"], 7);
    // No sentinel in NDJSON: the stop reason is what says it ended on purpose.
    assert_eq!(response["stream"]["terminated"], true);
}

#[tokio::test]
async fn a_stream_that_simply_stops_is_reported_as_unterminated() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(event_stream(
            "data: {\"choices\":[{\"delta\":{\"content\":\"half a sen\"}}]}\n\n",
        ))
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[("chat.yaml", streaming_profile(&endpoint.uri()))]).await;

    let (_, events) = harness
        .stream(json!({"profile": "chat", "prompt": "hi"}))
        .await;

    let response = &events.last().expect("done").1["response"];
    // What arrived is kept. A truncated answer is a finding, not a failure —
    // and this is exactly what a proxy cutting a long generation looks like.
    assert_eq!(response["decoded"]["content"], "half a sen");
    assert_eq!(response["stream"]["terminated"], false);
}

#[tokio::test]
async fn a_frame_that_is_not_json_is_counted_rather_than_swallowed() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(event_stream(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n",
            "data: <html>502 from a proxy that got bored</html>\n\n",
        )))
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[("chat.yaml", streaming_profile(&endpoint.uri()))]).await;

    let (_, events) = harness
        .stream(json!({"profile": "chat", "prompt": "hi"}))
        .await;

    let response = &events.last().expect("done").1["response"];
    assert_eq!(response["stream"]["unparsable"], 1);
    assert_eq!(response["decoded"]["content"], "ok");
}

/// The head is known long before the body, so a rejection should not wait for a
/// stream that will never carry anything.
#[tokio::test]
async fn a_rejected_streamed_call_says_so_in_its_first_event() {
    let endpoint = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({"error": "no"})))
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[("chat.yaml", streaming_profile(&endpoint.uri()))]).await;

    let (status, events) = harness
        .stream(json!({"profile": "chat", "prompt": "hi"}))
        .await;

    // A 401 from the endpoint under test is a successful call, streamed or not.
    assert_eq!(status, 200);
    assert_eq!(events[0].0, "open");
    assert_eq!(events[0].1["status"], 401);
    assert_eq!(
        events.last().expect("done").1["response"]["http"]["status"],
        401
    );
}

#[tokio::test]
async fn a_credential_never_appears_in_a_delta() {
    let endpoint = MockServer::start().await;
    // An endpoint that quotes the credential back at us, which is not as rare as
    // it should be.
    Mock::given(method("POST"))
        .and(header("authorization", "Bearer hunter2"))
        .respond_with(event_stream(
            "data: {\"choices\":[{\"delta\":{\"content\":\"your token is hunter2\"}}]}\n\n",
        ))
        .mount(&endpoint)
        .await;

    let harness = Harness::start(&[
        (
            "auth.yaml",
            "providers:\n  - name: typed\n    kind: token\n".to_owned(),
        ),
        ("chat.yaml", streaming_profile(&endpoint.uri())),
    ])
    .await;

    let (_, raw) = {
        let response = harness
            .client
            .post(format!("{}/api/call/stream", harness.base))
            .json(&json!({
                "profile": "chat",
                "auth": "typed",
                "token": "hunter2",
                "prompt": "hi",
            }))
            .send()
            .await
            .expect("call mire");
        let status = response.status().as_u16();
        (status, response.text().await.expect("read stream"))
    };

    // Not once, anywhere: not in a delta, not in the aggregate, not in the raw
    // stream text carried by the final outcome.
    assert!(!raw.contains("hunter2"), "the credential leaked: {raw}");
    assert!(raw.contains("your token is"), "{raw}");
}

#[tokio::test]
async fn streaming_something_that_cannot_stream_is_refused_before_anything_is_sent() {
    let harness = Harness::start(&[
        (
            "chat.yaml",
            streaming_profile("https://models.internal/v1/chat/completions"),
        ),
        (
            "embed.yaml",
            r#"
name: embed
kind: embedding
url: https://models.internal/v1/embeddings
request:
  template: '{"input": {{ input | tojson }}}'
"#
            .to_owned(),
        ),
    ])
    .await;

    // A refusal is an ordinary HTTP error, not a stream whose first event is a
    // failure: there is nothing to stream, and a status code is easier to act on.
    let (status, body) = harness
        .post(
            "/api/call/stream",
            json!({"profile": "embed", "input": ["x"]}),
        )
        .await;
    assert_eq!(status, 422);
    assert_eq!(body["code"], "not_a_chat_profile");

    let (status, body) = harness
        .post("/api/call/stream", json!({"profile": "nope"}))
        .await;
    assert_eq!(status, 404);
    assert_eq!(body["code"], "unknown_profile");
}

/// A token file outside the watched directory, so writing it does not trip the
/// config watcher mid-test.
fn token_file(name: &str, value: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("mire-{name}-{}", std::process::id()));
    std::fs::write(&path, format!("{value}\n")).expect("write token file");
    path
}

#[tokio::test]
async fn an_mcp_server_can_take_its_token_from_the_auth_registry() {
    let secret = token_file("mcp-auth-token", "s3cr3t-from-auth");
    let mcp = MockServer::start().await;
    // The server wants the credential in a place no `auth:` provider would put
    // it by itself, and refuses anything else — so a passing test means the
    // token really travelled, not that the request merely succeeded.
    Mock::given(method("POST"))
        .and(header("x-api-key", "key-s3cr3t-from-auth"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "resultType": "complete",
                "tools": [{"name": "read_file", "inputSchema": {"type": "object"}}],
            },
        })))
        .mount(&mcp)
        .await;

    let harness = Harness::start(&[
        (
            "auth.yaml",
            format!(
                "providers:\n  - name: workload\n    kind: token\n    value:\n      file: {}\n",
                secret.display()
            ),
        ),
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: files\n    url: {}/mcp\n    headers:\n      \
                 x-api-key: 'key-{{{{ auth[\"workload\"] }}}}'\n",
                mcp.uri()
            ),
        ),
    ])
    .await;

    let listing = harness.get("/api/mcp/files/tools").await;
    assert_eq!(listing["tools"][0]["name"], "read_file");

    // And it does not come back out: not in the listing, not in the server view.
    let text = serde_json::to_string(&listing).expect("serialise");
    assert!(
        !text.contains("s3cr3t-from-auth"),
        "the credential leaked: {text}"
    );
    let servers = serde_json::to_string(&harness.get("/api/mcp").await).expect("serialise");
    assert!(!servers.contains("s3cr3t-from-auth"), "{servers}");

    std::fs::remove_file(&secret).ok();
}

#[tokio::test]
async fn a_rotated_token_reaches_the_next_mcp_call_without_a_restart() {
    let secret = token_file("mcp-rotate-token", "first");
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"resultType": "complete", "tools": []},
        })))
        .mount(&mcp)
        .await;

    let harness = Harness::start(&[
        (
            "auth.yaml",
            format!(
                "providers:\n  - name: workload\n    kind: token\n    value:\n      file: {}\n",
                secret.display()
            ),
        ),
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: files\n    url: {}/mcp\n    headers:\n      \
                 x-api-key: '{{{{ auth[\"workload\"] }}}}'\n",
                mcp.uri()
            ),
        ),
    ])
    .await;

    harness.get("/api/mcp/files/tools").await;
    // A projected service account token is rewritten in place, under a process
    // that never restarts. Resolving the credential once at load would send the
    // expired one forever.
    std::fs::write(&secret, "second\n").expect("rotate");
    harness.get("/api/mcp/files/tools").await;

    // Only the listings: negotiation puts its own probes on the wire, and they
    // are not what this is about. They do carry the credential, which is the
    // point of checking the header rather than the count.
    let sent: Vec<String> = mcp
        .received_requests()
        .await
        .expect("requests")
        .iter()
        .filter(|request| {
            request
                .headers
                .get("mcp-method")
                .and_then(|value| value.to_str().ok())
                == Some("tools/list")
        })
        .map(|request| {
            request
                .headers
                .get("x-api-key")
                .and_then(|value| value.to_str().ok())
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(sent, vec!["first", "second"]);

    std::fs::remove_file(&secret).ok();
}

#[tokio::test]
async fn a_server_whose_token_needs_a_login_says_so_rather_than_calling() {
    let mcp = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&mcp)
        .await;

    let harness = Harness::start(&[
        (
            "auth.yaml",
            "providers:\n  - name: me\n    kind: oidc_browser\n    issuer: https://idp.internal/realms/x\n    client_id: mire-ui\n"
                .to_owned(),
        ),
        (
            "mcp.yaml",
            format!(
                "servers:\n  - name: files\n    url: {}/mcp\n    headers:\n      \
                 x-api-key: '{{{{ auth[\"me\"] }}}}'\n",
                mcp.uri()
            ),
        ),
    ])
    .await;

    let (status, body) = {
        let response = harness
            .client
            .get(format!("{}/api/mcp/files/tools", harness.base))
            .send()
            .await
            .expect("get");
        let status = response.status().as_u16();
        (status, response.json::<Value>().await.expect("json"))
    };

    // `409`, not `401`: the endpoint has not rejected anything, it has not been
    // asked. The answer is a button in the auth panel.
    assert_eq!(status, 409);
    assert_eq!(body["code"], "not_signed_in");
    // And nothing was sent half-authenticated.
    assert!(mcp.received_requests().await.unwrap().is_empty());
}

/// The shape a replayed tool call has to have.
///
/// Measured against a local Ollama, both of its endpoints: the flat
/// `{"name": …, "arguments": {…}}` this used to send is refused with
/// `400 invalid tool call arguments`, so every agent run died on its second
/// turn — the one where the tool result finally reaches the model.
#[tokio::test]
async fn a_replayed_tool_call_goes_back_in_the_shape_endpoints_accept() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(wants_tool("Paris")))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_answer("21 degrees.")))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "agent.yaml",
        agent_profile(&format!("{}/v1", server.uri()), ""),
    )])
    .await;
    harness
        .agent(json!({"profile": "agent", "prompt": "weather in Paris?"}))
        .await;

    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 2, "the loop should have run two turns");

    let replayed: Value = serde_json::from_slice(&requests[1].body).expect("second turn body");
    let assistant = &replayed["messages"][1];
    assert_eq!(assistant["role"], "assistant");

    let call = &assistant["tool_calls"][0];
    // Nested under `function`, with the type both families expect. Nothing flat.
    assert_eq!(call["type"], "function");
    assert_eq!(call["function"]["name"], "get_weather");
    assert!(
        call.get("name").is_none(),
        "a flat `name` came back: {call}"
    );

    // The endpoint sent its arguments as a JSON string, so it gets a JSON string
    // back — this one rejects an object here, and Ollama's native API rejects a
    // string. Handing back what arrived is what satisfies both.
    let arguments = call["function"]["arguments"]
        .as_str()
        .unwrap_or_else(|| panic!("arguments should be a string: {call}"));
    assert_eq!(
        serde_json::from_str::<Value>(arguments).expect("arguments parse"),
        json!({"city": "Paris"})
    );

    // And the tool result is tied back to the call it answers.
    assert_eq!(replayed["messages"][2]["role"], "tool");
    assert_eq!(replayed["messages"][2]["tool_call_id"], "call_Paris");
}

/// The other half: an endpoint that sends its arguments as an object gets an
/// object back.
#[tokio::test]
async fn arguments_that_arrived_as_an_object_are_replayed_as_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "c1",
                        "function": {"name": "get_weather", "arguments": {"city": "Lyon"}},
                    }],
                },
                "finish_reason": "tool_calls",
            }],
        })))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_answer("21 degrees.")))
        .mount(&server)
        .await;

    let harness = Harness::start(&[(
        "agent.yaml",
        agent_profile(&format!("{}/v1", server.uri()), ""),
    )])
    .await;
    harness
        .agent(json!({"profile": "agent", "prompt": "weather in Lyon?"}))
        .await;

    let requests = server.received_requests().await.expect("requests");
    let replayed: Value = serde_json::from_slice(&requests[1].body).expect("second turn body");
    assert_eq!(
        replayed["messages"][1]["tool_calls"][0]["function"]["arguments"],
        json!({"city": "Lyon"})
    );
}

// --- a loop that streams ------------------------------------------------------

/// An agent profile that passes `stream` on and knows where a chunk keeps its
/// text.
///
/// Same shape as [`agent_profile`] with the two streaming lines added, because
/// that is the whole difference: streaming is a flag on the run, not a second
/// kind of profile.
fn streaming_agent_profile(url: &str) -> String {
    format!(
        r#"
name: agent
kind: chat
url: {url}
timeout_ms: 5000
request:
  template: |
    {{"model": "m", "messages": {{{{ messages | tojson }}}}, "stream": {{{{ stream | tojson }}}}}}
decode:
  content: ["$.choices[0].message.content"]
  delta: ["$.choices[0].delta.content"]
  tool_calls: ["$.choices[0].message.tool_calls"]
  finish_reason: ["$.choices[0].finish_reason"]
agent:
  stop_when:
    no_tool_calls: true
  max_iterations: 3
"#
    )
}

/// An agent run reads whole answers unless the request asks otherwise, and the
/// template is told which it is.
#[tokio::test]
async fn a_loop_does_not_stream_unless_the_run_asks_for_it() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(final_answer("whole")))
        .mount(&server)
        .await;

    let harness = Harness::start(&[("agent.yaml", streaming_agent_profile(&server.uri()))]).await;

    let (status, events) = harness
        .agent(json!({"profile": "agent", "prompt": "hi"}))
        .await;
    assert_eq!(status, 200);

    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(names, vec!["turn", "done"], "{events:?}");

    // Not merely "no deltas arrived": the flag reached the template as `false`,
    // which is what an endpoint serving both shapes actually reads.
    let requests = server.received_requests().await.expect("requests");
    let body: Value = serde_json::from_slice(&requests[0].body).expect("body");
    assert_eq!(body["stream"], false);
}

/// The same loop, streamed: every turn arrives chunk by chunk, and each delta
/// says which turn it belongs to.
#[tokio::test]
async fn a_streamed_loop_reports_deltas_per_turn() {
    let server = MockServer::start().await;
    // Turn one asks for a tool, and the call is in the *last* chunk — the only
    // place `mire` reads one from in a stream. An endpoint that split it across
    // chunks would end the loop here instead, which is a fact about the endpoint
    // rather than a thing to work around.
    Mock::given(method("POST"))
        .respond_with(event_stream(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"look\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ing\"}}]}\n\n",
            "data: {\"choices\":[{\"message\":{\"tool_calls\":[{\"id\":\"c1\",\
             \"function\":{\"name\":\"get_weather\",\"arguments\":\"{}\"}}]},\
             \"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n",
        )))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(event_stream(concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"21 \"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"degrees\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n",
        )))
        .mount(&server)
        .await;

    let mut profile = streaming_agent_profile(&server.uri());
    profile.push_str(
        "tools:\n  - name: get_weather\n    schema:\n      type: object\n    \
         response: '{\"temp\": 21}'\n",
    );
    let harness = Harness::start(&[("agent.yaml", profile)]).await;

    let (status, events) = harness
        .agent(json!({"profile": "agent", "prompt": "weather in Lyon?", "stream": true}))
        .await;
    assert_eq!(status, 200);

    let names: Vec<&str> = events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec!["delta", "delta", "turn", "delta", "delta", "turn", "done"],
        "{events:?}"
    );

    // Each one says whose it is, which is the only thing separating the end of
    // one answer from the start of the next.
    let deltas: Vec<(u64, &str)> = events
        .iter()
        .filter(|(name, _)| name == "delta")
        .map(|(_, payload)| {
            (
                payload["turn"].as_u64().expect("a turn"),
                payload["text"].as_str().expect("text"),
            )
        })
        .collect();
    assert_eq!(
        deltas,
        vec![(1, "look"), (1, "ing"), (2, "21 "), (2, "degrees")]
    );

    // And the turns are the ordinary ones: the loop read a streamed answer the
    // same way it reads any other, tool call included.
    assert_eq!(
        events[2].1["call"]["response"]["decoded"]["content"],
        "looking"
    );
    assert_eq!(events[2].1["tools"][0]["call"]["name"], "get_weather");
    assert_eq!(
        events[5].1["call"]["response"]["decoded"]["content"],
        "21 degrees"
    );
    assert_eq!(events[6].1["stop"]["outcome"], "stopped");

    // Every turn streamed, not only the first: a loop that changed its mind
    // halfway would be measuring two different things and calling them one run.
    let requests = server.received_requests().await.expect("requests");
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let body: Value = serde_json::from_slice(&request.body).expect("body");
        assert_eq!(body["stream"], true);
    }
}

// --- uploads -----------------------------------------------------------------

#[tokio::test]
async fn an_attached_file_lands_in_the_upload_directory() {
    let mire = Harness::start(&[]).await;

    let (status, body) = mire.upload("report.pdf", b"a known signal").await;

    assert_eq!(status, 200);
    assert_eq!(body["name"], "report.pdf");
    assert_eq!(body["size"], 14);
    assert_eq!(body["contentType"], "application/octet-stream");

    // The stored name is the display name with a prefix, and the response says
    // which is which — the two are not interchangeable anywhere.
    let stored = body["storedAs"].as_str().expect("storedAs");
    assert!(stored.ends_with("-report.pdf"), "stored as `{stored}`");
    assert_eq!(mire.stored(), vec![stored.to_owned()]);
    assert_eq!(
        std::fs::read(body["path"].as_str().expect("path")).expect("read back"),
        b"a known signal"
    );
}

/// The one that matters. A name is a display name; it never gets to decide where
/// bytes go, whatever it looks like.
#[tokio::test]
async fn a_file_name_cannot_write_outside_the_upload_directory() {
    let mire = Harness::start(&[]).await;

    let (status, body) = mire.upload("../../../etc/mire-owned", b"nope").await;

    assert_eq!(status, 200);
    let stored = body["storedAs"].as_str().expect("storedAs");
    assert!(stored.ends_with("-mire-owned"), "stored as `{stored}`");
    // Flattened to one segment, and sitting where every other upload sits.
    assert!(!stored.contains('/'), "`{stored}` is more than one segment");
    assert_eq!(mire.stored(), vec![stored.to_owned()]);
}

#[tokio::test]
async fn two_uploads_of_the_same_name_are_two_files() {
    let mire = Harness::start(&[]).await;

    let (_, first) = mire.upload("payload.json", b"one").await;
    let (_, second) = mire.upload("payload.json", b"two").await;

    assert_ne!(first["storedAs"], second["storedAs"]);
    assert_eq!(mire.stored().len(), 2);
}

#[tokio::test]
async fn a_body_carrying_no_file_is_refused() {
    let mire = Harness::start(&[]).await;

    let response = mire
        .client
        .post(format!("{}/api/uploads", mire.base))
        .multipart(reqwest::multipart::Form::new().text("note", "no file here"))
        .send()
        .await
        .expect("upload");

    assert_eq!(response.status().as_u16(), 400);
    let body: Value = response.json().await.expect("json");
    assert_eq!(body["code"], "no_file");
    assert!(mire.stored().is_empty());
}

/// Over the cap the store enforces: a `413` naming the limit, and nothing on
/// disk. The route's own body limit sits above this on purpose, so the answer
/// comes from `mire` rather than from the router cutting the body off.
#[tokio::test]
async fn a_file_over_the_cap_is_refused_and_nothing_is_written() {
    let mire = Harness::start(&[]).await;

    let (status, body) = mire
        .upload("big.bin", &vec![0_u8; mire::uploads::MAX_BYTES + 1])
        .await;

    assert_eq!(status, 413);
    assert_eq!(body["code"], "upload_too_large");
    assert_eq!(body["detail"]["limitBytes"], mire::uploads::MAX_BYTES);
    assert!(mire.stored().is_empty());
}

/// Behind a notebook proxy every route moves, and a hard-coded `/api/uploads` in
/// the front end would be the one that did not.
#[tokio::test]
async fn uploads_are_reachable_under_a_base_path() {
    let mire = Harness::start_at(&[], "/notebook/team/gleroy/proxy/8787").await;

    let (status, _) = mire.upload("report.pdf", b"hi").await;

    assert_eq!(status, 200);
    assert_eq!(mire.stored().len(), 1);
}

/// A profile that turns every attachment into the content-part shape a vision
/// endpoint reads. What an upload becomes is the template's decision, so the
/// test states it in a template rather than asserting on something built in Rust.
fn vision_profile(url: &str) -> String {
    format!(
        r#"
name: vision
kind: chat
url: {url}
timeout_ms: 5000
request:
  template: |
    {{"model": "m", "content": [{{% for file in uploads %}}{{"type": "image_url", "name": "{{{{ file.name }}}}", "image_url": {{"url": "{{{{ file.dataUrl }}}}"}}}}{{% endfor %}}]}}
decode:
  content: ["$.choices[0].message.content"]
"#
    )
}

#[tokio::test]
async fn an_uploaded_file_reaches_the_endpoint_through_the_template() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let mire = Harness::start(&[(
        "vision.yaml",
        vision_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    // The whole round trip: store it, then name it in a call.
    let (_, stored) = mire.upload("shot.png", &[0x89, b'P', b'N', b'G']).await;
    let id = stored["id"].as_str().expect("id");

    let (status, _, body) = mire
        .call(json!({"profile": "vision", "prompt": "what is this", "uploads": [id]}))
        .await;

    assert_eq!(status, 200);
    let sent: Value = serde_json::from_str(body["request"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(sent["content"][0]["name"], "shot.png");
    assert_eq!(
        sent["content"][0]["image_url"]["url"],
        "data:image/png;base64,iVBORw=="
    );

    // And it is what the endpoint received, not merely what we were shown.
    let received = &server.received_requests().await.unwrap()[0];
    assert!(
        std::str::from_utf8(&received.body)
            .unwrap()
            .contains("data:image/png;base64,iVBORw=="),
    );
}

/// The same rule `stream` follows: it reaches the template, and a template that
/// says nothing about it sends what it always sent.
#[tokio::test]
async fn a_profile_that_ignores_uploads_sends_what_it_always_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let mire = Harness::start(&[(
        "chat.yaml",
        openai_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (_, stored) = mire.upload("shot.png", &[0x89, b'P', b'N', b'G']).await;

    let (status, _, body) = mire
        .call(json!({
            "profile": "chat",
            "prompt": "ping",
            "uploads": [stored["id"].as_str().expect("id")],
        }))
        .await;

    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(body["request"]["body"].as_str().unwrap()).unwrap(),
        json!({"model": "m", "messages": [{"role": "user", "content": "ping"}]})
    );
}

/// Named but not there: the call is refused rather than sent short. A body
/// quietly missing its attachment is the failure this tool exists to prevent.
#[tokio::test]
async fn a_call_naming_an_upload_that_is_gone_is_refused_before_anything_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let mire = Harness::start(&[(
        "vision.yaml",
        vision_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = mire
        .call(json!({"profile": "vision", "prompt": "ping", "uploads": ["aaaaaaaaaaaa"]}))
        .await;

    assert_eq!(status, 404);
    assert_eq!(body["code"], "unknown_upload");
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// An id is checked against one character class before it is joined onto the
/// upload directory, so a path never gets that far.
#[tokio::test]
async fn a_call_cannot_name_a_file_outside_the_upload_directory() {
    let mire =
        Harness::start(&[("vision.yaml", vision_profile("https://models.internal/v1"))]).await;

    let (status, _, body) = mire
        .call(json!({"profile": "vision", "prompt": "ping", "uploads": ["../../etc/passwd"]}))
        .await;

    assert_eq!(status, 400);
    assert_eq!(body["code"], "invalid_upload_id");
}

/// A profile whose request is built around the file, and says so.
///
/// A `template:` rather than a `multipart:`, deliberately: the rule is about
/// whether there is a call to make, and it holds for every request source.
fn requires_upload_profile(url: &str) -> String {
    format!(
        r#"
name: transcribe
kind: chat
url: {url}
timeout_ms: 5000
requires_upload: true
request:
  template: |
    {{"model": "m", "audio": "{{{{ uploads[0].base64 }}}}"}}
decode:
  content: ["$.choices[0].message.content"]
"#
    )
}

/// The refusal is the point: an endpoint asked to read a form with no file in it
/// answers about a field of its own, and this one answers about the profile.
#[tokio::test]
async fn a_profile_that_requires_a_file_refuses_a_call_carrying_none() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let mire = Harness::start(&[(
        "transcribe.yaml",
        requires_upload_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (status, _, body) = mire
        .call(json!({"profile": "transcribe", "prompt": "transcribe this"}))
        .await;

    assert_eq!(status, 422);
    assert_eq!(body["code"], "upload_required");
    assert!(
        body["message"]
            .as_str()
            .expect("message")
            .contains("transcribe")
    );
    // Nothing left the process, which is the half of the claim worth checking.
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Any file clears it. The profile asked for one, not for a particular one —
/// which of them the template reads is still the template's decision.
#[tokio::test]
async fn a_required_file_is_satisfied_by_attaching_one() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let mire = Harness::start(&[(
        "transcribe.yaml",
        requires_upload_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (_, stored) = mire.upload("clip.wav", b"RIFF").await;

    let (status, _, body) = mire
        .call(json!({
            "profile": "transcribe",
            "prompt": "transcribe this",
            "uploads": [stored["id"].as_str().expect("id")],
        }))
        .await;

    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_str::<Value>(body["request"]["body"].as_str().unwrap()).unwrap(),
        json!({"model": "m", "audio": "UklGRg=="})
    );
}

/// Both streaming endpoints settle it before the stream opens: a `422` is more
/// use than a stream whose first event is a failure.
#[tokio::test]
async fn a_required_file_is_a_status_code_rather_than_a_stream_that_fails() {
    let mire = Harness::start(&[(
        "transcribe.yaml",
        requires_upload_profile("https://models.internal/v1/chat/completions"),
    )])
    .await;

    let (status, events) = mire
        .stream(json!({"profile": "transcribe", "prompt": "ping"}))
        .await;
    assert_eq!(status, 422);
    assert!(events.is_empty());

    let (status, events) = mire
        .agent(json!({"profile": "transcribe", "prompt": "ping"}))
        .await;
    assert_eq!(status, 422);
    assert!(events.is_empty());
}

/// The composer greys **Send** rather than letting it produce a `422`, and this
/// is the field it reads to know.
#[tokio::test]
async fn the_profile_listing_says_which_profiles_need_a_file() {
    let mire = Harness::start(&[
        (
            "transcribe.yaml",
            requires_upload_profile("https://models.internal/v1/chat/completions"),
        ),
        ("chat.yaml", openai_profile("https://models.internal/v1")),
    ])
    .await;

    let listed = mire.get("/api/profiles").await;
    let profiles = listed["profiles"].as_array().expect("profiles");
    let of = |name: &str| {
        profiles
            .iter()
            .find(|profile| profile["name"] == name)
            .expect("profile")["requiresUpload"]
            .clone()
    };

    assert_eq!(of("transcribe"), json!(true));
    // Absent from the file is `false`, not missing: the UI reads a boolean.
    assert_eq!(of("chat"), json!(false));
}

/// Agent mode re-renders the whole body every turn, so a file the first turn
/// carried is a file the second one carries too.
#[tokio::test]
async fn an_attachment_is_carried_on_every_turn_of_a_loop() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(openai_response()))
        .mount(&server)
        .await;

    let mire = Harness::start(&[(
        "vision.yaml",
        vision_profile(&format!("{}/v1/chat/completions", server.uri())),
    )])
    .await;

    let (_, stored) = mire.upload("shot.png", &[0x89, b'P', b'N', b'G']).await;

    let (status, events) = mire
        .agent(json!({
            "profile": "vision",
            "prompt": "what is this",
            "uploads": [stored["id"].as_str().expect("id")],
        }))
        .await;

    assert_eq!(status, 200);
    assert!(events.iter().any(|(name, _)| name == "done"), "{events:?}");
    for request in server.received_requests().await.unwrap() {
        assert!(
            std::str::from_utf8(&request.body)
                .unwrap()
                .contains("data:image/png;base64,iVBORw=="),
        );
    }
}

// --- multipart requests ------------------------------------------------------

/// A transcription profile: the audio as bytes, the knobs as form fields beside
/// it. This is the shape a whisper-style endpoint actually reads, and none of it
/// is built in Rust — the profile says what the form carries.
fn transcription_profile(url: &str) -> String {
    format!(
        r#"
name: whisper
kind: chat
url: {url}
timeout_ms: 5000
request:
  multipart:
    file:
      upload: '{{{{ uploads[0] }}}}'
    model: whisper-1
    response_format: json
    temperature: 0
    prompt: '{{{{ messages[-1].content }}}}'
decode:
  content: ["$.text"]
  error: ["$.error"]
"#
    )
}

/// The whole point of the feature, end to end: attach an audio file, and what
/// leaves is a `multipart/form-data` a transcriber will accept.
#[tokio::test]
async fn a_multipart_profile_sends_the_file_and_its_knobs_as_form_parts() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "hello there"})))
        .mount(&server)
        .await;

    let mire = Harness::start(&[(
        "whisper.yaml",
        transcription_profile(&format!("{}/v1/audio/transcriptions", server.uri())),
    )])
    .await;

    let (_, stored) = mire.upload("meeting.mp3", b"ID3\x04audio").await;

    let (status, _, body) = mire
        .call(json!({
            "profile": "whisper",
            "prompt": "the speakers are French",
            "uploads": [stored["id"].as_str().expect("id")],
        }))
        .await;
    assert_eq!(status, 200);

    // Decoded like any other answer: `kind: chat` plus a `$.text` cascade is all
    // a transcriber needs, so nothing new had to be invented downstream.
    assert_eq!(body["response"]["decoded"]["content"], "hello there");

    // What actually went out.
    let sent = &server.received_requests().await.unwrap()[0];
    let content_type = sent
        .headers
        .get("content-type")
        .expect("content-type")
        .to_str()
        .expect("text");
    assert!(
        content_type.starts_with("multipart/form-data; boundary="),
        "{content_type}"
    );
    // Exactly one, which is the trap: `reqwest` appends its own, so a
    // `content-type` surviving the header pass would go out beside it and a
    // server reading the first would be told `json` about a form.
    assert_eq!(sent.headers.get_all("content-type").iter().count(), 1);

    let form = String::from_utf8_lossy(&sent.body);
    assert!(form.contains(r#"name="file""#), "{form}");
    assert!(form.contains(r#"filename="meeting.mp3""#), "{form}");
    assert!(form.contains("audio/mpeg"), "{form}");
    // The bytes themselves, not a description of them.
    assert!(form.contains("ID3\u{4}audio"), "{form}");
    // And the knobs, rendered: a literal, a number written as one, and a
    // template reading the call.
    assert!(form.contains(r#"name="model""#), "{form}");
    assert!(form.contains("whisper-1"), "{form}");
    assert!(form.contains(r#"name="temperature""#), "{form}");
    assert!(form.contains(r#"name="prompt""#), "{form}");
    assert!(form.contains("the speakers are French"), "{form}");

    // Order is the profile's, not the alphabet's.
    let file_at = form.find(r#"name="file""#).expect("file part");
    let model_at = form.find(r#"name="model""#).expect("model part");
    assert!(file_at < model_at, "{form}");
}

/// The other half of a transcriber: there is nothing to type.
///
/// `has_prompt: false` is the profile saying so, and both ends of that have to
/// hold — the listing carries it, so the composer knows to drop its box, and a
/// call arriving with no message at all is a call like any other rather than an
/// empty request nobody meant to send.
#[tokio::test]
async fn a_profile_declaring_no_prompt_is_called_with_nothing_typed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "hello there"})))
        .mount(&server)
        .await;

    let profile = transcription_profile(&format!("{}/v1/audio/transcriptions", server.uri()))
        .replace("kind: chat", "kind: chat\nhas_prompt: false")
        // The vocabulary hint is a knob rather than a sentence once the box is
        // gone, which is exactly what `params` is for.
        .replace(
            r"prompt: '{{ messages[-1].content }}'",
            r#"prompt: '{{ params.prompt | default("") }}'"#,
        );
    let mire = Harness::start(&[("whisper.yaml", profile)]).await;

    let listed = mire.get("/api/profiles").await;
    assert_eq!(listed["profiles"][0]["hasPrompt"], false);

    let (_, stored) = mire.upload("meeting.mp3", b"ID3\x04audio").await;

    // No `prompt`, no `messages`: the file is the whole of the signal going in.
    let (status, _, body) = mire
        .call(json!({
            "profile": "whisper",
            "uploads": [stored["id"].as_str().expect("id")],
        }))
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["response"]["decoded"]["content"], "hello there");

    let sent = server.received_requests().await.unwrap();
    let form = String::from_utf8_lossy(&sent[0].body);
    assert!(form.contains(r#"filename="meeting.mp3""#), "{form}");
    // The hint field still goes out, empty, because the profile still declares
    // it — a field nobody set is not a field that disappears.
    assert!(form.contains(r#"name="prompt""#), "{form}");
}

/// Nothing declared is a profile with a question to ask, which is every profile
/// written before the field existed.
#[tokio::test]
async fn a_profile_takes_a_prompt_unless_it_says_otherwise() {
    let harness = Harness::start(&[(
        "chat.yaml",
        openai_profile("https://models.internal/v1/chat/completions"),
    )])
    .await;

    let body = harness.get("/api/profiles").await;
    assert_eq!(body["profiles"][0]["hasPrompt"], true);
}

/// The trace has to say what went out, and for a form that is the parts — not a
/// body, which a form does not have, and not the bytes, which nobody can read.
#[tokio::test]
async fn the_trace_of_a_form_names_its_parts_and_carries_none_of_the_bytes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "ok"})))
        .mount(&server)
        .await;

    let mire = Harness::start(&[(
        "whisper.yaml",
        transcription_profile(&format!("{}/v1/audio/transcriptions", server.uri())),
    )])
    .await;

    let (_, stored) = mire.upload("meeting.mp3", b"ID3\x04audio").await;
    let (status, _, body) = mire
        .call(json!({
            "profile": "whisper",
            "prompt": "ping",
            "uploads": [stored["id"].as_str().expect("id")],
        }))
        .await;
    assert_eq!(status, 200);

    // A form is not text, so there is no body to show.
    assert_eq!(body["request"]["body"], "");

    let parts = body["request"]["parts"].as_array().expect("parts");
    let file = &parts[0];
    assert_eq!(file["field"], "file");
    assert_eq!(file["filename"], "meeting.mp3");
    assert_eq!(file["contentType"], "audio/mpeg");
    assert_eq!(file["size"], 9);
    assert_eq!(file["uploadId"], stored["id"]);
    // Named, never repeated: a panel carrying the bytes beside them would cost
    // everything and tell the reader nothing.
    assert!(file.get("value").is_none(), "{file}");

    let model = &parts[1];
    assert_eq!(model["field"], "model");
    assert_eq!(model["value"], "whisper-1");
    assert!(model.get("filename").is_none(), "{model}");

    // The `curl` is one somebody can run: `-F` flags, and the file by the path
    // `--uploads` put it at.
    let curl = body["curl"].as_str().expect("curl");
    assert!(curl.contains("-F 'model=whisper-1'"), "{curl}");
    assert!(curl.contains("-F 'file=@"), "{curl}");
    assert!(curl.contains("meeting.mp3;type=audio/mpeg"), "{curl}");
    assert!(!curl.contains("--data-raw"), "{curl}");
}

/// A form field naming a file nobody attached is refused before anything leaves,
/// the same way a hook's is. A `422` from the endpoint about a field nobody in
/// the profile ever mentioned is exactly the afternoon this avoids.
#[tokio::test]
async fn a_form_with_nothing_attached_is_refused_before_anything_is_sent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "ok"})))
        .mount(&server)
        .await;

    let mire = Harness::start(&[(
        "whisper.yaml",
        transcription_profile(&format!("{}/v1/audio/transcriptions", server.uri())),
    )])
    .await;

    let (status, _, body) = mire
        .call(json!({"profile": "whisper", "prompt": "transcribe this"}))
        .await;

    assert_eq!(status, 422);
    assert_eq!(body["code"], "multipart_error");
    let message = body["message"].as_str().expect("message");
    assert!(message.contains("nothing was attached"), "{message}");
    assert!(message.contains("`file`"), "{message}");

    // And nothing left: the endpoint was never asked to explain our own profile
    // back to us.
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// A diarisation-shaped profile: the audio, plus a JSON blob beside it. The
/// second is what `type:` on a text field exists for, and the overrides on the
/// file part are for a store that could not classify the extension.
#[tokio::test]
async fn a_text_part_can_declare_its_own_content_type() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/diarize"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "ok"})))
        .mount(&server)
        .await;

    let profile = format!(
        r#"
name: pyannote
kind: chat
url: {}/diarize
timeout_ms: 5000
request:
  multipart:
    file:
      upload: '{{{{ uploads[0] }}}}'
      type: audio/wav
      filename: input.wav
    config:
      text: '{{{{ params.config | tojson }}}}'
      type: application/json
decode:
  content: ["$.text"]
"#,
        server.uri()
    );

    let mire = Harness::start(&[("pyannote.yaml", profile)]).await;
    let (_, stored) = mire.upload("recording", b"RIFF").await;

    let (status, _, _) = mire
        .call(json!({
            "profile": "pyannote",
            "prompt": "who spoke",
            "uploads": [stored["id"].as_str().expect("id")],
            "params": {"config": {"num_speakers": 2}},
        }))
        .await;
    assert_eq!(status, 200);

    let sent = server.received_requests().await.unwrap();
    let form = String::from_utf8_lossy(&sent[0].body);
    assert!(form.contains(r#"filename="input.wav""#), "{form}");
    assert!(form.contains("audio/wav"), "{form}");
    assert!(form.contains("application/json"), "{form}");
    assert!(form.contains(r#"{"num_speakers":2}"#), "{form}");
}

/// Everything the JSON path already had still works when the body is a form: the
/// credential goes where the provider says, and it never comes back out.
#[tokio::test]
async fn a_form_is_authenticated_and_redacted_like_any_other_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/audio/transcriptions"))
        .and(header("authorization", "Bearer s3cr3t-token"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"text": "ok"})))
        .mount(&server)
        .await;

    let mire = Harness::start(&[
        (
            "auth.yaml",
            "providers:\n  - name: pasted\n    kind: token\n".to_owned(),
        ),
        (
            "whisper.yaml",
            transcription_profile(&format!("{}/v1/audio/transcriptions", server.uri())),
        ),
    ])
    .await;

    let (_, stored) = mire.upload("meeting.mp3", b"ID3").await;
    let (status, _, body) = mire
        .call(json!({
            "profile": "whisper",
            "auth": "pasted",
            "prompt": "ping",
            "token": "s3cr3t-token",
            "uploads": [stored["id"].as_str().expect("id")],
        }))
        .await;

    // The endpoint only answers a request carrying the credential, so a `200`
    // here is the credential having made it onto a form request.
    assert_eq!(status, 200);
    assert_eq!(body["response"]["http"]["status"], 200);

    let curl = body["curl"].as_str().expect("curl");
    assert!(!curl.contains("s3cr3t-token"), "{curl}");
    assert!(!body.to_string().contains("s3cr3t-token"), "{body}");
}
