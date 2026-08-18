//! Templated request headers for an MCP server.
//!
//! `auth:` covers the ordinary case: a named provider puts a credential in the
//! standard place. This covers the rest — an `X-Api-Key`, a tenant header, a
//! scheme nobody else uses — without adding a provider kind per endpoint that
//! does something slightly its own way.
//!
//! Templates see three things. `env` is the process environment. `auth` is the
//! **auth registry**, keyed by provider name, each entry the bare token that
//! provider would produce — so a credential `mire` already knows how to obtain
//! (a rotated file, a `client_credentials` exchange, the browser session you are
//! signed into) can go anywhere in the request, not only where the provider
//! itself would put it:
//!
//! ```yaml
//! headers:
//!   x-api-key: '{{ auth["keycloak-workload"] }}'
//! ```
//!
//! `vars` is what the run's tool calls have captured, per the profile's
//! `agent.capture:` — see [`crate::vars`]. It is what lets a tool that opens a
//! session put that session on every later request:
//!
//! ```yaml
//! headers:
//!   x-session: "{{ vars.session | default('') }}"
//! ```
//!
//! **The `default(...)` there is not decoration.** A server's headers render on
//! *every* request it makes, and the first of those is the `tools/list` at
//! setup — before any tool has been called, and so before anything has been
//! captured. Without a default, a run whose server header names a variable dies
//! negotiating, which is a strange way to find out that a session is opened by a
//! tool. A hook's headers have no such problem: a hook only fires around a call.
//!
//! Reach for `auth:` first when the server takes an ordinary bearer token: it is
//! one word, and it brings the `401`-refresh-and-replay behaviour that a
//! hand-written header cannot have.
//!
//! Two properties matter, and they pull in opposite directions:
//!
//! * **Rendered per request, never cached.** A rotated token is picked up on the
//!   next call, the same way [`crate::auth::TokenAuth`] re-reads its file. A
//!   value resolved once at load would be a credential frozen at startup.
//! * **Compiled at load.** A syntax error names the file and the server when
//!   `mire` starts, not on the first agent run twenty minutes later.
//!
//! # Why a strict environment
//!
//! A missing variable renders as the empty string by default, which here would
//! send `Authorization: Bearer ` — a header that looks present, passes every
//! local check, and fails at the far end with something unhelpful. Undefined is
//! an error instead, naming the variable. `| default(...)` still works when a
//! header really is optional.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use minijinja::{Environment, UndefinedBehavior};
use reqwest::header::HeaderName;
use serde::Serialize;

use super::McpError;
use crate::redact::Secret;
use crate::vars::Captured;

/// Separate from [`crate::render`]'s environment on purpose: body templates rely
/// on undefined being falsy (`{% if tools %}`), and a credential must not.
static ENVIRONMENT: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut environment = Environment::new();
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    environment
});

/// What a header template can see.
#[derive(Debug, Serialize)]
struct Context<'a> {
    /// The process environment, read fresh on every render.
    env: BTreeMap<String, String>,
    /// Bare credentials, by auth provider name. Only the providers this server's
    /// templates actually name are resolved — asking for one has a cost (a token
    /// exchange, a refresh) and a failure mode (not signed in), and neither
    /// belongs to a server that never mentioned it.
    auth: BTreeMap<String, String>,
    /// What the run's tool calls have captured so far. Empty off a run that
    /// captures nothing, and empty *early in* a run that does — see the module
    /// docs for why `| default(...)` is the answer there rather than a looser
    /// undefined.
    vars: &'a Captured,
}

impl<'a> Context<'a> {
    fn current(auth: &BTreeMap<String, Secret>, vars: &'a Captured) -> Self {
        Self {
            env: std::env::vars().collect(),
            auth: auth
                .iter()
                .map(|(name, token)| (name.clone(), token.expose().to_owned()))
                .collect(),
            vars,
        }
    }
}

/// The header templates declared for one server.
#[derive(Debug, Clone, Default)]
pub struct HeaderTemplates {
    entries: Vec<(HeaderName, String)>,
    /// Auth providers the templates name, worked out once at load.
    ///
    /// Read before rendering rather than during it: obtaining a credential is
    /// async and can fail, and `MiniJinja` renders synchronously. Knowing the
    /// list up front is what lets the caller resolve exactly those providers,
    /// and no others.
    providers: BTreeSet<String>,
}

impl HeaderTemplates {
    /// Validates every template and header name.
    ///
    /// # Errors
    ///
    /// Returns a message naming the offending header, for the load issue.
    pub fn compile(declared: &BTreeMap<String, String>) -> Result<Self, String> {
        let mut entries = Vec::with_capacity(declared.len());
        let mut providers = BTreeSet::new();

        for (name, template) in declared {
            let header = HeaderName::try_from(name.to_ascii_lowercase())
                .map_err(|_| format!("`{name}` is not a valid HTTP header name"))?;
            ENVIRONMENT
                .template_from_str(template)
                .map_err(|error| format!("header `{name}`: {error}"))?;
            providers.extend(lookups(template, "auth").into_iter().map(str::to_owned));
            entries.push((header, template.clone()));
        }

        Ok(Self { entries, providers })
    }

