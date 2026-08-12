//! OIDC `client_credentials`: the mode that reproduces what a pod actually does.
//!
//! Two ways to authenticate the client, and the second is the interesting one:
//!
//! * `client_secret`, from an environment variable or a file;
//! * `client_assertion` (RFC 7523), a **projected service account token**, re-read
//!   from disk on every exchange so that rotation is a non-event.
//!
//! # Why this is hand-rolled
//!
//! `openidconnect` and `oauth2` bring their own HTTP client, or want an adapter
//! written around ours. Either way the token endpoint would stop going through
//! [`crate::transport`] — losing the custom CA bundle and the redirect policy,
//! which is precisely what an internal `IdP` needs. What we use of the spec is a
//! `GET` on the discovery document and a form `POST`; that is cheaper to own than
//! to adapt.

use std::path::PathBuf;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;
use tokio::sync::Mutex;
use tracing::{debug, info};
use url::Url;

use super::token::TokenValue;
use super::{AuthError, AuthProvider, Retry, check_allowed_host};
use crate::redact::{Redactor, Secret};

/// Renew this long before the token actually expires, so a request in flight
/// never races the expiry.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// Assumed lifetime when the `IdP` does not send `expires_in`.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(300);

/// `client_assertion_type` for a JWT bearer assertion (RFC 7523).
const JWT_BEARER: &str = "urn:ietf:params:oauth:client-assertion-type:jwt-bearer";

/// How the client authenticates itself to the token endpoint.
#[derive(Debug, Clone)]
pub enum ClientCredential {
    /// A shared secret.
    Secret(TokenValue),
    /// A signed assertion, typically a projected service account token.
    ///
    /// Re-read on every exchange: a projected token is rotated under you, and
    /// caching it is how you get a mysterious `401` an hour after deploying.
    Assertion {
        /// Path to the token file.
        file: PathBuf,
    },
}

/// A token, and when to stop trusting it.
#[derive(Debug)]
struct CachedToken {
    access_token: Secret,
    expires_at: Instant,
    /// `true` once this token has been handed out by a *later* call than the one
    /// that minted it. A freshly minted token that gets a `401` is the endpoint's
    /// verdict, not a stale credential, so it is not worth replaying.
    served_from_cache: bool,
}

/// Discovered endpoints. Fetched once, then kept.
#[derive(Debug, Clone)]
struct Discovery {
    token_endpoint: Url,
}

/// Fetches and caches an access token via `client_credentials`.
#[derive(Debug)]
pub struct OidcAuth {
    name: String,
    issuer: Url,
    token_endpoint: Option<Url>,
    client_id: String,
    credential: ClientCredential,
    scope: Vec<String>,
    audience: Option<String>,
    header: HeaderName,
    scheme: Option<String>,
    allowed_hosts: Vec<String>,
    http: Client,
    discovery: RwLock<Option<Discovery>>,
    cache: RwLock<Option<CachedToken>>,
    /// Serialises refreshes so a burst of calls triggers one exchange, not ten.
    refreshing: Mutex<()>,
}

/// Everything needed to build an [`OidcAuth`], as declared in `auth.yaml`.
#[derive(Debug, Clone)]
pub struct OidcConfig {
    /// Registry name.
    pub name: String,
    /// Issuer, used for discovery unless `token_endpoint` is set.
    pub issuer: Url,
    /// Explicit token endpoint, skipping discovery.
    pub token_endpoint: Option<Url>,
    /// `OAuth2` client identifier.
    pub client_id: String,
    /// How the client proves who it is.
    pub credential: ClientCredential,
    /// Requested scopes.
    pub scope: Vec<String>,
    /// Requested audience, for `IdPs` that need one.
    pub audience: Option<String>,
    /// Header the access token goes into.
    pub header: HeaderName,
    /// Scheme prefix. `None` sends the token bare.
    pub scheme: Option<String>,
    /// Hosts this credential may be sent to. Empty means no restriction.
    pub allowed_hosts: Vec<String>,
}

