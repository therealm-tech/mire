//! The React UI, embedded in the binary.
//!
//! # Living under an arbitrary prefix
//!
//! A notebook proxy serves you at something like
//! `/notebook/<ns>/<name>/proxy/8787/`, and the bundle has no way to know that at
//! build time. Vite is built with `base: './'`, so every asset URL in
//! `index.html` is relative, and what those relative URLs resolve against is the
//! whole question. Proxies come in two kinds, and the difference is visible in
//! the request log:
//!
//! * **The prefix is forwarded** (`uri=/notebook/…/proxy/8787/`). Run with
//!   `--base-path`: the routes move under it, and this module injects
//!   `<base href="{prefix}/">` so relative URLs — and the UI's own `fetch` calls
//!   — resolve under the prefix rather than against the server root.
//! * **The prefix is stripped** (`uri=/`). Run with *no* `--base-path`, and no
//!   tag is injected. That absence is the mechanism, not an oversight: relative
//!   URLs then resolve against the document's own URL, which is the browser's
//!   prefixed one, so the asset request comes back through the proxy and gets
//!   stripped in its turn. It requires the document URL to end in a slash, which
//!   is what such proxies redirect to.
//!
//! Injecting `<base href="/">` in the second case is the trap this layout
//! avoids: it would point every asset at the origin root, several layers above
//! us, where somebody else's error page answers in HTML — which a browser
//! reports as a MIME type error on a module script, naming neither the proxy nor
//! the prefix.
//!
//! The UI therefore never learns its own prefix, and the same bundle works at
//! `/`, behind either kind of proxy, or anywhere else.
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

/// Where Vite puts everything it fingerprints.
const ASSETS: &str = "assets/";

/// Serves an embedded asset, falling back to `index.html`.
///
/// The fallback is what makes this a single-page app: an unknown path is the
/// UI's business, not a `404`. Under [`ASSETS`] it is the opposite — see
/// [`missing_asset`].
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

        if path.starts_with(ASSETS) {
            return missing_asset(path);
        }
    }

    index(base_path)
}

/// Answers a request for an asset this binary does not carry.
///
/// Nothing under `assets/` is ever a client-side route: the names are
/// fingerprinted and only our own `index.html` ever asks for them. Handing the
/// SPA fallback back instead would answer a `<script type="module">` request
/// with a page of HTML and a `200`, which the browser reports as a MIME type
/// error — a message that says nothing about the actual fault, a bundle and a
/// document that disagree. A `404` says it plainly.
fn missing_asset(path: &str) -> Response {
    (
        StatusCode::NOT_FOUND,
        [(header::CACHE_CONTROL, "no-store")],
        format!("no such asset: {path} — this binary carries a different UI build"),
    )
        .into_response()
}

/// Renders `index.html`, naming our prefix only when we have one.
///
/// See the module docs: with no `--base-path` the tag is deliberately left out
/// so the bundle's relative URLs resolve against the document's own URL.
fn index(base_path: &str) -> Response {
    let Some(asset) = Assets::get(INDEX) else {
        return (
            StatusCode::NOT_FOUND,
            "the UI was not built into this binary; the API is still at /docs",
        )
            .into_response();
    };

    let html = String::from_utf8_lossy(&asset.data);
    let patched = if base_path.is_empty() {
        html.into_owned()
    } else {
        with_base_tag(&html, base_path)
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

/// Injects `<base href="{base_path}/">` right after `<head>`.
fn with_base_tag(html: &str, base_path: &str) -> String {
    let tag = format!(r#"<base href="{}/">"#, base_path.trim_end_matches('/'));
    match html.find("<head>") {
        Some(position) => {
            let (head, tail) = html.split_at(position + "<head>".len());
            format!("{head}\n    {tag}{tail}")
        }
        // No <head> to hook into: better to serve the page unchanged than not
        // at all, and a page without one is not going to be our UI anyway.
        None => html.to_owned(),
    }
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
    if path.starts_with(ASSETS) {
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

    const HTML: &str = "<!doctype html><html><head>\n<title>mire</title></head></html>";

    #[test]
    fn a_base_path_becomes_a_base_tag_with_exactly_one_trailing_slash() {
        assert!(with_base_tag(HTML, "/mire").contains(r#"<base href="/mire/">"#));
        assert!(with_base_tag(HTML, "/mire/").contains(r#"<base href="/mire/">"#));
    }

    #[test]
    fn a_page_without_a_head_is_served_unchanged_rather_than_not_at_all() {
        assert_eq!(
            with_base_tag("<p>not our UI</p>", "/mire"),
            "<p>not our UI</p>"
        );
    }
}
