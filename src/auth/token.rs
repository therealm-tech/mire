//! Static-token auth: `Authorization: Bearer <token>`, with both parts configurable.
//!
//! The token value never comes from the profile YAML — that file goes in Git. It
//! comes from an environment variable, a file re-read on every call (so rotation
//! works), or the UI.

use std::path::PathBuf;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use url::Url;

use super::{AuthError, AuthProvider, Retry, check_allowed_host};
use crate::redact::{Redactor, Secret};

/// Where the token is read from.
///
/// Both fields absent means "supplied with the request", i.e. typed in the UI.
#[derive(Debug, Clone, Default, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TokenValue {
    /// Environment variable holding the token, read on every call.
    #[serde(default)]
    pub env: Option<String>,
    /// File holding the token, read on every call so rotation is picked up.
    #[serde(default)]
    pub file: Option<PathBuf>,
}

/// Sends a fixed credential in a configurable header.
#[derive(Debug, Clone)]
pub struct TokenAuth {
    name: String,
    header: HeaderName,
    scheme: Option<String>,
    value: TokenValue,
    allowed_hosts: Vec<String>,
}

impl TokenAuth {
    /// Builds a token provider.
    ///
    /// `scheme` is prepended to the token (`Bearer <token>`); `None` sends the
    /// credential bare, which some gateways want.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        header: HeaderName,
        scheme: Option<String>,
        value: TokenValue,
        allowed_hosts: Vec<String>,
    ) -> Self {
        Self {
            name: name.into(),
            header,
            scheme,
            value,
            allowed_hosts,
        }
    }

    /// Resolves the credential for this call.
    ///
    /// A value supplied with the request wins, then `env`, then `file`. Both `env`
    /// and `file` are read here rather than cached, so a rotated token is picked up
    /// without restarting.
    fn resolve(&self, supplied: Option<&Secret>) -> Result<Secret, AuthError> {
        if let Some(secret) = supplied.filter(|secret| !secret.is_empty()) {
            return Ok(secret.clone());
        }
        if let Some(variable) = &self.value.env {
            let raw = std::env::var(variable).map_err(|_| AuthError::MissingEnv {
                provider: self.name.clone(),
                variable: variable.clone(),
            })?;
            return Ok(Secret::new(raw.trim()));
        }
        if let Some(path) = &self.value.file {
            let raw = std::fs::read_to_string(path).map_err(|source| AuthError::TokenFile {
                provider: self.name.clone(),
                path: path.display().to_string(),
                source,
            })?;
            return Ok(Secret::new(raw.trim()));
        }
        Err(AuthError::NoCredential {
            provider: self.name.clone(),
        })
    }
}

impl AuthProvider for TokenAuth {
    fn name(&self) -> &str {
        &self.name
    }

    async fn apply(
        &self,
        headers: &mut HeaderMap,
        target: &Url,
        supplied: Option<&Secret>,
    ) -> Result<Redactor, AuthError> {
        let token =
            self.credential(target, supplied)
                .await?
                .ok_or_else(|| AuthError::NoCredential {
                    provider: self.name.clone(),
                })?;
        let rendered = match &self.scheme {
            Some(scheme) if !scheme.is_empty() => format!("{scheme} {}", token.expose()),
            _ => token.expose().to_owned(),
        };

        // The error deliberately carries no value: a malformed credential is still
        // a credential.
        let mut header_value =
            HeaderValue::from_str(&rendered).map_err(|_| AuthError::InvalidHeaderValue {
                provider: self.name.clone(),
            })?;
        header_value.set_sensitive(true);
        headers.insert(self.header.clone(), header_value);

        let mut redactor = Redactor::new();
        redactor.add(&token);
        redactor.add(&Secret::new(rendered));
        Ok(redactor)
    }

    async fn credential(
        &self,
        target: &Url,
        supplied: Option<&Secret>,
    ) -> Result<Option<Secret>, AuthError> {
        check_allowed_host(&self.name, &self.allowed_hosts, target)?;
        Ok(Some(self.resolve(supplied)?))
    }

