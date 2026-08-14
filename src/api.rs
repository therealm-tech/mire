//! The HTTP API, and the `OpenAPI` document that describes it.
//!
//! The browser never talks to a model endpoint directly — it talks to this, and
//! `mire` makes the outbound call. That is what removes CORS from the picture and
//! makes workload identities testable from a web page.
//!
//! Everything — API, docs and the embedded UI — is served under `--base-path`,
//! so running behind a notebook proxy (`/notebook/<ns>/<name>/proxy/8787/`) is a
//! flag, not a workaround. See [`ui`] for how the bundle stays prefix-agnostic.

pub mod dto;
pub mod handlers;
pub mod sse;
pub mod ui;

use std::sync::Arc;

use aide::axum::routing::{get_with, post_with};
use aide::axum::{ApiRouter, IntoApiResponse};
use aide::openapi::{Info, OpenApi};
use axum::extract::DefaultBodyLimit;
use axum::response::Redirect;
use axum::routing::get;
use axum::{Extension, Json, Router};
use tower_http::trace::TraceLayer;

use crate::error::ApiError;
use crate::exec::Runner;

/// What the handlers need.
#[derive(Clone, Debug)]
pub struct AppState {
    /// Profiles, auth registry and HTTP client.
    pub runner: Runner,
    /// Normalised base path: either empty or `/something` with no trailing slash.
    pub base_path: Arc<str>,
    /// Origin the outside world reaches us on, when it cannot be worked out.
    ///
    /// Only the OIDC browser login needs it, and only when the UI's own answer is
    /// wrong — see [`handlers::resolve_redirect_uri`].
    pub public_url: Option<Arc<str>>,
}

/// Normalises a user-supplied base path.
///
/// `""`, `"/"`, `"foo/"` and `"/foo"` all resolve to something the router can
/// nest: the empty string, or `/foo`.
#[must_use]
pub fn normalise_base_path(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

/// The documented product surface, kept apart so `router` stays readable.
fn documented(api: &mut OpenApi) -> Router<AppState> {
    ApiRouter::new()
        .merge(profile_routes())
        .merge(auth_routes())
        .merge(mcp_routes())
        .merge(call_routes())
        .finish_api(api)
}

/// Reading the configuration directory.
fn profile_routes() -> ApiRouter<AppState> {
    ApiRouter::new()
        .api_route(
            "/api/profiles",
            get_with(handlers::list_profiles, |op| {
                op.summary("List profiles and load errors")
                    .description(
                        "Every profile in the directory, plus the files that failed to load \
                         and why. Refreshed by the file watcher.",
                    )
                    .tag("profiles")
                    .response::<200, Json<dto::ProfilesResponse>>()
            }),
        )
        .api_route(
            "/api/profiles/{name}",
            get_with(handlers::get_profile, |op| {
                op.summary("Fetch one profile")
                    .description("The profile exactly as declared in YAML, field names included.")
                    .tag("profiles")
                    .response::<200, Json<crate::profile::Profile>>()
            }),
        )
}

/// Everything about who we are when we call.
fn auth_routes() -> ApiRouter<AppState> {
    ApiRouter::new()
        .api_route(
            "/api/auth",
            get_with(handlers::list_auth, |op| {
                op.summary("List auth providers")
                    .description(
                        "Feeds the auth selector. Never carries a credential; `needsValue` \
                         says when the UI must ask for one.",
                    )
                    .tag("auth")
                    .response::<200, Json<dto::AuthResponse>>()
            }),
        )
        .api_route(
            "/api/auth/{name}/login",
            post_with(handlers::start_login, |op| {
                op.summary("Start a browser login")
                    .description(
                        "For a `kind: oidc_browser` provider. Mints a PKCE verifier and a \
                         single-use state, and returns the authorization URL to open.\n\n\
                         `redirectUri` is where the identity provider will send the browser \
                         back; the UI computes it from `document.baseURI`, because behind a \
                         notebook proxy that is the only place the public URL is known. \
                         `--public-url` overrides it. Whatever is used must be registered \
                         with the identity provider.",
                    )
                    .tag("auth")
                    .response::<200, Json<dto::LoginResponse>>()
            }),
        )
        .api_route(
            "/api/auth/{name}/logout",
            post_with(handlers::logout, |op| {
                op.summary("Forget a browser session")
                    .description(
                        "Drops the tokens `mire` holds. It does not sign you out of the \
                         identity provider, so the next login may complete without a prompt.",
                    )
                    .tag("auth")
                    .response::<200, Json<dto::LogoutResponse>>()
            }),
        )
}

/// The MCP servers agent mode may call for real.
fn mcp_routes() -> ApiRouter<AppState> {
    ApiRouter::new()
        .api_route(
            "/api/mcp",
            get_with(handlers::list_mcp, |op| {
                op.summary("List MCP servers")
                    .description(
                        "Servers declared in `mcp.yaml`, plus the entries that failed to \
                         load. Declared, not contacted — nothing here talks to them.",
                    )
                    .tag("mcp")
                    .response::<200, Json<dto::McpResponse>>()
            }),
        )
        .api_route(
            "/api/mcp/{name}/tools",
            get_with(handlers::list_mcp_tools, |op| {
                op.summary("Ask a server what it offers")
                    .description(
                        "Really calls `tools/list`, with whatever auth the server \
                         declares. The quickest way to answer \"is this MCP endpoint up, \
                         and does my credential get me in?\" without running a model.\n\n\
                         Tools whose `x-mcp-header` annotations are invalid are left out, \
                         which is what the specification requires of a client.",
                    )
                    .tag("mcp")
                    .response::<200, Json<dto::McpToolsResponse>>()
            }),
        )
        .api_route(
            "/api/mcp/{name}/upload",
            post_with(handlers::upload_to_mcp, |op| {
                op.summary("Put a file where a server's tools can read it")
                    .description(
                        "Sends one `multipart/form-data` file to the `upload:` target the \
                         server declares, as whoever that server authenticates as, and \
                         answers with the identifier its target gave back.\n\n\
                         This exists because MCP cannot carry a file: tool arguments are \
                         JSON in a model's context window, and a few megabytes of base64 \
                         is not. The bytes therefore never go near the model — it is \
                         handed the identifier, which is what its tools take.\n\n\
                         A server with no `upload:` answers `422`: there is nowhere to \
                         put one.",
                    )
                    .tag("mcp")
                    .response::<200, Json<dto::McpUploadResponse>>()
            }),
        )
        // Scoped to the upload route, which is the only one here that carries
        // bytes. The default 2 MB is a limit on `axum`'s side of a file that the
        // target's own limit is supposed to be the one deciding.
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BYTES))
}

