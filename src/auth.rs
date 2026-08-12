//! Authentication modes, orthogonal to the profile.
//!
//! The same model must be testable anonymously, with a static token and with a
//! workload identity, without duplicating its profile — so auth lives in its own
//! registry ([`auth.yaml`](crate::profile::loader::AUTH_REGISTRY_FILE)) and a
//! profile only refers to it by name.
//!
//! Anonymous is a first-class mode, not the absence of one: hitting a protected
//! route with no credential and getting a `401` is a *passing* check.

pub mod anonymous;
pub mod browser;
pub mod oidc;
pub mod registry;
pub mod session;
pub mod token;

use std::future::Future;

use reqwest::header::HeaderMap;
use url::Url;

use crate::redact::Redactor;

pub use anonymous::Anonymous;
pub use browser::{CALLBACK_PATH, OidcBrowserAuth, OidcBrowserConfig};
pub use oidc::{ClientCredential, OidcAuth, OidcConfig};
pub use registry::{ANONYMOUS, AuthRegistry};
pub use session::{SessionStore, SessionView};
pub use token::{TokenAuth, TokenValue};

/// Whether a `401` is worth one more attempt with a fresh credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retry {
    /// The credential was refreshed; replay the request exactly once.
    Once,
    /// Nothing to refresh. A `401` here is the endpoint's answer, not our problem.
    No,
}

/// Something that turns a request into an authenticated request.
///
/// The only trait in the decode/render/auth trio, because it is the only one with
/// state (a token cache), real I/O to mock, and a non-trivial retry contract.
pub trait AuthProvider {
    /// Registry name, used in the API and the UI.
    fn name(&self) -> &str;

    /// Injects the credential headers for `target`.
    ///
    /// Returns a [`Redactor`] covering exactly what was injected, so the caller
    /// cannot forget to scrub it from traces and responses.
    fn apply(
        &self,
        headers: &mut HeaderMap,
        target: &Url,
        supplied: Option<&crate::redact::Secret>,
    ) -> impl Future<Output = Result<Redactor, AuthError>> + Send;

    /// The bare credential for `target`, with no header and no scheme around it.
    ///
    /// [`Self::apply`] puts the credential where the *provider* says it goes,
    /// which is right almost always. This is for the case it is not: a header
    /// template that needs the token somewhere else, or inside a larger value.
    ///
    /// `None` means the provider has no credential to give — `anonymous`, and
    /// only `anonymous`. Everything else either produces one or fails saying why.
    ///
    /// The host allow-list applies here exactly as it does to [`Self::apply`]:
    /// "this credential may only be sent to these hosts" is a rule about the
    /// credential, not about the shape of the request carrying it.
    fn credential(
        &self,
        target: &Url,
        supplied: Option<&crate::redact::Secret>,
    ) -> impl Future<Output = Result<Option<crate::redact::Secret>, AuthError>> + Send;

    /// Called on a `401`. Drops any cached credential and says whether replaying
    /// is worth it. Must return [`Retry::Once`] at most once per request.
    fn invalidate(&self) -> impl Future<Output = Retry> + Send;
}

/// Runtime-selected auth provider.
///
/// An enum rather than `Box<dyn AuthProvider>`: the set of modes is closed, and
/// this keeps dispatch static.
#[derive(Debug)]
pub enum Auth {
    /// No credential at all.
    Anonymous(Anonymous),
    /// A static token from an environment variable, a file, or the UI.
    Token(TokenAuth),
    /// An access token fetched with `client_credentials`, the mode that
    /// reproduces what a workload actually does.
    Oidc(Box<OidcAuth>),
    /// An access token obtained by a human signing in through their browser.
    OidcBrowser(Box<OidcBrowserAuth>),
}

impl AuthProvider for Auth {
    fn name(&self) -> &str {
        match self {
            Self::Anonymous(provider) => provider.name(),
            Self::Token(provider) => provider.name(),
            Self::Oidc(provider) => provider.name(),
            Self::OidcBrowser(provider) => provider.name(),
        }
    }

    async fn apply(
        &self,
        headers: &mut HeaderMap,
        target: &Url,
        supplied: Option<&crate::redact::Secret>,
    ) -> Result<Redactor, AuthError> {
        match self {
            Self::Anonymous(provider) => provider.apply(headers, target, supplied).await,
            Self::Token(provider) => provider.apply(headers, target, supplied).await,
            Self::Oidc(provider) => provider.apply(headers, target, supplied).await,
            Self::OidcBrowser(provider) => provider.apply(headers, target, supplied).await,
        }
    }