    /// Auth providers these templates need resolved before they can render.
    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.providers.iter().map(String::as_str)
    }

    /// The header names, for a listing. **Names only** — a rendered value is
    /// usually a credential, and there is no method here that hands one out.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.iter().map(|(name, _)| name.as_str())
    }

    /// Whether anything is declared at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Renders every header against the current environment.
    ///
    /// Values come back as [`Secret`] regardless of what they hold: the caller
    /// cannot then put one in a log by accident, and the common case really is a
    /// credential.
    ///
    /// # Errors
    ///
    /// Fails when a template references something undefined, or renders a value
    /// that cannot go in a header.
    pub fn render(
        &self,
        server: &str,
        auth: &BTreeMap<String, Secret>,
        vars: &Captured,
    ) -> Result<Vec<(HeaderName, Secret)>, McpError> {
        if self.entries.is_empty() {
            return Ok(Vec::new());
        }

        let context = Context::current(auth, vars);
        let mut rendered = Vec::with_capacity(self.entries.len());

        for (name, template) in &self.entries {
            let value = ENVIRONMENT
                .render_str(template, &context)
                .map_err(|error| McpError::Header {
                    server: server.to_owned(),
                    header: name.to_string(),
                    message: explain(&error, template, &context),
                })?;
            rendered.push((name.clone(), Secret::new(value)));
        }

        Ok(rendered)
    }
}

/// Turns a render failure into something actionable.
///
/// `MiniJinja` says "undefined value" without saying *which*, which for a header
/// whose whole job is to carry a token is the one thing you need. The template is
/// scanned for the lookups it makes — `env`, `auth` and `vars` alike — and the
/// ones that resolved to nothing are named.
///
/// Only **names** are ever emitted. Echoing the template back would be more
/// direct and is exactly the wrong thing: a template may hold a literal
/// credential, and an error message is a place secrets go to be logged forever.
fn explain(error: &minijinja::Error, template: &str, context: &Context) -> String {
    let mut message = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(inner) = source {
        message = inner.to_string();
        source = inner.source();
    }

    let mut missing: Vec<&str> = lookups(template, "env")
        .into_iter()
        .filter(|name| !context.env.contains_key(*name))
        .collect();
    // An auth provider that resolved to nothing is `anonymous`, which is a
    // configuration mistake worth naming as such rather than as "undefined".
    for name in lookups(template, "auth") {
        if !context.auth.contains_key(name) {
            missing.push(name);
        }
    }
    // A variable no tool call has captured *yet*. Named like the rest, because
    // "undefined value" on a header that was supposed to carry a session is the
    // one message that tells you nothing.
    for name in lookups(template, "vars") {
        if !context.vars.contains_key(name) {
            missing.push(name);
        }
    }

    match missing.as_slice() {
        [] => message,
        [one] => format!("{message} — `{one}` is not set"),
        many => format!("{message} — none of {many:?} are set"),
    }
}

