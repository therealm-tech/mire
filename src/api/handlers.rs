//! Request handlers. No logic beyond shaping: the work lives in [`crate::exec`].

use std::convert::Infallible;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{Html, Response};
use futures_core::Stream;
use tokio::sync::mpsc;
use tracing::{info, warn};
use url::Url;
use validator::Validate;

use super::AppState;
use super::dto::{
    AgentEvent, AgentRequest, AuthPath, AuthResponse, CallRequest, CallbackQuery, LoginRequest,
    LoginResponse, LogoutResponse, McpPath, McpResponse, McpToolsResponse, ProfilePath,
    ProfilesResponse, StreamEvent,
};
use super::sse::EventStream;
use super::ui;
use crate::agent::{self, AgentError, AgentInput};
use crate::auth::{Auth, AuthError, CALLBACK_PATH, OidcBrowserAuth};
use crate::config::Config;
use crate::error::ApiError;
use crate::exec::{CallInput, CallOutcome};
use crate::mcp::{McpCredentials, McpError, Revision};
use crate::profile::{Profile, ProfileKind};

/// Liveness probe. Deliberately outside the `OpenAPI` document.
pub async fn healthz() -> &'static str {
    "ok"
}

/// Every profile, and every file that failed to load.
pub async fn list_profiles(State(state): State<AppState>) -> Json<ProfilesResponse> {
    Json((&state.runner.config().snapshot().profiles).into())
}

/// One profile, as declared.
///
/// # Errors
///
/// `404` when no profile carries that name.
pub async fn get_profile(
    State(state): State<AppState>,
    Path(path): Path<ProfilePath>,
) -> Result<Json<Profile>, ApiError> {
    state
        .runner
        .config()
        .snapshot()
        .profiles
        .get(&path.name)
        .map(|profile| Json(profile.as_ref().clone()))
        .ok_or_else(|| {
            ApiError::not_found(
                "unknown_profile",
                format!("unknown profile `{}`", path.name),
            )
        })
}

/// Works out the callback the identity provider must redirect to.
///
/// Three sources, in this order, and the order is the whole design:
///
/// 1. **`--public-url`**, when set. An operator who has told us what the world
///    sees is not to be second-guessed.
/// 2. **What the browser says**, sent by the UI from `document.baseURI`. This is
///    the case that matters: inside a Kubeflow notebook the process binds
///    `127.0.0.1:8787` and the browser is at
///    `https://kubeflow.example/notebook/<ns>/<name>/proxy/8787/`. No amount of
///    looking at the socket recovers that.
/// 3. **The request headers**, `X-Forwarded-*` then `Host`, for a plain
///    reverse proxy that sets them.
///
/// A caller-supplied value is checked for shape only — scheme and path. That is
/// deliberate: the binding check is the identity provider's registered redirect
/// URI, and duplicating it here with a second, weaker list would only add a
/// config knob that lies. `mire` binds loopback by default; anyone who can post
/// to this endpoint can already read the answer.
///
/// # Errors
///
/// Fails when a supplied URI is not a usable callback, or when nothing at all
/// identifies the public origin.
pub fn resolve_redirect_uri(
    public_url: Option<&str>,
    base_path: &str,
    supplied: Option<&str>,
    headers: &HeaderMap,
) -> Result<String, AuthError> {
    if let Some(public) = public_url {
        let origin = public.trim_end_matches('/');
        return Ok(format!("{origin}{base_path}{CALLBACK_PATH}"));
    }

    if let Some(raw) = supplied.map(str::trim).filter(|value| !value.is_empty()) {
        let parsed = Url::parse(raw).map_err(|error| AuthError::BadRedirectUri {
            uri: raw.to_owned(),
            reason: error.to_string(),
        })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(AuthError::BadRedirectUri {
                uri: raw.to_owned(),
                reason: format!("`{}` is not an http(s) URL", parsed.scheme()),
            });
        }
        if !parsed.path().ends_with(CALLBACK_PATH) {
            return Err(AuthError::BadRedirectUri {
                uri: raw.to_owned(),
                reason: format!("the path must end in `{CALLBACK_PATH}`"),
            });
        }
        return Ok(parsed.to_string());
    }

    let host = first_header(headers, "x-forwarded-host")
        .or_else(|| first_header(headers, "host"))
        .ok_or_else(|| AuthError::BadRedirectUri {
            uri: String::new(),
            reason: "the request carries no `Host`; set --public-url".to_owned(),
        })?;
    let scheme = first_header(headers, "x-forwarded-proto").unwrap_or("http");
    Ok(format!("{scheme}://{host}{base_path}{CALLBACK_PATH}"))
}