    async fn credential(
        &self,
        target: &Url,
        supplied: Option<&crate::redact::Secret>,
    ) -> Result<Option<crate::redact::Secret>, AuthError> {
        match self {
            Self::Anonymous(provider) => provider.credential(target, supplied).await,
            Self::Token(provider) => provider.credential(target, supplied).await,
            Self::Oidc(provider) => provider.credential(target, supplied).await,
            Self::OidcBrowser(provider) => provider.credential(target, supplied).await,
        }
    }

    async fn invalidate(&self) -> Retry {
        match self {
            Self::Anonymous(provider) => provider.invalidate().await,
            Self::Token(provider) => provider.invalidate().await,
            Self::Oidc(provider) => provider.invalidate().await,
            Self::OidcBrowser(provider) => provider.invalidate().await,
        }
    }
}

/// Why a credential could not be produced.
///
/// No variant ever carries the credential itself.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The profile refers to an auth entry that is not declared.
    #[error("unknown auth provider `{0}`")]
    UnknownProvider(String),

    /// `value.env` names a variable that is not set.
    #[error("auth `{provider}`: environment variable `{variable}` is not set")]
    MissingEnv {
        /// Registry name of the provider.
        provider: String,
        /// Variable that was expected.
        variable: String,
    },

    /// `value.file` could not be read. Re-read on every call, to follow rotation.
    #[error("auth `{provider}`: cannot read token file `{path}`: {source}")]
    TokenFile {
        /// Registry name of the provider.
        provider: String,
        /// Path that was attempted.
        path: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// Nothing to send: no `env`, no `file`, and nothing supplied with the request.
    #[error(
        "auth `{provider}`: no credential available; set `value.env` or `value.file` in auth.yaml, or supply one with the request"
    )]
    NoCredential {
        /// Registry name of the provider.
        provider: String,
    },

    /// The provider restricts where its credential may be sent, and this is not it.
    #[error("auth `{provider}`: host `{host}` is not in `allowed_hosts`")]
    HostNotAllowed {
        /// Registry name of the provider.
        provider: String,
        /// Host of the target URL.
        host: String,
    },

    /// The credential contains bytes that cannot go in an HTTP header.
    #[error("auth `{provider}`: the credential is not a valid HTTP header value")]
    InvalidHeaderValue {
        /// Registry name of the provider.
        provider: String,
    },

    /// The OIDC discovery document could not be fetched or read.
    #[error("auth `{provider}`: discovery failed at `{url}`: {message}")]
    Discovery {
        /// Registry name of the provider.
        provider: String,
        /// The discovery URL that was tried.
        url: String,
        /// What went wrong.
        message: String,
    },

    /// The token exchange failed. Never carries the credential.
    #[error("auth `{provider}`: {message}")]
    TokenExchange {
        /// Registry name of the provider.
        provider: String,
        /// What the token endpoint said, scrubbed.
        message: String,
    },

    /// A browser provider was used before anyone signed in, or after the session
    /// died beyond refreshing. Not an endpoint failure — an instruction.
    #[error("auth `{provider}`: not signed in — {detail}")]
    NotSignedIn {
        /// Registry name of the provider.
        provider: String,
        /// What to do about it.
        detail: String,
    },

    /// The browser came back with something that does not match a login we
    /// started. Expired, already used, or not ours.
    #[error(
        "the login could not be matched to a pending request: it expired, was already completed, or did not start here"
    )]
    UnknownLoginState,

    /// The identity provider refused the login and said why.
    #[error("auth `{provider}`: the identity provider refused the login — {message}")]
    LoginRefused {
        /// Registry name of the provider.
        provider: String,
        /// The `error` / `error_description` pair, as sent.
        message: String,
    },

    /// The provider named exists but does not sign in through a browser.
    #[error("auth `{provider}`: this provider does not use a browser login")]
    NotABrowserProvider {
        /// Registry name of the provider.
        provider: String,
    },

    /// The callback URL could not be worked out, or is not usable.
    #[error("cannot use `{uri}` as a redirect URI: {reason}")]
    BadRedirectUri {
        /// What was proposed.
        uri: String,
        /// Why it was rejected.
        reason: String,
    },
}

/// Rejects a target whose host is outside a provider's `allowed_hosts`.
///
/// An empty list means "no restriction", which is the default: pointing at an
/// arbitrary endpoint is the feature.
fn check_allowed_host(provider: &str, allowed: &[String], target: &Url) -> Result<(), AuthError> {
    if allowed.is_empty() {
        return Ok(());
    }
    let host = target.host_str().unwrap_or_default();
    if allowed.iter().any(|candidate| candidate == host) {
        Ok(())
    } else {
        Err(AuthError::HostNotAllowed {
            provider: provider.to_owned(),
            host: host.to_owned(),
        })
    }
}
