//! OIDC authorization code + PKCE: the mode where a human signs in.
//!
//! [`super::oidc`] answers "what does the *workload* get?". This answers "what do
//! *I* get?" — which is a different question, and the one you have when a gateway
//! accepts service accounts and user tokens with different rules.
//!
//! # The shape of it
//!
//! 1. `POST /api/auth/{name}/login` mints a PKCE verifier and a `state`, and hands
//!    back an authorization URL.
//! 2. The browser goes there, the human authenticates, the `IdP` redirects to
//!    `{public-url}{base-path}/auth/callback`.
//! 3. That handler trades the code for tokens and stores them in the
//!    [`SessionStore`](super::session::SessionStore), which outlives config reloads.
//! 4. Calls then use the access token, refreshing it silently when they can.
//!
//! # PKCE is not optional here
//!
//! `mire` is a public client by nature — it runs on a laptop from a directory of
//! YAML files, so there is no secret it could keep. PKCE (RFC 7636, `S256`) is
//! what makes that safe: the authorization code is useless to anyone who did not
//! generate the verifier. A `client_secret` is still accepted, for `IdPs`
//! configured with a confidential client, but it is never required.

use std::sync::{Arc, RwLock};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use reqwest::Client;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use sha2::{Digest, Sha256};
use tracing::{debug, info};
use url::Url;

use super::oidc::{TokenResponse, describe_failure, fetch_discovery};
use super::session::{Pending, SessionStore, Tokens, random_urlsafe, subject_of};
use super::token::TokenValue;
use super::{AuthError, AuthProvider, Retry, check_allowed_host};
use crate::redact::{Redactor, Secret};

/// Assumed lifetime when the `IdP` does not send `expires_in`.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(300);

/// Path the `IdP` redirects back to, under `--base-path`.
///
/// Fixed relative to the prefix, because the *origin* is not: `mire` is meant to
/// run inside a Kubeflow notebook, where the URL the browser sees
/// (`https://kubeflow.example/notebook/<ns>/<name>/proxy/8787/`) has nothing to do
/// with the address it binds. See [`resolve_redirect_uri`](crate::api::handlers::resolve_redirect_uri).
pub const CALLBACK_PATH: &str = "/auth/callback";

/// Endpoints, from discovery or from configuration.
#[derive(Debug, Clone)]
struct Endpoints {
    authorization: Url,
    token: Url,
}

/// An authorization URL and the state that identifies the attempt.
#[derive(Debug)]
pub struct AuthorizationRequest {
    /// Where to send the browser.
    pub url: Url,
    /// Opaque, single-use, and the only thing tying the return trip to this login.
    pub state: String,
}