/// The first comma-separated entry of a header, trimmed. Proxies chain these.
fn first_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let value = headers.get(name)?.to_str().ok()?;
    let first = value.split(',').next()?.trim();
    (!first.is_empty()).then_some(first)
}

/// Looks up a provider and insists it signs in through a browser.
fn browser_provider<'a>(config: &'a Config, name: &str) -> Result<&'a OidcBrowserAuth, ApiError> {
    match config.registry.get(name) {
        Some(Auth::OidcBrowser(provider)) => Ok(provider),
        Some(_) => Err(AuthError::NotABrowserProvider {
            provider: name.to_owned(),
        }
        .into()),
        None => Err(AuthError::UnknownProvider(name.to_owned()).into()),
    }
}

/// Starts a browser login and says where to send the browser.
///
/// # Errors
///
/// `404` for an unknown provider, `422` for one that is not a browser login,
/// `400` for an unusable callback, `502` when the identity provider cannot be
/// reached or advertises no authorization endpoint.
pub async fn start_login(
    State(state): State<AppState>,
    Path(path): Path<AuthPath>,
    headers: HeaderMap,
    body: Option<Json<LoginRequest>>,
) -> Result<Json<LoginResponse>, ApiError> {
    let config = state.runner.config().snapshot();
    let provider = browser_provider(&config, &path.name)?;

    let request = body.map(|Json(request)| request).unwrap_or_default();
    let redirect_uri = resolve_redirect_uri(
        state.public_url.as_deref(),
        &state.base_path,
        request.redirect_uri.as_deref(),
        &headers,
    )?;

    let started = provider
        .start_login(&redirect_uri, request.prompt.as_deref())
        .await?;
    info!(provider = %path.name, %redirect_uri, "login started");

    Ok(Json(LoginResponse {
        authorization_url: started.url.to_string(),
        redirect_uri,
        state: started.state,
    }))
}

/// Forgets a session. Does not sign anyone out of the identity provider — the
/// next login may well come straight back, which is worth knowing when you are
/// trying to test as somebody else.
///
/// # Errors
///
/// `404` for an unknown provider, `422` for one that is not a browser login.
pub async fn logout(
    State(state): State<AppState>,
    Path(path): Path<AuthPath>,
) -> Result<Json<LogoutResponse>, ApiError> {
    let config = state.runner.config().snapshot();
    let provider = browser_provider(&config, &path.name)?;
    let signed_out = provider.sessions().clear(&path.name);
    info!(provider = %path.name, signed_out, "signed out");
    Ok(Json(LogoutResponse { signed_out }))
}

/// Where the identity provider sends the browser back.
///
/// Answers HTML rather than JSON: a human is looking at this, in a tab they did
/// not choose to open. Kept out of the `OpenAPI` document for the same reason —
/// it is a browser destination, not part of the API surface.
pub async fn callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
) -> Html<String> {
    match complete_login(&state, query).await {
        Ok(provider) => Html(callback_page(
            "Signed in",
            &format!("`{provider}` has a session. You can close this tab."),
            None,
            AutoClose::Yes,
        )),
        Err(failure) => {
            let message = failure.error.to_string();
            warn!(error = %message, "browser login failed");

            // Recorded against the provider so the auth panel can say what went
            // wrong, rather than only that a tab closed.
            if let Some(provider) = &failure.provider {
                state.runner.config().sessions().fail(provider, &message);
            }

            Html(callback_page(
                "Sign-in failed",
                &message,
                Some(&state.base_path),
                // Never close on a failure: this text is the only place the
                // reason exists, and a page that closes itself after a second
                // is a page nobody has read.
                AutoClose::No,
            ))
        }
    }
}

/// A login that did not complete, and who it was for when that is known.
struct LoginFailure {
    provider: Option<String>,
    error: AuthError,
}

impl LoginFailure {
    fn of(provider: &str, error: AuthError) -> Self {
        Self {
            provider: Some(provider.to_owned()),
            error,
        }
    }
}