/// How large a file may be on its way through.
///
/// Not a judgement about what an upload target accepts — that is its business,
/// and refusing here would hide its own answer, which is the thing worth seeing.
/// It is a ceiling on what this process will hold in memory for one request.
const MAX_UPLOAD_BYTES: usize = 64 * 1024 * 1024;

/// How large a call body may be.
///
/// The conversation lives in the browser, so the whole history — attachments
/// included — travels in the body of every request. `axum` defaults to 2 MB,
/// which the first screenshot somebody attaches goes straight through, and a
/// `413` with nothing else on it is a cryptic way to find that out. The UI caps
/// what it will attach well below this; the ceiling is here so that cap is the
/// one that speaks.
const MAX_CALL_BODY_BYTES: usize = 64 * 1024 * 1024;

/// Actually driving an endpoint.
fn call_routes() -> ApiRouter<AppState> {
    ApiRouter::new()
        .api_route(
            "/api/call",
            post_with(handlers::call, |op| {
                op.summary("Run one call")
                    .description(
                        "Renders the request, authenticates it, sends it and decodes the \
                         answer. The rendered request and its curl equivalent come back \
                         alongside the response, credentials masked. A 4xx or 5xx from the \
                         endpoint is a successful call — read `response.http.status`.\n\n\
                         For `kind: embedding`, the answer is judged on its shape and \
                         vectors are summarised, never returned whole; `includeVectors` \
                         opts into the full payload and `repeat: 2` enables the \
                         determinism check.",
                    )
                    .tag("call")
                    .response::<200, Json<crate::exec::CallOutcome>>()
            }),
        )
        .api_route(
            "/api/call/stream",
            post_with(handlers::call_stream, |op| {
                op.summary("Run one call, streamed")
                    .description(
                        "Same call as `POST /api/call`, read chunk by chunk. The template is \
                         told `stream` is true — write `\"stream\": {{ stream }}` in it, since \
                         nothing here makes an endpoint chunk its answer on its own — and \
                         `decode.delta` says where the text sits inside a chunk.\n\n\
                         Emits an `open` event as soon as the response head arrives (a `401` \
                         shows up there, before any body), one `delta` per chunk carrying \
                         text, then a `done` event holding the same outcome the \
                         non-streaming endpoint returns. `failed` means the call could not \
                         be made.\n\n\
                         `response.http.ttftMs` is time to first *token* — the first chunk \
                         that carried text, not the first byte. `response.stream` says how \
                         many chunks arrived and whether the endpoint ended the stream or \
                         it merely stopped.",
                    )
                    .tag("call")
                    .response::<200, String>()
            }),
        )
        .api_route(
            "/api/agent",
            post_with(handlers::agent, |op| {
                op.summary("Run a chat profile in a loop")
                    .description(
                        "Renders, calls, decodes; if the profile's stop condition is not \
                         met, answers the tool calls with their simulated results and goes \
                         round again.\n\n\
                         Streams server-sent events: one `turn` event per turn as it \
                         happens, then a single `done` event carrying the whole trace and \
                         how the loop ended. A `failed` event means the run could not \
                         continue — a loop that ended badly is a `stop` outcome inside \
                         `done`, not a failure.\n\n\
                         Nothing is ever executed: the tools are simulated, and what is \
                         being checked is that the model emits calls matching their schema \
                         and knows what to do with a result.",
                    )
                    .tag("call")
                    .response::<200, String>()
            }),
        )
        // Last, so it covers the three routes above and nothing else.
        .layer(DefaultBodyLimit::max(MAX_CALL_BODY_BYTES))
}