/// Everything needed to build an [`OidcBrowserAuth`], as declared in `auth.yaml`.
#[derive(Debug, Clone)]
pub struct OidcBrowserConfig {
    /// Registry name.
    pub name: String,
    /// Issuer, used for discovery unless both endpoints are set.
    pub issuer: Url,
    /// Explicit authorization endpoint, skipping discovery.
    pub authorization_endpoint: Option<Url>,
    /// Explicit token endpoint, skipping discovery.
    pub token_endpoint: Option<Url>,
    /// `OAuth2` client identifier.
    pub client_id: String,
    /// Optional, for a confidential client. PKCE alone covers the public case.
    pub client_secret: Option<TokenValue>,
    /// Requested scopes. `openid` is added if you forget it.
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

/// A provider whose token comes from a human signing in.
#[derive(Debug)]
pub struct OidcBrowserAuth {
    name: String,
    issuer: Url,
    authorization_endpoint: Option<Url>,
    token_endpoint: Option<Url>,
    client_id: String,
    client_secret: Option<TokenValue>,
    scope: Vec<String>,
    audience: Option<String>,
    header: HeaderName,
    scheme: Option<String>,
    allowed_hosts: Vec<String>,
    http: Client,
    discovery: RwLock<Option<Endpoints>>,
    /// Shared with every other provider, and *not* rebuilt on a config reload —
    /// which is what keeps you signed in while you edit a profile.
    sessions: Arc<SessionStore>,
    /// Serialises refreshes so a burst of calls triggers one exchange, not ten.
    refreshing: tokio::sync::Mutex<()>,
}

impl OidcBrowserAuth {
    /// Builds a provider. Nothing is fetched, and nobody is signed in, until asked.
    #[must_use]
    pub fn new(config: OidcBrowserConfig, http: Client, sessions: Arc<SessionStore>) -> Self {
        let mut scope = config.scope;
        // Without `openid` the IdP runs a plain OAuth2 flow and returns no
        // `id_token`, so the UI could not even say who signed in.
        if !scope.iter().any(|entry| entry == "openid") {
            scope.insert(0, "openid".to_owned());
        }

        Self {
            name: config.name,
            issuer: config.issuer,
            authorization_endpoint: config.authorization_endpoint,
            token_endpoint: config.token_endpoint,
            client_id: config.client_id,
            client_secret: config.client_secret,
            scope,
            audience: config.audience,
            header: config.header,
            scheme: config.scheme,
            allowed_hosts: config.allowed_hosts,
            http,
            discovery: RwLock::new(None),
            sessions,
            refreshing: tokio::sync::Mutex::new(()),
        }
    }

    /// The session store, so handlers can read status without going through a call.
    #[must_use]
    pub fn sessions(&self) -> &Arc<SessionStore> {
        &self.sessions
    }

    /// Starts a login: mints a PKCE verifier, records the attempt, and builds the
    /// URL to send the browser to.
    ///
    /// # Errors
    ///
    /// Fails if discovery cannot reach the issuer or the document has no
    /// authorization endpoint — which is exactly what happens when a browser
    /// provider is pointed at a machine-to-machine-only `IdP`.
    pub async fn start_login(
        &self,
        redirect_uri: &str,
        prompt: Option<&str>,
    ) -> Result<AuthorizationRequest, AuthError> {
        let endpoints = self.endpoints().await?;

        let verifier = random_urlsafe();
        let challenge = BASE64URL.encode(Sha256::digest(verifier.as_bytes()));
        let state =
            self.sessions
                .begin(&self.name, Secret::new(&verifier), redirect_uri.to_owned());

        let mut url = endpoints.authorization;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("response_type", "code");
            query.append_pair("client_id", &self.client_id);
            query.append_pair("redirect_uri", redirect_uri);
            query.append_pair("scope", &self.scope.join(" "));
            query.append_pair("state", &state);
            query.append_pair("code_challenge", &challenge);
            query.append_pair("code_challenge_method", "S256");
            if let Some(audience) = &self.audience {
                query.append_pair("audience", audience);
            }
            if let Some(prompt) = prompt.map(str::trim).filter(|value| !value.is_empty()) {
                query.append_pair("prompt", prompt);
            }
        }

        debug!(provider = %self.name, %redirect_uri, ?prompt, "login started");
        Ok(AuthorizationRequest { url, state })
    }

    /// Finishes a login: trades the authorization code for tokens and stores them.
    ///
    /// # Errors
    ///
    /// Fails if the token endpoint refuses the exchange. The message is scrubbed
    /// of anything we sent, because an `IdP` will happily quote it back.
    pub async fn complete_login(&self, pending: &Pending, code: &str) -> Result<(), AuthError> {
        let form = vec![
            ("grant_type".to_owned(), "authorization_code".to_owned()),
            ("code".to_owned(), code.to_owned()),
            ("redirect_uri".to_owned(), pending.redirect_uri.clone()),
            ("client_id".to_owned(), self.client_id.clone()),
            (
                "code_verifier".to_owned(),
                pending.verifier.expose().to_owned(),
            ),
        ];

        let tokens = self.exchange(form, Some(&pending.verifier)).await?;
        info!(
            provider = %self.name,
            subject = tokens.subject.as_deref().unwrap_or("(unknown)"),
            "signed in"
        );
        self.sessions.store(&self.name, tokens);
        Ok(())
    }

