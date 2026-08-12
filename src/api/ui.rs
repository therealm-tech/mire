//! The React UI, embedded in the binary.
//!
//! # Living under an arbitrary prefix
//!
//! A notebook proxy serves you at something like
//! `/notebook/<ns>/<name>/proxy/8787/`, and the bundle has no way to know that at
//! build time. Two halves solve it:
//!
//! * Vite is built with `base: './'`, so every asset URL in `index.html` is
//!   relative;
//! * this module injects `<base href="{prefix}/">` into `index.html` on the way
//!   out, so those relative URLs — and the UI's own `fetch` calls — resolve
//!   against the prefix rather than against the server root.
//!
//! The UI therefore never learns its own prefix, and the same bundle works at
//! `/`, behind a proxy, or anywhere else.
//!
//! In debug builds `rust-embed` reads from `ui/dist` at runtime, so
//! `npm run build` is enough to see a change — no `cargo build` needed.

use axum::http::{StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;

/// The built front end. Populated by `npm run build`; a placeholder page is
/// written by `build.rs` when it has never been built.
#[derive(RustEmbed)]
#[folder = "ui/dist"]
struct Assets;

const INDEX: &str = "index.html";

/// Serves an embedded asset, falling back to `index.html`.
///
/// The fallback is what makes this a single-page app: an unknown path is the
/// UI's business, not a `404`.
pub fn serve(uri: &Uri, base_path: &str) -> Response {
    let path = uri.path().trim_start_matches('/');

    if !path.is_empty() && path != INDEX {
        if let Some(asset) = Assets::get(path) {
            return (
                [
                    (header::CONTENT_TYPE, content_type(path)),
                    (header::CACHE_CONTROL, cache_control(path)),
                ],
                asset.data.into_owned(),
            )
                .into_response();
        }
    }

    index(base_path)
}

/// Renders `index.html` with a `<base href>` matching where we are mounted.
fn index(base_path: &str) -> Response {
    let Some(asset) = Assets::get(INDEX) else {
        return (
            StatusCode::NOT_FOUND,
            "the UI was not built into this binary; the API is still at /docs",
        )
            .into_response();
    };

    let html = String::from_utf8_lossy(&asset.data);
    let tag = format!(r#"<base href="{}/">"#, base_path.trim_end_matches('/'));
    let patched = match html.find("<head>") {
        Some(position) => {
            let (head, tail) = html.split_at(position + "<head>".len());
            format!("{head}\n    {tag}{tail}")
        }
        // No <head> to hook into: better to serve the page unchanged than not
        // at all, and a page without one is not going to be our UI anyway.
        None => html.into_owned(),
    };

    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            // The document names hashed assets, so it must never be the stale part.
            (header::CACHE_CONTROL, "no-cache"),
        ],
        patched,
    )
        .into_response()
}

/// Content type from the extension. Covers what Vite emits; anything else is
/// served as bytes rather than guessed at.
fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        // `.map` is a source map, which is JSON.
        Some("json" | "map") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("woff2") => "font/woff2",
        Some("woff") => "font/woff",
        _ => "application/octet-stream",
    }
}

/// Vite fingerprints everything under `assets/`, so those can be cached forever.
fn cache_control(path: &str) -> &'static str {
    if path.starts_with("assets/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_are_typed_and_fingerprinted_ones_are_cached_forever() {
        assert_eq!(
            content_type("assets/index-abc123.js"),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type("assets/index-abc123.css"),
            "text/css; charset=utf-8"
        );
        assert_eq!(content_type("favicon.ico"), "image/x-icon");
        assert_eq!(content_type("weird.bin"), "application/octet-stream");

        assert_eq!(
            cache_control("assets/index-abc123.js"),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(cache_control("index.html"), "no-cache");
    }
}
