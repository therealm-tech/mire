//! Browser login sessions, held outside the auth registry.
//!
//! A registry is rebuilt from scratch every time the configuration directory
//! changes — which, in a tool whose selling point is that editing a profile takes
//! effect immediately, would mean being logged out every time you fix a typo. So
//! the tokens live here instead, in a store the reload does not touch, and the
//! providers only borrow it.
//!
//! Nothing in this module is ever serialised whole: [`SessionView`] is what the
//! API is allowed to see, and it carries no token.

use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use schemars::JsonSchema;
use serde::Serialize;
use tracing::debug;

use crate::redact::Secret;

/// How long an unfinished login stays valid. Long enough to type a password and
/// answer an MFA prompt, short enough that abandoned attempts do not pile up.
const PENDING_TTL: Duration = Duration::from_secs(600);

/// Renew this long before expiry, so a request in flight never races it.
const REFRESH_MARGIN: Duration = Duration::from_secs(60);

/// A login that has been started and not yet come back.
#[derive(Debug)]
pub struct Pending {
    /// Provider the login belongs to.
    pub provider: String,
    /// PKCE verifier, sent at the token exchange to prove we started this login.
    pub verifier: Secret,
    /// Exactly the `redirect_uri` sent to the authorization endpoint. RFC 6749
    /// requires the token request to repeat it, byte for byte.
    pub redirect_uri: String,
    /// Where the UI was, so the callback page can say something useful.
    pub started: Instant,
}

/// A completed login.
#[derive(Debug)]
struct Session {
    access_token: Secret,
    refresh_token: Option<Secret>,
    expires_at: Instant,
    /// `true` once handed out by a later call than the one that obtained it. Same
    /// rule as the `client_credentials` cache: a token minted for this very call
    /// getting a `401` is the endpoint's verdict, not a stale credential.
    served_from_cache: bool,
    subject: Option<String>,
    scope: Option<String>,
}

/// What the API may say about a session. No token, ever.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    /// Who the `IdP` says this is, for display only — read out of the `id_token`
    /// without verifying it. Never treat it as a check.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    /// Scopes the `IdP` actually granted, which are not always the ones asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Seconds until the access token needs renewing. `0` once it does.
    pub expires_in_s: u64,
    /// `true` when a refresh token is held, so expiry is silent rather than a
    /// second trip through the browser.
    pub can_refresh: bool,
}

/// Pending logins and live sessions, shared by every browser provider.
#[derive(Debug, Default)]
pub struct SessionStore {
    pending: Mutex<HashMap<String, Pending>>,
    sessions: RwLock<HashMap<String, Session>>,
    /// Why the last login attempt failed, per provider.
    ///
    /// The callback happens in a tab the auth panel does not control, so without
    /// this the panel can only report that the tab went away. Cleared when a new
    /// attempt starts, so what you see always belongs to the attempt you just made.
    failures: RwLock<HashMap<String, String>>,
}

impl SessionStore {
    /// Records a login in flight and returns its `state` parameter.
    ///
    /// The state is the only thing tying the browser's return trip back to this
    /// attempt, so it is random, single-use, and never derived from the input.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn begin(&self, provider: &str, verifier: Secret, redirect_uri: String) -> String {
        // A new attempt supersedes whatever the last one had to say about itself.
        self.failures
            .write()
            .expect("session store lock")
            .remove(provider);

        let state = random_urlsafe();
        let mut guard = self.pending.lock().expect("session store lock");

        // Abandoned attempts are the norm — someone clicks "sign in", changes
        // their mind, closes the tab. Sweep them here rather than on a timer.
        let now = Instant::now();
        guard.retain(|_, entry| now.duration_since(entry.started) < PENDING_TTL);

