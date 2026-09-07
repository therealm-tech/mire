//! Serving one resource in the representation the caller asked for.
//!
//! A model is one thing at one URL, and `models/qwen3.yaml` on disk and the JSON
//! the UI reads are two ways of writing it rather than two resources. So the
//! choice is `Accept`'s, not the path's — and `curl … > models/qwen3.yaml` still
//! works, which is the whole reason the YAML representation exists.

use aide::OperationOutput;
use aide::generate::GenContext;
use aide::openapi::{MediaType, Operation, Response};
use axum::Json;
use axum::http::{HeaderMap, header};
use axum::response::IntoResponse;
use schemars::JsonSchema;
use serde::Serialize;

/// The media types that mean "the YAML one".
///
/// `application/yaml` is the registered one (RFC 9512); the other two predate it
/// and are what half the tooling still sends.
const YAML_TYPES: [&str; 3] = ["application/yaml", "text/yaml", "application/x-yaml"];

/// Whether the caller asked for YAML rather than the JSON everything else takes.
///
/// Deliberately not a full `Accept` negotiation: this answers two shapes, and
/// the only question worth asking is whether YAML was named outright. `*/*` —
/// what a browser and a bare `curl` send — is not naming it, so it lands on
/// JSON, which is what a client that expressed no preference should get.
#[must_use]
pub fn wants_yaml(headers: &HeaderMap) -> bool {
    let Some(accept) = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };

    accept.split(',').any(|entry| {
        let media_type = entry.split(';').next().unwrap_or_default().trim();
        YAML_TYPES.contains(&media_type)
    })
}

/// One value, as JSON or as a YAML document.
pub enum Negotiated<T> {
    /// The default.
    Json(Json<T>),
    /// What `Accept: application/yaml` asked for, already rendered.
    Yaml(String),
}

impl<T: Serialize> IntoResponse for Negotiated<T> {
    fn into_response(self) -> axum::response::Response {
        match self {
            Self::Json(json) => json.into_response(),
            Self::Yaml(document) => {
                ([(header::CONTENT_TYPE, "application/yaml")], document).into_response()
            }
        }
    }
}

impl<T: JsonSchema + Serialize> OperationOutput for Negotiated<T> {
    type Inner = T;

    fn operation_response(ctx: &mut GenContext, operation: &mut Operation) -> Option<Response> {
        // The JSON half is `Json<T>`'s own answer, schema included; the YAML one
        // is the same value in another notation, so it hangs off the same
        // response rather than pretending to be a second one.
        let mut response = Json::<T>::operation_response(ctx, operation)?;
        response
            .content
            .insert("application/yaml".to_owned(), MediaType::default());
        Some(response)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn accepting(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, value.parse().expect("a header value"));
        headers
    }

    #[test]
    fn a_browser_and_a_bare_curl_get_json() {
        assert!(!wants_yaml(&HeaderMap::new()));
        assert!(!wants_yaml(&accepting("*/*")));
        assert!(!wants_yaml(&accepting(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
        )));
        assert!(!wants_yaml(&accepting("application/json")));
    }

    #[test]
    fn naming_yaml_anywhere_in_the_header_is_asking_for_it() {
        for value in YAML_TYPES {
            assert!(wants_yaml(&accepting(value)), "{value}");
        }
        assert!(wants_yaml(&accepting("application/json, application/yaml")));
        assert!(wants_yaml(&accepting("application/yaml; q=1.0")));
    }
}