/// Builds the whole application: API, `OpenAPI` document and Scalar reference.
pub fn router(state: AppState) -> Router {
    // Emit reusable `#/components/schemas` refs rather than inlining every type.
    aide::generate::extract_schemas(true);

    let mut api = OpenApi {
        info: Info {
            title: "mire".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            description: Some(
                "A known signal in, a look at what comes out. Drive a model endpoint \
                 through a profile, on any auth mode, and see exactly what was sent."
                    .to_owned(),
            ),
            ..Info::default()
        },
        ..OpenApi::default()
    };

    let base_path = state.base_path.to_string();

    let api_and_ui = documented(&mut api)
        // Ops plumbing stays out of the documented product surface.
        .route("/healthz", get(handlers::healthz))
        .route("/openapi.json", get(serve_openapi))
        .route("/docs", get(handlers::docs))
        // A browser destination, not an API endpoint: it answers HTML to a human
        // who did not choose to open this tab. Its path is fixed relative to the
        // prefix because it has to match what the identity provider registered.
        .route(crate::auth::CALLBACK_PATH, get(handlers::callback))
        // Anything else is the UI's: an unknown path is a client-side route, not
        // a 404. These are explicit routes rather than `.fallback()`, because a
        // nested router does not inherit its own fallback and this whole router
        // gets nested under `--base-path`. Static segments still win over the
        // wildcard, so `/api/...` is unaffected.
        .route("/", get(handlers::assets))
        .route("/{*asset}", get(handlers::assets));

    let routed = if base_path.is_empty() {
        api_and_ui
    } else {
        let landing = format!("{base_path}/");
        Router::new()
            .nest(&base_path, api_and_ui)
            // `nest` makes the inner `/` reachable at `{base}` but *not* at
            // `{base}/` — which is exactly the form a notebook proxy hands you.
            // Serving it here rather than redirecting matters: proxies that
            // normalise towards the trailing slash would turn a redirect into a
            // loop.
            .route(&landing, get(handlers::assets))
            // Outside the prefix there is nothing. At the root, a bare 404 is a
            // needlessly cryptic way to say "you forgot the prefix".
            .route(
                "/",
                get(move || std::future::ready(Redirect::temporary(&landing))),
            )
    };

    routed
        .layer(Extension(Arc::new(api)))
        .with_state(state)
        .layer(TraceLayer::new_for_http())
}

/// Serves the generated `OpenAPI` document.
async fn serve_openapi(Extension(api): Extension<Arc<OpenApi>>) -> impl IntoApiResponse {
    Json(api.as_ref().clone())
}

/// Turns a validation failure into a `422` naming the offending fields.
impl From<validator::ValidationErrors> for ApiError {
    fn from(errors: validator::ValidationErrors) -> Self {
        Self::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "invalid_request",
            errors.to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_paths_are_normalised_to_something_nestable() {
        assert_eq!(normalise_base_path(""), "");
        assert_eq!(normalise_base_path("/"), "");
        assert_eq!(normalise_base_path("mire"), "/mire");
        assert_eq!(normalise_base_path("/mire/"), "/mire");
        assert_eq!(
            normalise_base_path("/notebook/team/gleroy/proxy/8787/"),
            "/notebook/team/gleroy/proxy/8787"
        );
    }
}