impl OidcAuth {
    /// Builds a provider. Nothing is fetched until the first call.
    #[must_use]
    pub fn new(config: OidcConfig, http: Client) -> Self {
        Self {
            name: config.name,
            issuer: config.issuer,
            token_endpoint: config.token_endpoint,
            client_id: config.client_id,
            credential: config.credential,
            scope: config.scope,
            audience: config.audience,
            header: config.header,
            scheme: config.scheme,
            allowed_hosts: config.allowed_hosts,
            http,
            discovery: RwLock::new(None),
            cache: RwLock::new(None),
            refreshing: Mutex::new(()),
        }
    }

    /// Returns a usable access token, exchanging for a new one if needed.
    async fn access_token(&self) -> Result<Secret, AuthError> {
        if let Some(token) = self.cached() {
            return Ok(token);
        }

        // Double-checked: several concurrent calls arriving on an expired token
        // should produce one exchange, not one each.
        let _guard = self.refreshing.lock().await;
        if let Some(token) = self.cached() {
            return Ok(token);
        }

        let endpoint = self.resolve_token_endpoint().await?;
        let (access_token, lifetime) = self.exchange(&endpoint).await?;

        // Renew early, but never claim a token expired before it was issued: the
        // margin is capped at half the lifetime, so this subtraction is safe.
        let usable = lifetime.saturating_sub(REFRESH_MARGIN.min(lifetime / 2));
        info!(
            provider = %self.name,
            lifetime_s = lifetime.as_secs(),
            "access token obtained"
        );
        *self.cache.write().expect("oidc cache lock") = Some(CachedToken {
            access_token: access_token.clone(),
            expires_at: Instant::now() + usable,
            served_from_cache: false,
        });

        Ok(access_token)
    }

    /// Reads the cached token if it is still good, marking it as reused.
    fn cached(&self) -> Option<Secret> {
        let mut guard = self.cache.write().expect("oidc cache lock");
        let entry = guard.as_mut()?;
        if Instant::now() >= entry.expires_at {
            *guard = None;
            return None;
        }
        entry.served_from_cache = true;
        Some(entry.access_token.clone())
    }

    /// The token endpoint, from configuration or from discovery.
    async fn resolve_token_endpoint(&self) -> Result<Url, AuthError> {
        if let Some(endpoint) = &self.token_endpoint {
            return Ok(endpoint.clone());
        }
        if let Some(discovery) = self.discovery.read().expect("oidc discovery lock").as_ref() {
            return Ok(discovery.token_endpoint.clone());
        }

        let document = fetch_discovery(&self.name, &self.http, &self.issuer).await?;
        let token_endpoint = document.token_endpoint(&self.name, &self.issuer)?;

        *self.discovery.write().expect("oidc discovery lock") = Some(Discovery {
            token_endpoint: token_endpoint.clone(),
        });
        Ok(token_endpoint)
    }

    /// Performs the `client_credentials` exchange.
    async fn exchange(&self, endpoint: &Url) -> Result<(Secret, Duration), AuthError> {
        let client_credential = self.read_client_credential()?;
        // Whatever the IdP says back could quote what we sent it.
        let scrub = Redactor::new().with(&client_credential);

        let mut form = vec![
            ("grant_type".to_owned(), "client_credentials".to_owned()),
            ("client_id".to_owned(), self.client_id.clone()),
        ];
        match &self.credential {
            ClientCredential::Secret(_) => form.push((
                "client_secret".to_owned(),
                client_credential.expose().to_owned(),
            )),
            ClientCredential::Assertion { .. } => {
                form.push(("client_assertion_type".to_owned(), JWT_BEARER.to_owned()));
                form.push((
                    "client_assertion".to_owned(),
                    client_credential.expose().to_owned(),
                ));
            }
        }
        if !self.scope.is_empty() {
            form.push(("scope".to_owned(), self.scope.join(" ")));
        }
        if let Some(audience) = &self.audience {
            form.push(("audience".to_owned(), audience.clone()));
        }

        let response = self
            .http
            .post(endpoint.clone())
            .form(&form)
            .send()
            .await
            .map_err(|error| AuthError::TokenExchange {
                provider: self.name.clone(),
                message: scrub.text(&error.to_string()),
            })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            let detail = describe_failure(&body);
            return Err(AuthError::TokenExchange {
                provider: self.name.clone(),
                message: format!(
                    "the token endpoint answered {status} — {}",
                    scrub.text(&detail)
                ),
            });
        }