/// The half of the callback that can fail, so the handler above stays about HTML.
async fn complete_login(state: &AppState, query: CallbackQuery) -> Result<String, LoginFailure> {
    let config = state.runner.config().snapshot();

    // The state is what identifies the attempt, so it is read before anything
    // else — including before believing an `error` that claims to be about it.
    let pending = query
        .state
        .as_deref()
        .and_then(|value| state.runner.config().sessions().take(value))
        .ok_or(LoginFailure {
            provider: None,
            error: AuthError::UnknownLoginState,
        })?;

    if let Some(error) = query.error {
        let message = match query.error_description {
            Some(description) => format!("{error}: {description}"),
            None => error,
        };
        return Err(LoginFailure::of(
            &pending.provider,
            AuthError::LoginRefused {
                provider: pending.provider.clone(),
                message,
            },
        ));
    }

    let code = query.code.ok_or_else(|| {
        LoginFailure::of(
            &pending.provider,
            AuthError::LoginRefused {
                provider: pending.provider.clone(),
                message: "the identity provider came back with neither a code nor an error"
                    .to_owned(),
            },
        )
    })?;

    let Some(Auth::OidcBrowser(provider)) = config.registry.get(&pending.provider) else {
        // The provider was renamed or removed while the browser was away.
        return Err(LoginFailure::of(
            &pending.provider,
            AuthError::UnknownProvider(pending.provider.clone()),
        ));
    };

    provider
        .complete_login(&pending, &code)
        .await
        .map_err(|error| LoginFailure::of(&pending.provider, error))?;
    Ok(pending.provider)
}

/// Whether the callback page should close itself once it has been seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoClose {
    /// It worked; the panel already knows. Get out of the way.
    Yes,
    /// It did not; this page is the only account of why. Stay.
    No,
}

/// The page the browser lands on. Self-contained, and never shows a token.
fn callback_page(
    title: &str,
    message: &str,
    back_to: Option<&str>,
    auto_close: AutoClose,
) -> String {
    let back = back_to.map_or_else(String::new, |base| {
        format!(
            r#"<p><a href="{base}/">Back to mire</a> — the panel there has the same message.</p>"#
        )
    });
    let script = if auto_close == AutoClose::Yes {
        // Opened as a popup by the auth panel, which is polling for the result.
        // If it was opened some other way, closing silently fails and the text
        // above stands.
        "<script>setTimeout(() => window.close(), 1200);</script>"
    } else {
        ""
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} — mire</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font: 15px/1.5 ui-sans-serif, system-ui, sans-serif; margin: 0;
         display: grid; place-items: center; min-height: 100vh; padding: 1rem; }}
  main {{ max-width: 32rem; }}
  h1 {{ font-size: 1.1rem; margin: 0 0 .5rem; }}
  p {{ margin: .5rem 0; opacity: .8; }}
</style>
</head>
<body>
<main>
<h1>{title}</h1>
<p>{message}</p>
{back}
</main>
{script}
</body>
</html>"#,
        title = escape(title),
        message = escape(message),
    )
}

/// Minimal HTML escaping. The only untrusted text here is an identity provider's
/// error description, which is exactly the kind of thing that arrives with a
/// quote in it.
fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Every auth provider the UI can offer.
///
/// Session status is stitched in here rather than stored in the descriptor: the
/// registry is rebuilt on every config reload and sessions are not, so the only
/// place the two are consistent is at read time.
pub async fn list_auth(State(state): State<AppState>) -> Json<AuthResponse> {
    let config = state.runner.config().snapshot();
    let sessions = state.runner.config().sessions();

    let providers = config
        .registry
        .descriptors()
        .iter()
        .map(|descriptor| {
            let mut descriptor = descriptor.clone();
            if descriptor.needs_login {
                descriptor.session = sessions.view(&descriptor.name);
                descriptor.last_error = sessions.last_failure(&descriptor.name);
            }
            descriptor
        })
        .collect();

    Json(AuthResponse {
        providers,
        issues: config.registry.issues().to_vec(),
    })
}

