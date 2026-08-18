//! Shared, hot-reloading view of the configuration directory.
//!
//! Profiles, the auth registry, the MCP servers and the saved prompts all come
//! from the same directory and reload together, as one atomic snapshot: a call
//! that starts with a given profile also gets the auth registry that was current
//! when it started.
//!
//! Readers take a cheap [`Arc`] snapshot; a reload swaps a whole new one in.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher};
use reqwest::Client;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use crate::auth::{AuthRegistry, SessionStore};
use crate::issue::LoadIssue;
use crate::mcp::McpRegistry;
use crate::profile::loader::{self, ProfileSet};
use crate::prompt::PromptRegistry;

/// How long the directory must stay quiet before a reload is triggered.
///
/// Editors write in bursts (temp file, rename, chmod); reloading on every event
/// would reload three times per save.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// One consistent view of the configuration directory.
#[derive(Debug, Default)]
pub struct Config {
    /// Profiles that parsed and validated, plus the files that did not.
    pub profiles: ProfileSet,
    /// Auth providers, plus the entries that did not load.
    pub registry: AuthRegistry,
    /// MCP servers agent mode may call for real, plus the entries that did not
    /// load.
    pub mcp: McpRegistry,
    /// Saved prompts the UI can drop in the box, plus the entries that did not
    /// load.
    pub prompts: PromptRegistry,
}

impl Config {
    /// Every issue found in the directory, whichever file it came from.
    pub fn issues(&self) -> impl Iterator<Item = &LoadIssue> {
        self.profiles
            .issues()
            .iter()
            .chain(self.registry.issues().iter())
            .chain(self.mcp.issues().iter())
            .chain(self.prompts.issues().iter())
    }
}

/// The configuration directory and its current contents.
#[derive(Debug)]
pub struct ConfigStore {
    dir: PathBuf,
    /// Handed to OIDC providers on every load, so their token exchanges use the
    /// same client — and therefore the same CA bundle — as everything else.
    http: Client,
    /// Browser logins, deliberately *outside* the snapshot. Everything else in
    /// this directory is declarative and can be rebuilt from the files; a session
    /// cannot, and losing it on every save would make the flow unusable.
    sessions: Arc<SessionStore>,
    current: RwLock<Arc<Config>>,
}

impl ConfigStore {
    /// Performs the initial load of `dir`.
    ///
    /// # Errors
    ///
    /// Fails only if the directory cannot be read. Broken profiles and broken auth
    /// entries are recorded as issues, not returned as errors.
    pub fn load(dir: impl Into<PathBuf>, http: Client) -> std::io::Result<Arc<Self>> {
        let dir = dir.into();
        let sessions = Arc::new(SessionStore::default());
        let config = read(&dir, &http, &sessions)?;
        Ok(Arc::new(Self {
            dir,
            http,
            sessions,
            current: RwLock::new(Arc::new(config)),
        }))
    }

    /// The directory being watched.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Browser login sessions. Survives reloads; see the field's comment.
    #[must_use]
    pub fn sessions(&self) -> &Arc<SessionStore> {
        &self.sessions
    }

    /// Current contents. Cheap; call it per request rather than holding it.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic while reloading.
    #[must_use]
    pub fn snapshot(&self) -> Arc<Config> {
        Arc::clone(&self.current.read().expect("config store lock"))
    }

    /// Re-reads the directory and swaps the result in. Errors are logged and the
    /// previous snapshot is kept: a transiently unreadable directory must not
    /// blank the UI.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic while reloading.
    pub fn reload(&self) {
        match read(&self.dir, &self.http, &self.sessions) {
            Ok(config) => {
                info!(
                    profiles = config.profiles.len(),
                    providers = config.registry.descriptors().len(),
                    prompts = config.prompts.len(),
                    issues = config.issues().count(),
                    dir = %self.dir.display(),
                    "configuration reloaded"
                );
                for issue in config.issues() {
                    warn!(%issue, "configuration issue");
                }
                *self.current.write().expect("config store lock") = Arc::new(config);
            }
            Err(error) => {
                error!(%error, dir = %self.dir.display(), "reload failed, keeping the previous configuration");
            }
        }
    }
}

fn read(dir: &Path, http: &Client, sessions: &Arc<SessionStore>) -> std::io::Result<Config> {
    Ok(Config {
        profiles: loader::load_dir(dir)?,
        registry: AuthRegistry::load(dir, http, sessions),
        mcp: McpRegistry::load(dir, http),
        prompts: PromptRegistry::load(dir),
    })
}