        guard.insert(
            state.clone(),
            Pending {
                provider: provider.to_owned(),
                verifier,
                redirect_uri,
                started: now,
            },
        );
        state
    }

    /// Consumes a pending login. A state works exactly once.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn take(&self, state: &str) -> Option<Pending> {
        let entry = self
            .pending
            .lock()
            .expect("session store lock")
            .remove(state)?;
        if Instant::now().duration_since(entry.started) >= PENDING_TTL {
            debug!(provider = %entry.provider, "login came back too late");
            return None;
        }
        Some(entry)
    }

    /// Stores a fresh set of tokens for `provider`, replacing any previous one.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn store(&self, provider: &str, tokens: Tokens) {
        let usable = tokens
            .lifetime
            .saturating_sub(REFRESH_MARGIN.min(tokens.lifetime / 2));
        self.failures
            .write()
            .expect("session store lock")
            .remove(provider);
        self.sessions
            .write()
            .expect("session store lock")
            .insert(provider.to_owned(), {
                Session {
                    access_token: tokens.access_token,
                    refresh_token: tokens.refresh_token,
                    expires_at: Instant::now() + usable,
                    served_from_cache: false,
                    subject: tokens.subject,
                    scope: tokens.scope,
                }
            });
    }

    /// The access token, if there is a live session.
    ///
    /// Marks it as reused, which is what makes the `401` replay rule work.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn access_token(&self, provider: &str) -> Option<Secret> {
        let mut guard = self.sessions.write().expect("session store lock");
        let session = guard.get_mut(provider)?;
        if Instant::now() >= session.expires_at {
            return None;
        }
        session.served_from_cache = true;
        Some(session.access_token.clone())
    }

    /// The refresh token, if the `IdP` granted one.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn refresh_token(&self, provider: &str) -> Option<Secret> {
        self.sessions
            .read()
            .expect("session store lock")
            .get(provider)?
            .refresh_token
            .clone()
    }

    /// Records why a login attempt failed, so the panel can say it out loud.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn fail(&self, provider: &str, message: &str) {
        self.failures
            .write()
            .expect("session store lock")
            .insert(provider.to_owned(), message.to_owned());
    }

    /// Why the last attempt failed, if the last attempt did.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn last_failure(&self, provider: &str) -> Option<String> {
        self.failures
            .read()
            .expect("session store lock")
            .get(provider)
            .cloned()
    }

    /// Whether anything is signed in for `provider`, expired or not.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn is_signed_in(&self, provider: &str) -> bool {
        self.sessions
            .read()
            .expect("session store lock")
            .contains_key(provider)
    }

    /// What the API may report about `provider`.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn view(&self, provider: &str) -> Option<SessionView> {
        let guard = self.sessions.read().expect("session store lock");
        let session = guard.get(provider)?;
        Some(SessionView {
            subject: session.subject.clone(),
            scope: session.scope.clone(),
            expires_in_s: session
                .expires_at
                .saturating_duration_since(Instant::now())
                .as_secs(),
            can_refresh: session.refresh_token.is_some(),
        })
    }

    /// Forgets everything about `provider`. Signing out here does not sign you out
    /// of the `IdP` — the next login may well come straight back.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn clear(&self, provider: &str) -> bool {
        self.sessions
            .write()
            .expect("session store lock")
            .remove(provider)
            .is_some()
    }

    /// Drops the access token but keeps the refresh token, so the replay after a
    /// `401` can mint a new one without another trip through the browser.
    ///
    /// Returns `false` — meaning "do not replay" — when the token was minted for
    /// this very call, or when there is no refresh token to use.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic.
    pub fn invalidate(&self, provider: &str) -> bool {
        let mut guard = self.sessions.write().expect("session store lock");
        let Some(session) = guard.get_mut(provider) else {
            return false;
        };
        if !session.served_from_cache || session.refresh_token.is_none() {
            return false;
        }
        // Expire it in place rather than dropping the session: the refresh token
        // is what lets the replay succeed silently.
        session.expires_at = Instant::now();
        true
    }
}

/// A set of tokens as they come back from the `IdP`.
#[derive(Debug)]
pub struct Tokens {
    /// What goes in the header.
    pub access_token: Secret,
    /// What renews it without a browser, when granted.
    pub refresh_token: Option<Secret>,
    /// How long the access token is good for.
    pub lifetime: Duration,
    /// Display name read out of the `id_token`, unverified.
    pub subject: Option<String>,
    /// Scopes actually granted.
    pub scope: Option<String>,
}

/// 32 random bytes, base64url. Used for both the `state` and the PKCE verifier —
/// 43 characters, comfortably inside RFC 7636's 43–128 range.
#[must_use]
pub fn random_urlsafe() -> String {
    // `rand::rng()` is a CSPRNG seeded from the operating system, which is the
    // bar both the `state` and the PKCE verifier have to clear.
    use rand::Rng;

    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    BASE64URL.encode(bytes)
}