/// Every MCP server declared, and the entries that did not load.
pub async fn list_mcp(State(state): State<AppState>) -> Json<McpResponse> {
    let config = state.runner.config().snapshot();
    Json(McpResponse {
        servers: config.mcp.descriptors().to_vec(),
        // Newest first: that is the one a reader is looking for, and the one a
        // selector should list at the top.
        revisions: Revision::ALL.iter().rev().copied().collect(),
        issues: config.mcp.issues().to_vec(),
    })
}

/// Asks a server what it currently offers.
///
/// This really talks to it, with the auth the server declares — which makes it
/// the quickest way to answer "is this MCP endpoint up, does my credential get me
/// in, and which revision are we actually speaking?" without running a model at
/// all.
///
/// # Errors
///
/// `404` for an unknown server, `502` when it cannot be reached, shares no
/// protocol revision, or answers something that is not a tool listing.
pub async fn list_mcp_tools(
    State(state): State<AppState>,
    Path(path): Path<McpPath>,
) -> Result<Json<McpToolsResponse>, ApiError> {
    let config = state.runner.config().snapshot();
    let client = config
        .mcp
        .get(&path.name)
        .ok_or_else(|| McpError::UnknownServer(path.name.clone()))?;

    // Resolves the server's `auth:` provider *and* whatever its header templates
    // name, which is what makes this endpoint answer the question it is for:
    // "does my credential actually get me in?"
    let credentials = McpCredentials::resolve(&config.registry, client.server()).await?;

    // Settled before the listing rather than read after it, so the revision is
    // reported even when the negotiation succeeded and the listing then failed —
    // which is precisely the case worth telling apart.
    let protocol = client.session(&credentials).await?;
    let tools = client.list_tools(&credentials).await?;

    Ok(Json(McpToolsResponse {
        server: path.name,
        protocol,
        tools,
    }))
}

/// Runs one call.
///
/// # Errors
///
/// See [`ApiError`]. A `4xx`/`5xx` *from the endpoint* is not one of them.
pub async fn call(
    State(state): State<AppState>,
    Json(request): Json<CallRequest>,
) -> Result<Json<CallOutcome>, ApiError> {
    request.validate()?;
    Ok(Json(state.runner.call(request.into()).await?))
}