/// Watches the configuration directory and reloads `store` on change.
///
/// The returned [`RecommendedWatcher`] must be kept alive: dropping it stops the
/// watch. Hand it to the caller rather than leaking it, so shutdown is clean.
///
/// # Errors
///
/// Fails if the platform watcher cannot be created or the directory cannot be watched.
pub fn watch(store: Arc<ConfigStore>) -> notify::Result<RecommendedWatcher> {
    let (tx, mut rx) = mpsc::unbounded_channel::<()>();

    let mut watcher =
        notify::recommended_watcher(move |event: notify::Result<Event>| match event {
            Ok(event) if event.kind.is_access() => {}
            Ok(event) => {
                debug!(kind = ?event.kind, "configuration directory changed");
                let _ = tx.send(());
            }
            Err(error) => error!(%error, "configuration watcher error"),
        })?;
    watcher.watch(store.dir(), RecursiveMode::NonRecursive)?;

    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            // Coalesce the burst an editor produces for a single save.
            loop {
                tokio::select! {
                    event = rx.recv() => {
                        if event.is_none() {
                            return;
                        }
                    }
                    () = tokio::time::sleep(DEBOUNCE) => break,
                }
            }
            store.reload();
        }
    });

    Ok(watcher)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const PROFILE: &str =
        "name: late\nkind: chat\nurl: https://models.internal/late\nrequest:\n  template: '{}'\n";

    #[test]
    fn a_reload_picks_up_a_new_profile() {
        let dir = temp_dir("profile");
        let store = ConfigStore::load(&dir, Client::new()).unwrap();
        assert!(store.snapshot().profiles.is_empty());

        std::fs::write(dir.join("late.yaml"), PROFILE).unwrap();
        store.reload();

        assert_eq!(store.snapshot().profiles.len(), 1);
    }

    #[test]
    fn a_reload_picks_up_a_new_auth_provider() {
        let dir = temp_dir("auth");
        let store = ConfigStore::load(&dir, Client::new()).unwrap();
        assert!(store.snapshot().registry.get("gateway").is_none());

        std::fs::write(
            dir.join("auth.yaml"),
            "providers:\n  - name: gateway\n    kind: token\n    value:\n      env: MODEL_TOKEN\n",
        )
        .unwrap();
        store.reload();

        assert!(store.snapshot().registry.get("gateway").is_some());
    }

    #[test]
    fn a_reload_picks_up_a_new_prompt() {
        let dir = temp_dir("prompt");
        let store = ConfigStore::load(&dir, Client::new()).unwrap();
        assert!(store.snapshot().prompts.is_empty());

        std::fs::write(
            dir.join("prompts.yaml"),
            "prompts:\n  - name: ping\n    text: ping\n",
        )
        .unwrap();
        store.reload();

        assert_eq!(store.snapshot().prompts.prompts()[0].text, "ping");
    }

    #[test]
    fn a_broken_auth_registry_leaves_anonymous_working_and_reports_the_problem() {
        let dir = temp_dir("broken-auth");
        std::fs::write(dir.join("auth.yaml"), "providers: [unclosed\n").unwrap();

        let store = ConfigStore::load(&dir, Client::new()).unwrap();
        let config = store.snapshot();

        assert!(config.registry.get("anonymous").is_some());
        assert_eq!(config.issues().count(), 1);
    }

    #[test]
    fn profiles_and_providers_swap_together() {
        let dir = temp_dir("atomic");
        let store = ConfigStore::load(&dir, Client::new()).unwrap();

        std::fs::write(dir.join("late.yaml"), PROFILE).unwrap();
        std::fs::write(
            dir.join("auth.yaml"),
            "providers:\n  - name: gateway\n    kind: anonymous\n",
        )
        .unwrap();
        store.reload();

        let config = store.snapshot();
        assert!(config.profiles.get("late").is_some());
        assert!(config.registry.get("gateway").is_some());
    }

    #[test]
    fn a_reload_does_not_sign_you_out() {
        use std::time::Duration;

        use crate::auth::session::Tokens;
        use crate::redact::Secret;

        let dir = temp_dir("session");
        std::fs::write(
            dir.join("auth.yaml"),
            "providers:\n  - name: kc\n    kind: oidc_browser\n    issuer: https://idp.internal/realms/mire\n    client_id: mire-ui\n",
        )
        .unwrap();
        let store = ConfigStore::load(&dir, Client::new()).unwrap();

        store.sessions().store(
            "kc",
            Tokens {
                access_token: Secret::new("access"),
                refresh_token: None,
                lifetime: Duration::from_secs(300),
                subject: Some("gleroy".to_owned()),
                scope: None,
            },
        );

        // Editing a profile rebuilds the registry from scratch. The session must
        // not be collateral damage — that is the whole reason it lives outside.
        std::fs::write(dir.join("late.yaml"), PROFILE).unwrap();
        store.reload();

        assert_eq!(store.snapshot().profiles.len(), 1);
        assert!(store.sessions().access_token("kc").is_some());
    }
}
