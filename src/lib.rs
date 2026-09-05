//! `mire` — a test pattern for model endpoints.
//!
//! You deploy a model, or you change a route, and you want to know four things:
//! does the endpoint answer, is the auth actually enforced, is the response shaped
//! the way you expect, and does tool calling work. Today that is a copy-pasted
//! `curl`. This is the same thing, reproducible.
//!
//! # Layout
//!
//! * [`model`] — one YAML file per endpoint, read-only.
//! * [`prompt`] — the questions worth keeping, read-only like the models.
//! * [`config`] — the configuration directories, watched and hot-reloaded as one
//!   atomic snapshot (models *and* auth registry together); [`config::layout`]
//!   is what a directory holds, one subdirectory per kind of thing.
//! * [`auth`] — the auth registry, orthogonal to the models so one model can be
//!   replayed on every mode without duplication.
//! * [`render`] — `MiniJinja` templates producing the request body.
//! * [`transport`] — the single place that builds a client and sends a request.
//! * [`decode`] — `JSONPath` cascades turning any response into a normalised shape.
//! * [`script`] — the sandboxed Rhai escape hatch, for shapes the cascades cannot reach.
//! * [`exec`] — the four steps above, wired together.
//! * [`agent`] — the same four steps, in a loop, answering simulated tools.
//! * [`api`] — the HTTP surface the UI talks to.
//! * [`redact`] — credentials, and the guarantee they do not leave the process.
//! * [`uploads`] — the one thing here that writes to disk, and the rules that
//!   keep it from writing anywhere it was not pointed at.
//! * [`vars`] — what a tool call left behind, for the hooks that fire after it.
//!   The rules that put it there are [`mcp::capture`], declared on the server
//!   whose tools produce it.
//!
//! # A note on wire naming
//!
//! `mire`'s own API types are `camelCase`. Two families stay `snake_case` on
//! purpose, because they mirror someone else's format rather than ours:
//! [`model::Model`] (which *is* the YAML document) and [`message::Message`]
//! (which is what templates serialise straight into an OpenAI-shaped body).

pub mod agent;
pub mod api;
pub mod auth;
pub mod config;
pub mod decode;
pub mod error;
pub mod exec;
pub mod issue;
pub mod mcp;
pub mod message;
pub mod model;
pub mod pattern;
pub mod prompt;
pub mod redact;
pub mod render;
pub mod script;
pub mod transport;
pub mod uploads;
pub mod vars;
