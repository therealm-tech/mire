//! HTTP Basic auth (RFC 7617): `Authorization: Basic base64(username:password)`.
//!
//! The username is not a secret and lives in the YAML; the password is read like
//! a static token — from an environment variable, a file re-read on every call,
//! or the UI.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};
use url::Url;

use super::{AuthError, AuthProvider, Retry, TokenValue, check_allowed_host};
use crate::redact::{Redactor, Secret};

/// Sends a username and password as `Authorization: Basic …`.
#[derive(Debug, Clone)]
pub struct BasicAuth {
    name: String,
    username: String,
    password: TokenValue,
    allowed_hosts: Vec<String>,
}

impl BasicAuth {
    /// Builds a basic provider. `username` must not contain `:`, which RFC 7617
    /// reserves as the separator; the registry refuses one at load time.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        username: impl Into<String>,
        password: TokenValue,
        allowed_hosts: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            username: username.into(),
            password,
            allowed_hosts,
        }
    }

    /// The password, then `base64(username:password)`. A password supplied with
    /// the request wins over the configured source.
    fn encode(&self, supplied: Option<&Secret>) -> Result<(Secret, Secret), AuthError> {
        let password = match supplied.filter(|secret| !secret.is_empty()) {
            Some(secret) => secret.clone(),
            None => self.password.read(&self.name)?,
        };
        let encoded = BASE64.encode(format!("{}:{}", self.username, password.expose()));
        Ok((password, Secret::new(encoded)))
    }
}

impl AuthProvider for BasicAuth {
    fn name(&self) -> &str {
        &self.name
    }

    async fn apply(
        &self,
        headers: &mut HeaderMap,
        target: &Url,
        supplied: Option<&Secret>,
    ) -> Result<Redactor, AuthError> {
        check_allowed_host(&self.name, &self.allowed_hosts, target)?;
        let (password, encoded) = self.encode(supplied)?;
        let rendered = Secret::new(format!("Basic {}", encoded.expose()));

        let mut header_value = HeaderValue::from_str(rendered.expose()).map_err(|_| {
            AuthError::InvalidHeaderValue {
                provider: self.name.clone(),
            }
        })?;
        header_value.set_sensitive(true);
        headers.insert(AUTHORIZATION, header_value);

        let mut redactor = Redactor::new();
        redactor.add(&password);
        redactor.add(&encoded);
        redactor.add(&rendered);
        Ok(redactor)
    }

    /// The encoded `username:password`, with no `Basic ` in front — the same
    /// part a token provider hands over without its scheme.
    async fn credential(
        &self,
        target: &Url,
        supplied: Option<&Secret>,
    ) -> Result<Option<Secret>, AuthError> {
        check_allowed_host(&self.name, &self.allowed_hosts, target)?;
        let (_, encoded) = self.encode(supplied)?;
        Ok(Some(encoded))
    }

    async fn invalidate(&self) -> Retry {
        // Nothing is cached: replaying would send the exact same bytes.
        Retry::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("https://models.internal/v1/chat/completions").unwrap()
    }

    fn provider(password: TokenValue) -> BasicAuth {
        BasicAuth::new("gateway", "alice", password, Vec::new())
    }

    #[tokio::test]
    async fn sends_the_encoded_pair_and_redacts_every_form_of_it() {
        let auth = provider(TokenValue::default());
        let mut headers = HeaderMap::new();

        let redactor = auth
            .apply(&mut headers, &url(), Some(&Secret::new("s3cret")))
            .await
            .unwrap();

        // base64("alice:s3cret")
        assert_eq!(headers["authorization"], "Basic YWxpY2U6czNjcmV0");
        assert!(headers["authorization"].is_sensitive());
        for form in ["s3cret", "YWxpY2U6czNjcmV0", "Basic YWxpY2U6czNjcmV0"] {
            assert_eq!(redactor.text(form), crate::redact::MASK, "{form}");
        }
    }

    #[tokio::test]
    async fn the_password_file_is_read_on_every_call() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "first\n").unwrap();
        let auth = provider(TokenValue {
            env: None,
            file: Some(file.path().to_path_buf()),
        });

        let first = auth.credential(&url(), None).await.unwrap().unwrap();
        std::fs::write(file.path(), "rotated\n").unwrap();
        let second = auth.credential(&url(), None).await.unwrap().unwrap();

        assert_eq!(first.expose(), BASE64.encode("alice:first"));
        assert_eq!(second.expose(), BASE64.encode("alice:rotated"));
    }

    #[tokio::test]
    async fn a_missing_env_var_names_the_variable() {
        let auth = provider(TokenValue {
            env: Some("MIRE_TEST_UNSET_BASIC_PASSWORD".to_owned()),
            file: None,
        });
        let error = auth
            .apply(&mut HeaderMap::new(), &url(), None)
            .await
            .unwrap_err();

        assert!(matches!(error, AuthError::MissingEnv { .. }));
        assert!(error.to_string().contains("MIRE_TEST_UNSET_BASIC_PASSWORD"));
    }

    #[tokio::test]
    async fn allowed_hosts_blocks_a_foreign_target() {
        let auth = BasicAuth::new(
            "scoped",
            "alice",
            TokenValue::default(),
            vec!["models.internal".to_owned()],
        );
        let password = Secret::new("s3cret");

        let error = auth
            .apply(
                &mut HeaderMap::new(),
                &Url::parse("https://elsewhere.example/v1").unwrap(),
                Some(&password),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::HostNotAllowed { .. }));

        auth.apply(&mut HeaderMap::new(), &url(), Some(&password))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn never_asks_for_a_retry() {
        assert_eq!(
            provider(TokenValue::default()).invalidate().await,
            Retry::No
        );
    }
}