/// Runs one call, streaming the answer as it arrives.
///
/// Same checks up front as [`agent`], and for the same reason: a `404` beats a
/// stream whose first event is a failure. Once the request is on the wire,
/// everything — including a broken stream — comes back as an event.
///
/// # Errors
///
/// `404` for an unknown profile, `422` for an embedding profile.
pub async fn call_stream(
    State(state): State<AppState>,
    Json(request): Json<CallRequest>,
) -> Result<EventStream<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    request.validate()?;

    let profile = state
        .runner
        .config()
        .snapshot()
        .profiles
        .get(&request.profile)
        .cloned()
        .ok_or_else(|| {
            ApiError::not_found(
                "unknown_profile",
                format!("unknown profile `{}`", request.profile),
            )
        })?;
    if profile.kind != ProfileKind::Chat {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_a_chat_profile",
            format!(
                "profile `{}` is `kind: embedding`; there is nothing to stream",
                profile.name
            ),
        ));
    }
    let (sender, mut receiver) = mpsc::unbounded_channel::<StreamEvent>();
    let runner = state.runner.clone();
    let mut input: CallInput = request.into();
    // This endpoint *is* the streaming one. Whatever the body said, the template
    // is told to ask for a stream.
    input.stream = true;

    tokio::spawn(async move {
        let live = sender.clone();
        let outcome = runner
            .call_streaming(input, |event| {
                // Unbounded so a slow reader never stalls the endpoint we are
                // measuring: a stalled read would show up as latency that is
                // ours, not the model's.
                let _ = live.send(event.into());
            })
            .await;

        let event = match outcome {
            Ok(outcome) => StreamEvent::Done(Box::new(outcome)),
            Err(error) => {
                let api = ApiError::from(error);
                StreamEvent::Failed {
                    code: api.code().to_owned(),
                    message: api.message().to_owned(),
                }
            }
        };
        let _ = sender.send(event);
    });

    let stream = async_stream::stream! {
        while let Some(event) = receiver.recv().await {
            let name = event.name();
            let sse = Event::default()
                .event(name)
                .json_data(&event)
                .unwrap_or_else(|error| {
                    Event::default()
                        .event("failed")
                        .data(format!(r#"{{"code":"unserialisable_event","message":"{error}"}}"#))
                });
            yield Ok(sse);
        }
    };

    Ok(EventStream::new(
        Sse::new(stream).keep_alive(KeepAlive::default()),
        "an `open` event, then one `delta` per chunk, then `done` or `failed`",
    ))
}

/// Runs an agent loop, streaming one server-sent event per turn.
///
/// Mistakes that can be caught before anything is sent — an unknown profile, an
/// embedding profile — come back as a normal HTTP error, because a `404` is more
/// use than a stream whose first event is a failure. Anything that goes wrong
/// once the loop is running arrives as a `failed` event.
///
/// # Errors
///
/// `404` for an unknown profile, `422` for a profile agent mode cannot run.
pub async fn agent(
    State(state): State<AppState>,
    Json(request): Json<AgentRequest>,
) -> Result<EventStream<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    request.validate()?;

    let profile = state
        .runner
        .config()
        .snapshot()
        .profiles
        .get(&request.call.profile)
        .cloned()
        .ok_or_else(|| {
            ApiError::not_found(
                "unknown_profile",
                format!("unknown profile `{}`", request.call.profile),
            )
        })?;
    if profile.kind != ProfileKind::Chat {
        return Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_a_chat_profile",
            AgentError::NotChat {
                profile: profile.name.clone(),
            }
            .to_string(),
        ));
    }
    // Same reasoning: a server that is not declared is a typo, and a typo
    // deserves a status code rather than a stream that opens and fails.
    // Whether a declared server is *reachable* is a runtime matter, and comes
    // back as a `failed` event like any other upstream failure.
    for server in &profile.mcp {
        if state.runner.config().snapshot().mcp.get(server).is_none() {
            return Err(McpError::UnknownServer(server.clone()).into());
        }
    }

    let (sender, mut receiver) = mpsc::unbounded_channel::<AgentEvent>();
    let runner = state.runner.clone();
    let input: AgentInput = request.into();

    tokio::spawn(async move {
        let updates = sender.clone();
        let outcome = agent::run(&runner, input, |update| {
            let event = match update {
                agent::AgentUpdate::Setup(mcp) => AgentEvent::Setup { mcp: mcp.to_vec() },
                agent::AgentUpdate::Turn(turn) => AgentEvent::Turn(Box::new(turn.clone())),
            };
            // Unbounded so the loop is never blocked by a client that stopped
            // reading; a dropped receiver just means nobody is listening.
            let _ = updates.send(event);
        })
        .await;

        let event = match outcome {
            Ok(trace) => AgentEvent::Done(Box::new(trace)),
            Err(error) => {
                let api = ApiError::from(error);
                AgentEvent::Failed {
                    code: api.code().to_owned(),
                    message: api.message().to_owned(),
                }
            }
        };
        let _ = sender.send(event);
    });

    let stream = async_stream::stream! {
        while let Some(event) = receiver.recv().await {
            let name = event.name();
            let sse = Event::default()
                .event(name)
                .json_data(&event)
                .unwrap_or_else(|error| {
                    Event::default()
                        .event("failed")
                        .data(format!(r#"{{"code":"unserialisable_event","message":"{error}"}}"#))
                });
            yield Ok(sse);
        }
    };

    Ok(EventStream::new(
        Sse::new(stream).keep_alive(KeepAlive::default()),
        "one `turn` event per turn, then a single `done` or `failed` event",
    ))
}

/// Serves the embedded UI, falling back to `index.html` for client-side routes.
pub async fn assets(State(state): State<AppState>, uri: Uri) -> Response {
    ui::serve(&uri, &state.base_path)
}

/// The Scalar API reference.
///
/// The spec URL is *relative* on purpose: `/docs` and `/openapi.json` are
/// siblings, so this resolves correctly no matter what path a proxy mounts us at.
pub async fn docs() -> Html<&'static str> {
    Html(
        r#"<!doctype html>
<html>
  <head>
    <title>mire — API reference</title>
    <meta charset="utf-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1" />
  </head>
  <body>
    <div id="app"></div>
    <script src="https://cdn.jsdelivr.net/npm/@scalar/api-reference"></script>
    <script>
      Scalar.createApiReference('#app', { url: 'openapi.json' })
    </script>
  </body>
</html>
"#,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.insert(
                axum::http::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                value.parse().unwrap(),
            );
        }
        headers
    }

    #[test]
    fn an_explicit_public_url_wins_over_everything() {
        let resolved = resolve_redirect_uri(
            Some("https://kubeflow.example/"),
            "/notebook/team/gleroy/proxy/8787",
            Some("http://127.0.0.1:8787/auth/callback"),
            &headers(&[("host", "127.0.0.1:8787")]),
        )
        .unwrap();

        assert_eq!(
            resolved,
            "https://kubeflow.example/notebook/team/gleroy/proxy/8787/auth/callback"
        );
    }

    #[test]
    fn the_browsers_own_url_is_used_when_nothing_overrides_it() {
        // The case this exists for: the socket says loopback, the browser says
        // Kubeflow, and only the browser is right.
        let supplied = "https://kubeflow.example/notebook/team/gleroy/proxy/8787/auth/callback";
        let resolved = resolve_redirect_uri(
            None,
            "",
            Some(supplied),
            &headers(&[("host", "127.0.0.1:8787")]),
        )
        .unwrap();

        assert_eq!(resolved, supplied);
    }

    #[test]
    fn forwarded_headers_are_the_last_resort() {
        let resolved = resolve_redirect_uri(
            None,
            "/mire",
            None,
            &headers(&[
                ("host", "127.0.0.1:8787"),
                ("x-forwarded-host", "gateway.example"),
                ("x-forwarded-proto", "https"),
            ]),
        )
        .unwrap();

        assert_eq!(resolved, "https://gateway.example/mire/auth/callback");
    }

    #[test]
    fn a_chained_forwarded_header_keeps_the_first_hop() {
        let resolved = resolve_redirect_uri(
            None,
            "",
            None,
            &headers(&[
                ("host", "127.0.0.1:8787"),
                ("x-forwarded-host", "outer.example, inner.example"),
                ("x-forwarded-proto", "https, http"),
            ]),
        )
        .unwrap();

        assert_eq!(resolved, "https://outer.example/auth/callback");
    }

    #[test]
    fn the_host_header_alone_is_enough() {
        let resolved =
            resolve_redirect_uri(None, "", None, &headers(&[("host", "localhost:8787")])).unwrap();
        assert_eq!(resolved, "http://localhost:8787/auth/callback");
    }

    #[test]
    fn a_supplied_callback_has_to_look_like_one() {
        for bad in [
            "javascript:alert(1)",
            "ftp://host/auth/callback",
            "https://host/somewhere-else",
            "nonsense",
        ] {
            let error = resolve_redirect_uri(None, "", Some(bad), &HeaderMap::new()).unwrap_err();
            assert!(
                matches!(error, AuthError::BadRedirectUri { .. }),
                "{bad} should be refused"
            );
        }
    }

    #[test]
    fn an_empty_supplied_callback_falls_through_rather_than_failing() {
        let resolved =
            resolve_redirect_uri(None, "", Some("  "), &headers(&[("host", "h:1")])).unwrap();
        assert_eq!(resolved, "http://h:1/auth/callback");
    }

    #[test]
    fn with_nothing_to_go_on_the_error_says_what_to_set() {
        let error = resolve_redirect_uri(None, "", None, &HeaderMap::new()).unwrap_err();
        assert!(error.to_string().contains("--public-url"), "{error}");
    }

    #[test]
    fn the_callback_page_escapes_what_the_identity_provider_said() {
        let page = callback_page(
            "Sign-in failed",
            r#"<script>"bad"</script>"#,
            Some("/mire"),
            AutoClose::No,
        );

        assert!(!page.contains("<script>\"bad\""), "{page}");
        assert!(page.contains("&lt;script&gt;&quot;bad&quot;"), "{page}");
        assert!(page.contains(r#"<a href="/mire/">"#), "{page}");
    }

    #[test]
    fn a_failure_page_stays_open_and_a_success_page_gets_out_of_the_way() {
        // The reason this exists: a page that closes itself after a second is a
        // page nobody has read, and the failure text is the only account of why.
        let failed = callback_page("Sign-in failed", "nope", Some(""), AutoClose::No);
        assert!(!failed.contains("window.close"), "{failed}");

        let ok = callback_page("Signed in", "done", None, AutoClose::Yes);
        assert!(ok.contains("window.close"), "{ok}");
    }
}
