//! `mire` — a test pattern for model endpoints.
//!
//! You deploy a model, or you change a route, and you want to know four things:
//! does the endpoint answer, is the auth actually enforced, is the response shaped
//! the way you expect, and does tool calling work. Today that is a copy-pasted
//! `curl`. This is the same thing, reproducible.
//!
//! # Layout
//!
//! * [`profile`] — the YAML files, read-only.
//! * [`config`] — the configuration directory, watched and hot-reloaded as one
//!   atomic snapshot (profiles *and* auth registry together).
//! * [`auth`] — the auth registry, orthogonal to the profiles so one model can be
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
//!
//! # A note on wire naming
//!
//! `mire`'s own API types are `camelCase`. Two families stay `snake_case` on
//! purpose, because they mirror someone else's format rather than ours:
//! [`profile::Profile`] (which *is* the YAML document) and [`message::Message`]
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
pub mod profile;
pub mod redact;
pub mod render;
pub mod script;
pub mod transport;
pub mod uploads;
