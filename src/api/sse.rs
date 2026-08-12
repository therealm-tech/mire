//! Server-sent events, described to `OpenAPI`.
//!
//! `axum`'s [`Sse`] is not an `aide` [`OperationOutput`], so a streaming endpoint
//! would otherwise have to drop out of the documented surface. `OpenAPI` cannot
//! really describe an event stream anyway — what it can do is say that one is
//! what comes back, and point at the event shapes. That is what this wrapper
//! does, and it is enough to keep every route in the document.

use aide::OperationOutput;
use aide::generate::GenContext;
use aide::openapi::{Operation, Response};
use axum::response::IntoResponse;
use axum::response::sse::Sse;

/// An SSE response that `aide` will document.
pub struct EventStream<S> {
    inner: Sse<S>,
    /// What the stream carries, for the generated document.
    description: &'static str,
}

impl<S> EventStream<S> {
    /// Wraps a stream, describing what it emits.
    pub fn new(inner: Sse<S>, description: &'static str) -> Self {
        Self { inner, description }
    }
}

impl<S> IntoResponse for EventStream<S>
where
    Sse<S>: IntoResponse,
{
    fn into_response(self) -> axum::response::Response {
        self.inner.into_response()
    }
}

impl<S> OperationOutput for EventStream<S> {
    type Inner = ();

    fn operation_response(_ctx: &mut GenContext, _operation: &mut Operation) -> Option<Response> {
        Some(Response {
            description: "A `text/event-stream` of server-sent events".to_owned(),
            ..Response::default()
        })
    }

    fn inferred_responses(
        ctx: &mut GenContext,
        operation: &mut Operation,
    ) -> Vec<(Option<u16>, Response)> {
        Self::operation_response(ctx, operation)
            .into_iter()
            .map(|response| (Some(200), response))
            .collect()
    }
}

impl<S> std::fmt::Debug for EventStream<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventStream")
            .field("description", &self.description)
            .finish_non_exhaustive()
    }
}