        let token: TokenResponse =
            serde_json::from_str(&body).map_err(|error| AuthError::TokenExchange {
                provider: self.name.clone(),
                message: format!("the token response is not usable: {error}"),
            })?;

        let lifetime = token
            .expires_in
            .map_or(DEFAULT_LIFETIME, Duration::from_secs);
        Ok((Secret::new(token.access_token), lifetime))
    }

    /// Reads the client secret or the assertion. Always from disk or the
    /// environment, never from a cache — that is what makes rotation work.
    fn read_client_credential(&self) -> Result<Secret, AuthError> {
        match &self.credential {
            ClientCredential::Secret(value) => {
                if let Some(variable) = &value.env {
                    let raw = std::env::var(variable).map_err(|_| AuthError::MissingEnv {
                        provider: self.name.clone(),
                        variable: variable.clone(),
                    })?;
                    return Ok(Secret::new(raw.trim()));
                }
                if let Some(path) = &value.file {
                    return read_trimmed(&self.name, path);
                }
                Err(AuthError::NoCredential {
                    provider: self.name.clone(),
                })
            }
            ClientCredential::Assertion { file } => read_trimmed(&self.name, file),
        }
    }
}

fn read_trimmed(provider: &str, path: &std::path::Path) -> Result<Secret, AuthError> {
    let raw = std::fs::read_to_string(path).map_err(|source| AuthError::TokenFile {
        provider: provider.to_owned(),
        path: path.display().to_string(),
        source,
    })?;
    Ok(Secret::new(raw.trim()))
}

/// Turns a token endpoint's failure body into one readable line, falling back to
/// a truncated body when it is not the `OAuth2` error shape.
pub(super) fn describe_failure(body: &str) -> String {
    serde_json::from_str::<TokenError>(body).map_or_else(
        |_| body.chars().take(200).collect::<String>(),
        |error| error.describe(),
    )
}

/// Builds `{issuer}/.well-known/openid-configuration`, tolerating a trailing
/// slash on the issuer.
pub(super) fn discovery_url(issuer: &Url) -> Url {
    let mut url = issuer.clone();
    let base = url.path().trim_end_matches('/').to_owned();
    url.set_path(&format!("{base}/.well-known/openid-configuration"));
    url
}

impl AuthProvider for OidcAuth {
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

        let mut value =
            HeaderValue::from_str(&rendered).map_err(|_| AuthError::InvalidHeaderValue {
                provider: self.name.clone(),
            })?;
        value.set_sensitive(true);
        headers.insert(self.header.clone(), value);

        let mut redactor = Redactor::new();
        redactor.add(&token);
        redactor.add(&Secret::new(rendered));
        if let Ok(credential) = self.read_client_credential() {
            redactor.add(&credential);
        }
        Ok(redactor)
    }

    async fn credential(
        &self,
        target: &Url,
        _supplied: Option<&Secret>,
    ) -> Result<Option<Secret>, AuthError> {
        check_allowed_host(&self.name, &self.allowed_hosts, target)?;
        Ok(Some(self.access_token().await?))
    }

    async fn invalidate(&self) -> Retry {
        let mut guard = self.cache.write().expect("oidc cache lock");
        match guard.as_ref() {
            // A token we had been reusing may well have been revoked upstream.
            // Drop it and let the replay mint a fresh one.
            Some(token) if token.served_from_cache => {
                *guard = None;
                debug!(provider = %self.name, "cached token rejected, will replay once");
                Retry::Once
            }
            // We minted this token for this very call, so it is as fresh as it
            // gets: the `401` is about something else — a missing scope, an
            // audience mismatch — and replaying would only hide it. The token
            // stays cached, because there is nothing wrong with it.
            _ => Retry::No,
        }
    }
}

/// The subset of `{issuer}/.well-known/openid-configuration` anyone here needs.
#[derive(Debug, Deserialize)]
pub(super) struct DiscoveryDocument {
    token_endpoint: String,
    /// Absent from a document advertising only machine-to-machine grants, which
    /// is a perfectly valid thing for an `IdP` to publish — and a useful error
    /// when a browser provider points at one.
    #[serde(default)]
    authorization_endpoint: Option<String>,
}