    /// Trades the refresh token for a new access token.
    async fn refresh(&self) -> Result<Secret, AuthError> {
        // Double-checked: a burst of calls on an expired token should refresh once.
        let _guard = self.refreshing.lock().await;
        if let Some(token) = self.sessions.access_token(&self.name) {
            return Ok(token);
        }

        let refresh_token =
            self.sessions
                .refresh_token(&self.name)
                .ok_or_else(|| AuthError::NotSignedIn {
                    provider: self.name.clone(),
                    detail:
                        "the session expired and the identity provider granted no refresh token"
                            .to_owned(),
                })?;

        let form = vec![
            ("grant_type".to_owned(), "refresh_token".to_owned()),
            (
                "refresh_token".to_owned(),
                refresh_token.expose().to_owned(),
            ),
            ("client_id".to_owned(), self.client_id.clone()),
        ];

        let tokens = match self.exchange(form, Some(&refresh_token)).await {
            Ok(tokens) => tokens,
            Err(error) => {
                // A refresh token the IdP no longer accepts is a dead session, and
                // leaving it in place would fail every subsequent call the same
                // way. Drop it so the UI offers the sign-in button again.
                self.sessions.clear(&self.name);
                return Err(error);
            }
        };

        debug!(provider = %self.name, "session refreshed without a browser");
        let access_token = tokens.access_token.clone();
        self.sessions.store(&self.name, tokens);
        Ok(access_token)
    }

    /// Posts to the token endpoint and reads the answer.
    ///
    /// `sent` is whatever secret went into the form: an `IdP` that echoes its
    /// input in an error must not turn that into a leak.
    async fn exchange(
        &self,
        mut form: Vec<(String, String)>,
        sent: Option<&Secret>,
    ) -> Result<Tokens, AuthError> {
        let endpoint = self.endpoints().await?.token;

        let mut scrub = Redactor::new();
        if let Some(secret) = sent {
            scrub.add(secret);
        }
        if let Some(value) = &self.client_secret {
            let secret = read_client_secret(&self.name, value)?;
            scrub.add(&secret);
            form.push(("client_secret".to_owned(), secret.expose().to_owned()));
        }
        if let Some(audience) = &self.audience {
            form.push(("audience".to_owned(), audience.clone()));
        }

        let response = self
            .http
            .post(endpoint)
            .form(&form)
            .send()
            .await
            .map_err(|error| AuthError::TokenExchange {
                provider: self.name.clone(),
                message: scrub.text(&crate::transport::explain(&error)),
            })?;

        let status = response.status();
        let body = response.text().await.unwrap_or_default();

        if !status.is_success() {
            return Err(AuthError::TokenExchange {
                provider: self.name.clone(),
                message: format!(
                    "the token endpoint answered {status} — {}",
                    scrub.text(&describe_failure(&body))
                ),
            });
        }

        let token: TokenResponse =
            serde_json::from_str(&body).map_err(|error| AuthError::TokenExchange {
                provider: self.name.clone(),
                message: format!("the token response is not usable: {error}"),
            })?;

        Ok(Tokens {
            access_token: Secret::new(token.access_token),
            refresh_token: token.refresh_token.map(Secret::new),
            lifetime: token
                .expires_in
                .map_or(DEFAULT_LIFETIME, Duration::from_secs),
            subject: token.id_token.as_deref().and_then(subject_of),
            scope: token.scope,
        })
    }