    async fn invalidate(&self) -> Retry {
        // Nothing is cached: env and file are read on every call, so replaying
        // would send the exact same bytes.
        Retry::No
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url() -> Url {
        Url::parse("https://models.internal/v1/chat/completions").unwrap()
    }

    fn provider(value: TokenValue) -> TokenAuth {
        TokenAuth::new(
            "gateway",
            HeaderName::from_static("authorization"),
            Some("Bearer".to_owned()),
            value,
            Vec::new(),
        )
    }

    #[tokio::test]
    async fn a_supplied_token_wins_over_the_configured_source() {
        let auth = provider(TokenValue {
            env: Some("MIRE_TEST_UNSET_VAR".to_owned()),
            file: None,
        });
        let mut headers = HeaderMap::new();

        let redactor = auth
            .apply(&mut headers, &url(), Some(&Secret::new("from-the-ui")))
            .await
            .unwrap();

        assert_eq!(headers["authorization"], "Bearer from-the-ui");
        assert_eq!(redactor.text("Bearer from-the-ui"), crate::redact::MASK);
    }

    #[tokio::test]
    async fn a_missing_env_var_names_the_variable_and_nothing_else() {
        let auth = provider(TokenValue {
            env: Some("MIRE_TEST_UNSET_VAR".to_owned()),
            file: None,
        });
        let error = auth
            .apply(&mut HeaderMap::new(), &url(), None)
            .await
            .unwrap_err();

        assert!(matches!(error, AuthError::MissingEnv { .. }));
        assert!(error.to_string().contains("MIRE_TEST_UNSET_VAR"));
    }

    #[tokio::test]
    async fn a_token_file_is_read_on_every_call() {
        let path = std::env::temp_dir().join(format!("mire-token-{}", std::process::id()));
        std::fs::write(&path, "first-token\n").unwrap();
        let auth = provider(TokenValue {
            env: None,
            file: Some(path.clone()),
        });

        let mut headers = HeaderMap::new();
        auth.apply(&mut headers, &url(), None).await.unwrap();
        assert_eq!(headers["authorization"], "Bearer first-token");

        std::fs::write(&path, "rotated-token\n").unwrap();
        let mut headers = HeaderMap::new();
        auth.apply(&mut headers, &url(), None).await.unwrap();
        assert_eq!(headers["authorization"], "Bearer rotated-token");

        std::fs::remove_file(&path).unwrap();
    }

    #[tokio::test]
    async fn a_malformed_credential_never_appears_in_the_error() {
        let auth = provider(TokenValue::default());
        let error = auth
            .apply(
                &mut HeaderMap::new(),
                &url(),
                Some(&Secret::new("bad\nvalue")),
            )
            .await
            .unwrap_err();

        assert!(matches!(error, AuthError::InvalidHeaderValue { .. }));
        assert!(!error.to_string().contains("bad"));
    }

    #[tokio::test]
    async fn no_source_and_nothing_supplied_is_a_clear_error() {
        let auth = provider(TokenValue::default());
        let error = auth
            .apply(&mut HeaderMap::new(), &url(), None)
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::NoCredential { .. }));
    }

    #[tokio::test]
    async fn allowed_hosts_blocks_a_foreign_target() {
        let auth = TokenAuth::new(
            "scoped",
            HeaderName::from_static("authorization"),
            Some("Bearer".to_owned()),
            TokenValue::default(),
            vec!["models.internal".to_owned()],
        );

        auth.apply(
            &mut HeaderMap::new(),
            &Url::parse("https://elsewhere.example/v1").unwrap(),
            Some(&Secret::new("token")),
        )
        .await
        .unwrap_err();

        auth.apply(&mut HeaderMap::new(), &url(), Some(&Secret::new("token")))
            .await
            .unwrap();
    }
}