impl DiscoveryDocument {
    /// The token endpoint, parsed.
    pub(super) fn token_endpoint(&self, provider: &str, issuer: &Url) -> Result<Url, AuthError> {
        parse_endpoint(
            provider,
            issuer,
            "token_endpoint",
            Some(&self.token_endpoint),
        )
    }

    /// The authorization endpoint, parsed.
    pub(super) fn authorization_endpoint(
        &self,
        provider: &str,
        issuer: &Url,
    ) -> Result<Url, AuthError> {
        parse_endpoint(
            provider,
            issuer,
            "authorization_endpoint",
            self.authorization_endpoint.as_deref(),
        )
    }
}

fn parse_endpoint(
    provider: &str,
    issuer: &Url,
    field: &str,
    value: Option<&str>,
) -> Result<Url, AuthError> {
    let raw = value.ok_or_else(|| AuthError::Discovery {
        provider: provider.to_owned(),
        url: discovery_url(issuer).to_string(),
        message: format!("the discovery document declares no `{field}`"),
    })?;
    Url::parse(raw).map_err(|error| AuthError::Discovery {
        provider: provider.to_owned(),
        url: discovery_url(issuer).to_string(),
        message: format!("`{field}` is not a URL: {error}"),
    })
}

/// Fetches and parses the discovery document.
///
/// Shared with the browser flow, which needs the authorization endpoint out of
/// the same document — and, more to the point, needs it fetched through the same
/// HTTP client, so `--ca-bundle` keeps applying to the `IdP`.
pub(super) async fn fetch_discovery(
    provider: &str,
    http: &Client,
    issuer: &Url,
) -> Result<DiscoveryDocument, AuthError> {
    let url = discovery_url(issuer);
    debug!(provider, %url, "fetching the discovery document");

    let response = http
        .get(url.clone())
        .send()
        .await
        .map_err(|error| AuthError::Discovery {
            provider: provider.to_owned(),
            url: url.to_string(),
            message: error.to_string(),
        })?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(AuthError::Discovery {
            provider: provider.to_owned(),
            url: url.to_string(),
            message: format!("the issuer answered {status}"),
        });
    }

    serde_json::from_str(&body).map_err(|error| AuthError::Discovery {
        provider: provider.to_owned(),
        url: url.to_string(),
        message: format!("the discovery document is not usable: {error}"),
    })
}

/// A token endpoint's answer. Shared: the shapes are identical across grants.
#[derive(Debug, Deserialize)]
pub(super) struct TokenResponse {
    pub(super) access_token: String,
    #[serde(default)]
    pub(super) expires_in: Option<u64>,
    #[serde(default)]
    pub(super) refresh_token: Option<String>,
    #[serde(default)]
    pub(super) id_token: Option<String>,
    #[serde(default)]
    pub(super) scope: Option<String>,
}

/// The `OAuth2` error shape (RFC 6749 §5.2), also used on the redirect back.
#[derive(Debug, Deserialize)]
pub(super) struct TokenError {
    pub(super) error: String,
    #[serde(default)]
    pub(super) error_description: Option<String>,
}

impl TokenError {
    /// One line, whichever fields the `IdP` bothered to send.
    pub(super) fn describe(&self) -> String {
        match &self.error_description {
            Some(description) => format!("{}: {description}", self.error),
            None => self.error.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_url_tolerates_a_trailing_slash() {
        let expected = "https://idp.internal/realms/models/.well-known/openid-configuration";

        for issuer in [
            "https://idp.internal/realms/models",
            "https://idp.internal/realms/models/",
        ] {
            let url = discovery_url(&Url::parse(issuer).unwrap());
            assert_eq!(url.as_str(), expected);
        }
    }

    #[test]
    fn discovery_url_works_for_a_root_issuer() {
        let url = discovery_url(&Url::parse("https://idp.internal").unwrap());
        assert_eq!(
            url.as_str(),
            "https://idp.internal/.well-known/openid-configuration"
        );
    }
}
