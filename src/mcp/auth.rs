//! Who `mire` is when it talks to an MCP server.
//!
//! Two ways in, and they answer different questions:
//!
//! * `auth: <provider>` on the server — the credential goes where the *provider*
//!   says it goes. The ordinary case, and it brings the `401`-refresh-and-replay
//!   behaviour with it.
//! * `{{ auth["<provider>"] }}` inside a header template — the same credential,
//!   placed wherever the server happens to want it. For the endpoint that takes
//!   its token in `X-Api-Key`, or under a scheme nobody else uses, or wrapped in
//!   something larger.
//!
//! Both read the same registry, so pointing a server at `anonymous`, a token and
//! a workload identity in turn is one word either way — which is the move this
//! whole tool exists for.
//!
//! # Resolved before rendering, not during
//!
//! Obtaining a credential is asynchronous and can fail: a token exchange, a
//! refresh, a session that is not signed in. `MiniJinja` renders synchronously
//! and cannot await any of that. So the templates declare which providers they
//! name when they compile, and this resolves exactly those, once, before a
//! single header is rendered.
//!
//! Exactly those, and no others: asking a provider for a credential has a cost
//! and a failure mode, and neither belongs to a server that never mentioned it.

use std::collections::BTreeMap;

use crate::auth::{Auth, AuthProvider, registry::AuthRegistry};
use crate::redact::Secret;

use super::{McpError, McpServer};

/// Everything one MCP request needs to authenticate.
#[derive(Debug, Default)]
pub struct McpCredentials<'a> {
    provider: Option<&'a Auth>,
    named: BTreeMap<String, Secret>,
}

impl<'a> McpCredentials<'a> {
    /// Resolves the server's own provider and whatever its templates name.
    ///
    /// # Errors
    ///
    /// [`McpError::Auth`] when a name is not in the registry, or when a provider
    /// cannot produce its credential — an unset variable, an unreadable token
    /// file, a failed exchange, a browser session nobody has signed into.
    pub async fn resolve(registry: &'a AuthRegistry, server: &McpServer) -> Result<Self, McpError> {
        let provider = match server.auth.as_deref() {
            None => None,
            Some(name) => Some(look_up(registry, name)?),
        };

        let mut named = BTreeMap::new();
        for name in server.headers.providers() {
            let entry = look_up(registry, name)?;
            // `anonymous` has nothing to give. It is left out rather than
            // inserted empty, so the template reports a provider that produces
            // no credential instead of quietly sending an empty header.
            if let Some(token) = entry.credential(&server.url, None).await? {
                named.insert(name.to_owned(), token);
            }
        }

        Ok(Self { provider, named })
    }

    /// The server's `auth:` provider, if it declares one.
    #[must_use]
    pub fn provider(&self) -> Option<&'a Auth> {
        self.provider
    }

    /// Bare credentials by provider name, for the header templates.
    #[must_use]
    pub fn named(&self) -> &BTreeMap<String, Secret> {
        &self.named
    }
}

fn look_up<'a>(registry: &'a AuthRegistry, name: &str) -> Result<&'a Auth, McpError> {
    registry
        .get(name)
        .ok_or_else(|| McpError::Auth(crate::auth::AuthError::UnknownProvider(name.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::registry::AuthRegistry;
    use crate::mcp::McpServer;

    fn registry(yaml: &str) -> AuthRegistry {
        let dir = tempfile::TempDir::new().expect("temp dir");
        std::fs::write(dir.path().join("auth.yaml"), yaml).expect("write");
        AuthRegistry::load(
            dir.path(),
            &reqwest::Client::new(),
            &std::sync::Arc::new(crate::auth::SessionStore::default()),
        )
    }

    fn server(auth: Option<&str>, headers: &[(&str, &str)]) -> McpServer {
        let declared: BTreeMap<String, String> = headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        McpServer {
            name: "files".to_owned(),
            url: "https://mcp.internal/mcp".parse().expect("url"),
            auth: auth.map(str::to_owned),
            tools: Vec::new(),
            headers: crate::mcp::HeaderTemplates::compile(&declared).expect("compile"),
            timeout: std::time::Duration::from_secs(5),
        }
    }

    /// The point of the whole module: a credential the registry knows how to
    /// produce, placed somewhere the provider would never have put it.
    #[tokio::test]
    async fn a_named_provider_reaches_a_header_template() {
        let registry = registry(
            "providers:\n  - name: workload\n    kind: token\n    value:\n      env: MIRE_TEST_MCP_TOKEN\n",
        );
        let server = server(None, &[("x-api-key", "{{ auth.workload }}")]);

        let error = McpCredentials::resolve(&registry, &server)
            .await
            .expect_err("the variable is not set");
        // The failure names the provider's own problem rather than the header's,
        // which is the layer that can actually be fixed.
        assert!(error.to_string().contains("MIRE_TEST_MCP_TOKEN"), "{error}");
    }

    #[tokio::test]
    async fn a_provider_nobody_declared_is_named() {
        let registry = registry("providers: []\n");
        let server = server(None, &[("x-api-key", r#"{{ auth["ghost"] }}"#)]);

        let error = McpCredentials::resolve(&registry, &server)
            .await
            .expect_err("no such provider");
        assert!(error.to_string().contains("ghost"), "{error}");
    }

    /// A server that names no provider resolves nothing at all — no exchange, no
    /// refresh, no session lookup.
    #[tokio::test]
    async fn a_server_that_asks_for_nothing_resolves_nothing() {
        let registry = registry("providers: []\n");
        let server = server(None, &[("x-tenant", "acme")]);

        let credentials = McpCredentials::resolve(&registry, &server)
            .await
            .expect("nothing to resolve");
        assert!(credentials.provider().is_none());
        assert!(credentials.named().is_empty());
    }

    #[tokio::test]
    async fn anonymous_produces_no_credential_and_is_left_out() {
        let registry = registry("providers: []\n");
        let server = server(None, &[("x-api-key", r#"{{ auth["anonymous"] }}"#)]);

        let credentials = McpCredentials::resolve(&registry, &server)
            .await
            .expect("anonymous resolves");
        // Absent, not empty: the render then says so by name instead of sending
        // a header that looks present.
        assert!(credentials.named().is_empty());
    }
}