    /// Both endpoints, from configuration or from one discovery round trip.
    async fn endpoints(&self) -> Result<Endpoints, AuthError> {
        if let (Some(authorization), Some(token)) =
            (&self.authorization_endpoint, &self.token_endpoint)
        {
            return Ok(Endpoints {
                authorization: authorization.clone(),
                token: token.clone(),
            });
        }
        if let Some(cached) = self.discovery.read().expect("discovery lock").as_ref() {
            return Ok(cached.clone());
        }

        let document = fetch_discovery(&self.name, &self.http, &self.issuer).await?;
        let endpoints = Endpoints {
            authorization: match &self.authorization_endpoint {
                Some(url) => url.clone(),
                None => document.authorization_endpoint(&self.name, &self.issuer)?,
            },
            token: match &self.token_endpoint {
                Some(url) => url.clone(),
                None => document.token_endpoint(&self.name, &self.issuer)?,
            },
        };

        *self.discovery.write().expect("discovery lock") = Some(endpoints.clone());
        Ok(endpoints)
    }
}

/// Reads the client secret, from the environment or a file, on every exchange.
fn read_client_secret(provider: &str, value: &TokenValue) -> Result<Secret, AuthError> {
    if let Some(variable) = &value.env {
        let raw = std::env::var(variable).map_err(|_| AuthError::MissingEnv {
            provider: provider.to_owned(),
            variable: variable.clone(),
        })?;
        return Ok(Secret::new(raw.trim()));
    }
    if let Some(path) = &value.file {
        let raw = std::fs::read_to_string(path).map_err(|source| AuthError::TokenFile {
            provider: provider.to_owned(),
            path: path.display().to_string(),
            source,
        })?;
        return Ok(Secret::new(raw.trim()));
    }
    Err(AuthError::NoCredential {
        provider: provider.to_owned(),
    })
}

