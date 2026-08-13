//! The error type the HTTP API answers with.
//!
//! Mapping is deliberate: a `4xx` or `5xx` *from the endpoint under test* is never
//! an API error — it is the result you asked for. Only `mire`'s own failures land
//! here.

use aide::OperationOutput;
use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

use crate::auth::AuthError;
use crate::exec::ExecError;
use crate::render::RenderError;
use crate::transport::TransportError;

/// Error payload. Machine-readable `code`, human-readable `message`, and a
/// `detail` carrying whatever the UI needs to be useful about it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ErrorBody {
    /// Stable identifier, e.g. `unknown_profile`.
    pub code: &'static str,
    /// What went wrong, in one sentence.
    pub message: String,
    /// Extra context. For a template that rendered invalid JSON, this carries the
    /// rendered body and the position — which is the whole reason you are looking.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

/// An API failure, with the status it maps to.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    body: ErrorBody,
}

impl ApiError {
    /// Builds an error with no extra detail.
    #[must_use]
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            body: ErrorBody {
                code,
                message: message.into(),
                detail: None,
            },
        }
    }

    /// Attaches structured detail.
    #[must_use]
    pub fn with_detail(mut self, detail: Value) -> Self {
        self.body.detail = Some(detail);
        self
    }

    /// `404` for something the caller asked for by name.
    #[must_use]
    pub fn not_found(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, code, message)
    }

    /// The stable identifier.
    #[must_use]
    pub fn code(&self) -> &str {
        self.body.code
    }

    /// The human-readable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.body.message
    }
}

impl From<crate::agent::AgentError> for ApiError {
    fn from(error: crate::agent::AgentError) -> Self {
        match error {
            crate::agent::AgentError::NotChat { .. } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "not_a_chat_profile",
                error.to_string(),
            ),
            crate::agent::AgentError::Turn(exec) => Self::from(exec),
            crate::agent::AgentError::Mcp(mcp) => Self::from(mcp),
        }
    }
}

impl From<crate::mcp::McpError> for ApiError {
    fn from(error: crate::mcp::McpError) -> Self {
        use crate::mcp::McpError;

        let message = error.to_string();
        match error {
            McpError::UnknownServer(_) => Self::not_found("unknown_mcp_server", message),
            McpError::Auth(auth) => Self::from(auth),
            // The MCP server is upstream of us, like the model endpoint: not
            // reaching it is a gateway problem, not a bad request.
            McpError::Transport { .. } => {
                Self::new(StatusCode::BAD_GATEWAY, "mcp_unreachable", message)
            }
            McpError::Rpc { .. } | McpError::Protocol { .. } => {
                Self::new(StatusCode::BAD_GATEWAY, "mcp_protocol_error", message)
            }
            // Its own code, deliberately: "we do not speak the same protocol" is
            // the one MCP failure you fix by changing a version rather than by
            // looking at the server, and it used to arrive as an opaque `400`.
            McpError::NoCommonRevision { .. } => {
                Self::new(StatusCode::BAD_GATEWAY, "mcp_no_common_revision", message)
            }
            McpError::SessionLost { .. } => {
                Self::new(StatusCode::BAD_GATEWAY, "mcp_session_lost", message)
            }
            // Neither a failure nor something to retry: the server wants a human
            // in the loop, and a test harness has nobody to ask.
            McpError::InputRequired { .. } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "mcp_input_required",
                message,
            ),
            // A header that will not render is a configuration problem, like a
            // credential source that is not set.
            McpError::Header { .. } => {
                Self::new(StatusCode::BAD_REQUEST, "mcp_header_error", message)
            }
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.body)).into_response()
    }
}

impl OperationOutput for ApiError {
    type Inner = ErrorBody;
}

impl From<ExecError> for ApiError {
    fn from(error: ExecError) -> Self {
        let message = error.to_string();
        match error {
            ExecError::UnknownProfile(_) => Self::not_found("unknown_profile", message),
            ExecError::InvalidHeader { .. } => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_header", message)
            }
            ExecError::Auth(auth) => Self::from(auth),
            ExecError::Render(render) => Self::from(render),
            ExecError::Transport(transport) => Self::from(transport),
        }
    }
}

impl From<AuthError> for ApiError {
    fn from(error: AuthError) -> Self {
        let message = error.to_string();
        match error {
            AuthError::UnknownProvider(_) => Self::not_found("unknown_auth_provider", message),
            AuthError::HostNotAllowed { .. } => {
                Self::new(StatusCode::FORBIDDEN, "host_not_allowed", message)
            }
            AuthError::MissingEnv { .. }
            | AuthError::TokenFile { .. }
            | AuthError::NoCredential { .. }
            | AuthError::InvalidHeaderValue { .. } => {
                Self::new(StatusCode::BAD_REQUEST, "credential_unavailable", message)
            }
            // The identity provider is upstream of us, like the endpoint itself:
            // failing to reach it is a gateway problem, not a bad request.
            AuthError::Discovery { .. } => {
                Self::new(StatusCode::BAD_GATEWAY, "oidc_discovery_failed", message)
            }
            AuthError::TokenExchange { .. } => Self::new(
                StatusCode::BAD_GATEWAY,
                "oidc_token_exchange_failed",
                message,
            ),
            // Not a failure so much as a prerequisite. `409` rather than `401`,
            // because a `401` here would read as "the endpoint rejected you" —
            // and the endpoint has not been asked yet.
            AuthError::NotSignedIn { .. } => {
                Self::new(StatusCode::CONFLICT, "not_signed_in", message)
            }
            AuthError::UnknownLoginState => {
                Self::new(StatusCode::BAD_REQUEST, "unknown_login_state", message)
            }
            AuthError::LoginRefused { .. } => {
                Self::new(StatusCode::BAD_REQUEST, "login_refused", message)
            }
            AuthError::NotABrowserProvider { .. } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "not_a_browser_provider",
                message,
            ),
            AuthError::BadRedirectUri { .. } => {
                Self::new(StatusCode::BAD_REQUEST, "bad_redirect_uri", message)
            }
        }
    }
}

impl From<RenderError> for ApiError {
    fn from(error: RenderError) -> Self {
        let message = error.to_string();
        match error {
            RenderError::Template(_) => {
                Self::new(StatusCode::UNPROCESSABLE_ENTITY, "template_error", message)
            }
            RenderError::Script(_) => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "request_script_error",
                message,
            ),
            RenderError::NoSource => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "no_request_source",
                message,
            ),
            RenderError::InvalidJson {
                line,
                column,
                rendered,
                ..
            } => Self::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "rendered_body_is_not_json",
                message,
            )
            .with_detail(serde_json::json!({
                "line": line,
                "column": column,
                "rendered": rendered,
            })),
        }
    }
}

impl From<TransportError> for ApiError {
    fn from(error: TransportError) -> Self {
        let message = error.to_string();
        match error {
            TransportError::Timeout { .. } => {
                Self::new(StatusCode::GATEWAY_TIMEOUT, "endpoint_timeout", message)
            }
            TransportError::Send(_) => {
                Self::new(StatusCode::BAD_GATEWAY, "endpoint_unreachable", message)
            }
            TransportError::Build(_)
            | TransportError::CaBundleRead { .. }
            | TransportError::CaBundleParse { .. } => Self::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "http_client_error",
                message,
            ),
        }
    }
}