/// Pulls a display name out of an `id_token` **without verifying it**.
///
/// This is for the "signed in as …" line and nothing else. The token we actually
/// send is the access token, and whether it is any good is the endpoint's call —
/// which is the entire point of the tool. Trusting an unverified claim for
/// anything else would be a real bug; trusting it for a label is not.
#[must_use]
pub fn subject_of(id_token: &str) -> Option<String> {
    let payload = id_token.split('.').nth(1)?;
    let decoded = BASE64URL.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;

    ["preferred_username", "email", "name", "sub"]
        .iter()
        .find_map(|claim| claims.get(*claim)?.as_str().map(ToOwned::to_owned))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(lifetime_s: u64, refresh: Option<&str>) -> Tokens {
        Tokens {
            access_token: Secret::new("access"),
            refresh_token: refresh.map(Secret::new),
            lifetime: Duration::from_secs(lifetime_s),
            subject: Some("gleroy".to_owned()),
            scope: Some("openid profile".to_owned()),
        }
    }

    #[test]
    fn a_state_can_only_be_used_once() {
        let store = SessionStore::default();
        let state = store.begin("kc", Secret::new("verifier"), "http://x/cb".to_owned());

        assert!(store.take(&state).is_some());
        assert!(
            store.take(&state).is_none(),
            "replaying a state must not work"
        );
    }

    #[test]
    fn an_unknown_state_resolves_to_nothing() {
        let store = SessionStore::default();
        assert!(store.take("not-a-state").is_none());
    }

    #[test]
    fn states_are_not_predictable() {
        let store = SessionStore::default();
        let first = store.begin("kc", Secret::new("v"), "http://x/cb".to_owned());
        let second = store.begin("kc", Secret::new("v"), "http://x/cb".to_owned());
        assert_ne!(first, second);
        assert!(first.len() >= 43, "43 chars is the PKCE floor");
    }

    #[test]
    fn a_session_is_readable_then_clearable() {
        let store = SessionStore::default();
        store.store("kc", tokens(300, Some("refresh")));

        assert!(store.access_token("kc").is_some());
        let view = store.view("kc").unwrap();
        assert_eq!(view.subject.as_deref(), Some("gleroy"));
        assert!(view.can_refresh);
        assert!(view.expires_in_s > 0);

        assert!(store.clear("kc"));
        assert!(store.access_token("kc").is_none());
        assert!(!store.is_signed_in("kc"));
    }

    #[test]
    fn an_expired_access_token_is_not_handed_out() {
        let store = SessionStore::default();
        // Zero lifetime: the margin is capped at half of it, so this expires now.
        store.store("kc", tokens(0, Some("refresh")));

        assert!(store.access_token("kc").is_none());
        // Still signed in, though — the refresh token is what saves the trip.
        assert!(store.is_signed_in("kc"));
        assert!(store.refresh_token("kc").is_some());
    }

    #[test]
    fn a_freshly_minted_token_is_not_replayed() {
        let store = SessionStore::default();
        store.store("kc", tokens(300, Some("refresh")));

        // Never read back, so this call is the one that obtained it.
        assert!(!store.invalidate("kc"));
    }

    #[test]
    fn a_reused_token_is_replayed_once_when_it_can_be_refreshed() {
        let store = SessionStore::default();
        store.store("kc", tokens(300, Some("refresh")));

        let _ = store.access_token("kc");
        assert!(store.invalidate("kc"));
        // The access token is gone, the refresh token is what replays with.
        assert!(store.access_token("kc").is_none());
        assert!(store.refresh_token("kc").is_some());
    }

    #[test]
    fn without_a_refresh_token_a_401_is_the_endpoints_verdict() {
        let store = SessionStore::default();
        store.store("kc", tokens(300, None));

        let _ = store.access_token("kc");
        assert!(!store.invalidate("kc"));
        // And the session survives: nothing is wrong with it.
        assert!(store.access_token("kc").is_some());
    }

    #[test]
    fn a_subject_is_read_out_of_an_id_token() {
        // {"sub":"1234","preferred_username":"gleroy"} — unsigned, which is fine:
        // the signature is not what this is for.
        let payload = BASE64URL.encode(br#"{"sub":"1234","preferred_username":"gleroy"}"#);
        let id_token = format!("header.{payload}.signature");
        assert_eq!(subject_of(&id_token).as_deref(), Some("gleroy"));
    }

    #[test]
    fn a_subject_falls_back_through_the_claims() {
        let payload = BASE64URL.encode(br#"{"sub":"1234"}"#);
        assert_eq!(
            subject_of(&format!("h.{payload}.s")).as_deref(),
            Some("1234")
        );
    }

    #[test]
    fn a_malformed_id_token_is_simply_anonymous() {
        for bad in ["", "not-a-jwt", "h.!!!.s", "h.aGVsbG8.s"] {
            assert!(
                subject_of(bad).is_none(),
                "{bad} should not yield a subject"
            );
        }
    }
}