impl AuthProvider for OidcBrowserAuth {
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
        if let Some(refresh) = self.sessions.refresh_token(&self.name) {
            redactor.add(&refresh);
        }
        Ok(redactor)
    }

    async fn credential(
        &self,
        target: &Url,
        _supplied: Option<&Secret>,
    ) -> Result<Option<Secret>, AuthError> {
        check_allowed_host(&self.name, &self.allowed_hosts, target)?;

        match self.sessions.access_token(&self.name) {
            Some(token) => Ok(Some(token)),
            None if self.sessions.is_signed_in(&self.name) => Ok(Some(self.refresh().await?)),
            // Not a failure of the endpoint under test, and worth saying plainly:
            // the answer is a button, not a stack trace.
            None => Err(AuthError::NotSignedIn {
                provider: self.name.clone(),
                detail: "sign in from the auth panel first".to_owned(),
            }),
        }
    }

    async fn invalidate(&self) -> Retry {
        if self.sessions.invalidate(&self.name) {
            debug!(provider = %self.name, "session token rejected, refreshing and replaying once");
            Retry::Once
        } else {
            Retry::No
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(sessions: Arc<SessionStore>) -> OidcBrowserAuth {
        OidcBrowserAuth::new(
            OidcBrowserConfig {
                name: "kc".to_owned(),
                issuer: Url::parse("https://idp.internal/realms/mire").unwrap(),
                authorization_endpoint: Some(Url::parse("https://idp.internal/authorize").unwrap()),
                token_endpoint: Some(Url::parse("https://idp.internal/token").unwrap()),
                client_id: "mire-ui".to_owned(),
                client_secret: None,
                scope: vec!["profile".to_owned()],
                audience: None,
                header: HeaderName::from_static("authorization"),
                scheme: Some("Bearer".to_owned()),
                allowed_hosts: Vec::new(),
            },
            Client::new(),
            sessions,
        )
    }

    #[tokio::test]
    async fn the_authorization_url_carries_pkce_and_a_state() {
        let sessions = Arc::new(SessionStore::default());
        let auth = provider(Arc::clone(&sessions));

        let request = auth
            .start_login("http://127.0.0.1:8787/auth/callback", None)
            .await
            .unwrap();

        let query: std::collections::HashMap<_, _> = request.url.query_pairs().collect();
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["client_id"], "mire-ui");
        assert_eq!(query["redirect_uri"], "http://127.0.0.1:8787/auth/callback");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(query["state"], request.state);

        // The challenge is the hash, never the verifier itself.
        let pending = sessions.take(&request.state).unwrap();
        let expected = BASE64URL.encode(Sha256::digest(pending.verifier.expose().as_bytes()));
        assert_eq!(query["code_challenge"], expected);
        assert_ne!(query["code_challenge"], pending.verifier.expose());
    }

    #[tokio::test]
    async fn openid_is_added_when_the_profile_forgets_it() {
        let sessions = Arc::new(SessionStore::default());
        let auth = provider(Arc::clone(&sessions));

        let request = auth.start_login("http://x/cb", None).await.unwrap();
        let query: std::collections::HashMap<_, _> = request.url.query_pairs().collect();
        assert_eq!(query["scope"], "openid profile");
    }

    #[tokio::test]
    async fn a_prompt_is_passed_through_so_a_stuck_login_can_be_forced() {
        let sessions = Arc::new(SessionStore::default());
        let auth = provider(Arc::clone(&sessions));

        let silent = auth.start_login("http://x/cb", None).await.unwrap();
        assert!(!silent.url.query().unwrap().contains("prompt="));

        // With an established SSO session the identity provider redirects back
        // instantly, so a broken attempt repeats itself with nothing to click.
        // `prompt=login` is what makes it ask again.
        let forced = auth
            .start_login("http://x/cb", Some("login"))
            .await
            .unwrap();
        let query: std::collections::HashMap<_, _> = forced.url.query_pairs().collect();
        assert_eq!(query["prompt"], "login");

        // An empty string is not a prompt; sending `prompt=` would be a protocol error.
        let blank = auth.start_login("http://x/cb", Some("  ")).await.unwrap();
        assert!(!blank.url.query().unwrap().contains("prompt="));
    }

    #[tokio::test]
    async fn a_call_without_a_session_says_to_sign_in() {
        let sessions = Arc::new(SessionStore::default());
        let auth = provider(sessions);

        let mut headers = HeaderMap::new();
        let error = auth
            .apply(
                &mut headers,
                &Url::parse("https://models.internal/v1/chat").unwrap(),
                None,
            )
            .await
            .unwrap_err();

        assert!(matches!(error, AuthError::NotSignedIn { .. }));
        assert!(error.to_string().contains("sign in"));
        assert!(headers.is_empty(), "nothing may be sent unauthenticated");
    }

    #[tokio::test]
    async fn a_live_session_produces_the_header() {
        let sessions = Arc::new(SessionStore::default());
        sessions.store(
            "kc",
            Tokens {
                access_token: Secret::new("the-access-token"),
                refresh_token: None,
                lifetime: Duration::from_secs(300),
                subject: Some("gleroy".to_owned()),
                scope: None,
            },
        );
        let auth = provider(Arc::clone(&sessions));

        let mut headers = HeaderMap::new();
        let redactor = auth
            .apply(
                &mut headers,
                &Url::parse("https://models.internal/v1/chat").unwrap(),
                None,
            )
            .await
            .unwrap();

        assert_eq!(headers["authorization"], "Bearer the-access-token");
        assert!(headers["authorization"].is_sensitive());
        assert!(
            !redactor
                .text("token=the-access-token")
                .contains("the-access-token")
        );
    }

    #[tokio::test]
    async fn allowed_hosts_still_apply() {
        let sessions = Arc::new(SessionStore::default());
        sessions.store(
            "kc",
            Tokens {
                access_token: Secret::new("t"),
                refresh_token: None,
                lifetime: Duration::from_secs(300),
                subject: None,
                scope: None,
            },
        );
        let mut auth = provider(Arc::clone(&sessions));
        auth.allowed_hosts = vec!["models.internal".to_owned()];

        let error = auth
            .apply(
                &mut HeaderMap::new(),
                &Url::parse("https://elsewhere.internal/v1/chat").unwrap(),
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AuthError::HostNotAllowed { .. }));
    }
}