/// The names a template reads off `root`: `root.NAME`, `root["NAME"]`,
/// `root['NAME']`.
///
/// Deliberately simple, and it earns its keep twice: it names the missing
/// variable in an error message, and it tells the caller which auth providers to
/// resolve before rendering. A name it misses costs a less specific error, or a
/// provider that renders as undefined and is then reported as one — never a
/// silently empty credential.
///
/// `pub(super)` so a hook's `url:` and `json:` can name their missing variables
/// the same way, rather than growing a second scanner that finds slightly
/// different names.
pub(super) fn lookups<'a>(template: &'a str, root: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut rest = template;
    let width = root.len();

    while let Some(at) = rest.find(root) {
        let after = &rest[at + width..];
        let (name, consumed) = match after.as_bytes().first() {
            Some(b'.') => {
                let name = &after[1..];
                let end = name
                    .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                    .unwrap_or(name.len());
                (&name[..end], width + 1 + end)
            }
            // The bracket form is the one to write for a provider name, since
            // `auth.keycloak-workload` parses as a subtraction rather than a name.
            Some(b'[') => {
                let quoted = &after[1..];
                let quote = quoted.chars().next().filter(|c| *c == '"' || *c == '\'');
                match quote.and_then(|q| quoted[1..].find(q).map(|end| (q, end))) {
                    Some((_, end)) => (&quoted[1..=end], width + 2 + end),
                    None => ("", width),
                }
            }
            _ => ("", width),
        };

        if !name.is_empty() && !found.contains(&name) {
            found.push(name);
        }
        rest = &rest[at + consumed.min(rest.len() - at)..];
    }

    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn declared(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(name, template)| ((*name).to_owned(), (*template).to_owned()))
            .collect()
    }

    #[test]
    fn a_literal_header_needs_no_template_at_all() {
        let templates = HeaderTemplates::compile(&declared(&[("x-tenant", "acme")])).unwrap();
        let rendered = templates
            .render("files", &BTreeMap::new(), &Captured::new())
            .unwrap();

        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].0.as_str(), "x-tenant");
        assert_eq!(rendered[0].1.expose(), "acme");
    }

    #[test]
    fn a_template_reads_the_environment_when_it_renders() {
        // Set by the test harness rather than the process: `unsafe_code` is
        // forbidden, so this uses a variable that is always present instead.
        let templates =
            HeaderTemplates::compile(&declared(&[("authorization", "Bearer {{ env.PATH }}")]))
                .unwrap();

        let rendered = templates
            .render("files", &BTreeMap::new(), &Captured::new())
            .unwrap();
        let value = rendered[0].1.expose();
        assert!(value.starts_with("Bearer /"), "{value}");
    }

    #[test]
    fn a_missing_variable_is_an_error_rather_than_an_empty_credential() {
        let templates = HeaderTemplates::compile(&declared(&[(
            "authorization",
            "Bearer {{ env.DEFINITELY_NOT_SET_ANYWHERE }}",
        )]))
        .unwrap();

        let error = templates
            .render("files", &BTreeMap::new(), &Captured::new())
            .unwrap_err();
        let message = error.to_string();
        // The trap this avoids: `Authorization: Bearer ` is a header that looks
        // present everywhere except at the far end.
        assert!(message.contains("DEFINITELY_NOT_SET_ANYWHERE"), "{message}");
        assert!(message.contains("files"), "{message}");
        assert!(message.contains("authorization"), "{message}");
    }

    #[test]
    fn the_lookups_a_template_makes_are_found_in_every_spelling() {
        assert_eq!(lookups("Bearer {{ env.TOKEN }}", "env"), vec!["TOKEN"]);
        assert_eq!(lookups(r#"{{ env["A_B"] }}"#, "env"), vec!["A_B"]);
        assert_eq!(lookups("{{ env['C'] }}", "env"), vec!["C"]);
        assert_eq!(
            lookups("{{ env.ONE }}-{{ env.TWO }}-{{ env.ONE }}", "env"),
            vec!["ONE", "TWO"]
        );
        assert!(lookups("no lookups here", "env").is_empty());
        // A bare `env` reads the whole map; there is no name to report.
        assert!(lookups("{{ env }}", "env").is_empty());
    }

    #[test]
    fn a_header_carries_what_the_run_captured() {
        let templates =
            HeaderTemplates::compile(&declared(&[("x-session", "{{ vars.session }}")])).unwrap();
        let vars = Captured::from([("session".to_owned(), serde_json::json!("abc-123"))]);

        let rendered = templates.render("files", &BTreeMap::new(), &vars).unwrap();

        assert_eq!(rendered[0].1.expose(), "abc-123");
    }

    #[test]
    fn a_captured_value_that_is_not_a_string_still_goes_in_a_header() {
        let templates =
            HeaderTemplates::compile(&declared(&[("x-attempt", "{{ vars.attempt }}")])).unwrap();
        let vars = Captured::from([("attempt".to_owned(), serde_json::json!(3))]);

        let rendered = templates.render("files", &BTreeMap::new(), &vars).unwrap();

        assert_eq!(rendered[0].1.expose(), "3");
    }

    #[test]
    fn a_variable_no_call_has_captured_yet_is_named_rather_than_sent_empty() {
        let templates =
            HeaderTemplates::compile(&declared(&[("x-session", "{{ vars.session }}")])).unwrap();

        let message = templates
            .render("files", &BTreeMap::new(), &Captured::new())
            .unwrap_err()
            .to_string();

        // Named, like a missing `env` or `auth`: "undefined value" on a header
        // that was meant to carry a session tells nobody anything.
        assert!(message.contains("session"), "{message}");
        assert!(message.contains("files"), "{message}");
    }

    #[test]
    fn a_header_that_is_optional_until_a_tool_opens_the_session_says_so() {
        // The escape hatch a *server* header needs: its first render is the
        // `tools/list` at setup, before any tool has been called at all.
        let templates = HeaderTemplates::compile(&declared(&[(
            "x-session",
            "{{ vars.session | default('') }}",
        )]))
        .unwrap();

        let empty = templates
            .render("files", &BTreeMap::new(), &Captured::new())
            .unwrap();
        assert_eq!(empty[0].1.expose(), "");

        let vars = Captured::from([("session".to_owned(), serde_json::json!("abc-123"))]);
        let later = templates.render("files", &BTreeMap::new(), &vars).unwrap();
        assert_eq!(later[0].1.expose(), "abc-123");
    }

    #[test]
    fn an_error_names_variables_and_never_the_template() {
        // A template can hold a literal credential. An error message is exactly
        // where one must not end up.
        let templates = HeaderTemplates::compile(&declared(&[(
            "x-api-key",
            "hunter2-{{ env.MIRE_MISSING_SUFFIX }}",
        )]))
        .unwrap();

        let message = templates
            .render("files", &BTreeMap::new(), &Captured::new())
            .unwrap_err()
            .to_string();
        assert!(message.contains("MIRE_MISSING_SUFFIX"), "{message}");
        assert!(!message.contains("hunter2"), "{message}");
    }

    #[test]
    fn an_optional_header_can_still_have_a_fallback() {
        let templates = HeaderTemplates::compile(&declared(&[(
            "x-tenant",
            "{{ env.MIRE_TENANT_NOT_SET | default('dev') }}",
        )]))
        .unwrap();

        assert_eq!(
            templates
                .render("files", &BTreeMap::new(), &Captured::new())
                .unwrap()[0]
                .1
                .expose(),
            "dev"
        );
    }

    #[test]
    fn a_token_comes_out_of_the_auth_registry_and_into_any_header() {
        let templates =
            HeaderTemplates::compile(&declared(&[("x-api-key", r#"{{ auth["workload"] }}"#)]))
                .unwrap();

        // Declared once at load, so the caller knows what to resolve — and
        // resolves nothing else.
        assert_eq!(templates.providers().collect::<Vec<_>>(), vec!["workload"]);

        let auth = BTreeMap::from([("workload".to_owned(), Secret::new("t0ken"))]);
        let rendered = templates.render("files", &auth, &Captured::new()).unwrap();
        assert_eq!(rendered[0].0.as_str(), "x-api-key");
        assert_eq!(rendered[0].1.expose(), "t0ken");
    }

    #[test]
    fn a_token_can_be_composed_into_a_larger_value() {
        // The whole reason this exists next to `auth:`: the provider puts its
        // credential where the provider says, and sometimes that is not where
        // this server wants it.
        let templates = HeaderTemplates::compile(&declared(&[(
            "authorization",
            r#"Custom tenant={{ env.PATH | length }} token={{ auth["workload"] }}"#,
        )]))
        .unwrap();

        let auth = BTreeMap::from([("workload".to_owned(), Secret::new("t0ken"))]);
        let value = templates.render("files", &auth, &Captured::new()).unwrap()[0]
            .1
            .expose()
            .to_owned();
        assert!(value.ends_with("token=t0ken"), "{value}");
    }

    #[test]
    fn both_spellings_of_a_provider_lookup_are_found() {
        // A registry name usually has a hyphen in it, and `auth.keycloak-workload`
        // parses as a subtraction — so the bracket form has to work.
        assert_eq!(
            lookups(r#"{{ auth["keycloak-workload"] }}"#, "auth"),
            vec!["keycloak-workload"]
        );
        assert_eq!(lookups("{{ auth.workload }}", "auth"), vec!["workload"]);
        assert!(lookups("{{ env.TOKEN }}", "auth").is_empty());
    }

    #[test]
    fn a_provider_that_produces_no_credential_is_named_rather_than_undefined() {
        let templates =
            HeaderTemplates::compile(&declared(&[("authorization", r#"{{ auth["nobody"] }}"#)]))
                .unwrap();

        // `anonymous` resolves to no credential, so it never reaches the map.
        let message = templates
            .render("files", &BTreeMap::new(), &Captured::new())
            .unwrap_err()
            .to_string();
        assert!(message.contains("nobody"), "{message}");
    }

    #[test]
    fn a_broken_template_is_caught_when_the_registry_loads() {
        let error =
            HeaderTemplates::compile(&declared(&[("authorization", "{{ unclosed")])).unwrap_err();
        assert!(error.contains("authorization"), "{error}");
    }

    #[test]
    fn a_header_name_that_is_not_one_is_caught_too() {
        let error = HeaderTemplates::compile(&declared(&[("not a header", "x")])).unwrap_err();
        assert!(error.contains("not a valid HTTP header name"), "{error}");
    }

    #[test]
    fn a_rendered_value_never_prints_itself() {
        let templates = HeaderTemplates::compile(&declared(&[("x-api-key", "hunter2")])).unwrap();
        let rendered = templates
            .render("files", &BTreeMap::new(), &Captured::new())
            .unwrap();

        // The whole point of handing back a `Secret`: this cannot end up in a log
        // through a stray `{:?}`.
        assert!(!format!("{:?}", rendered[0].1).contains("hunter2"));
    }
}
