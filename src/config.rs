//! Shared, hot-reloading view of the configuration directories.
//!
//! Models, the auth registry, the MCP servers and the saved prompts all come
//! from the same directories and reload together, as one atomic snapshot: a call
//! that starts with a given model also gets the auth registry that was current
//! when it started.
//!
//! What a directory holds is [`layout`]'s business: one subdirectory per kind of
//! thing, one file per entry.
//!
//! There can be more than one directory, and then they are layered in the order
//! given: a name declared twice belongs to the last directory that declared it,
//! and the one it displaced is named in a warning. That is what lets a shared,
//! read-only directory be a base you put your own on top of, instead of
//! something you have to copy before you can change one line of it.
//!
//! Readers take a cheap [`Arc`] snapshot; a reload swaps a whole new one in.

pub mod layout;

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
use crate::model::loader::{self, ModelSet};
use crate::prompt::PromptRegistry;

/// How long the directories must stay quiet before a reload is triggered.
///
/// Editors write in bursts (temp file, rename, chmod); reloading on every event
/// would reload three times per save.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// One consistent view of the configuration directories.
#[derive(Debug, Default)]
pub struct Config {
    /// Models that parsed and validated, plus the files that did not.
    pub models: ModelSet,
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
    /// Every issue found, whichever directory and file it came from.
    pub fn issues(&self) -> impl Iterator<Item = &LoadIssue> {
        self.models
            .issues()
            .iter()
            .chain(self.registry.issues().iter())
            .chain(self.mcp.issues().iter())
            .chain(self.prompts.issues().iter())
    }
}

/// The configuration directories and their current contents.
#[derive(Debug)]
pub struct ConfigStore {
    dirs: Vec<PathBuf>,
    /// Handed to OIDC providers on every load, so their token exchanges use the
    /// same client — and therefore the same CA bundle — as everything else.
    http: Client,
    /// Browser logins, deliberately *outside* the snapshot. Everything else in
    /// these directories is declarative and can be rebuilt from the files; a session
    /// cannot, and losing it on every save would make the flow unusable.
    sessions: Arc<SessionStore>,
    current: RwLock<Arc<Config>>,
}

impl ConfigStore {
    /// Performs the initial load of `dirs`, layered in the order given.
    ///
    /// # Errors
    ///
    /// Fails only if one of the directories cannot be read — the error names
    /// which. Broken models and broken auth entries are recorded as issues, not
    /// returned as errors.
    pub fn load(dirs: &[impl AsRef<Path>], http: Client) -> std::io::Result<Arc<Self>> {
        // A slice rather than an `IntoIterator`: `&Path` is itself iterable, over
        // its own components, so the looser signature would happily accept a
        // single directory and read each of its segments as one.
        let dirs: Vec<PathBuf> = dirs.iter().map(|dir| dir.as_ref().to_path_buf()).collect();
        let sessions = Arc::new(SessionStore::default());
        let config = read(&dirs, &http, &sessions)?;
        Ok(Arc::new(Self {
            dirs,
            http,
            sessions,
            current: RwLock::new(Arc::new(config)),
        }))
    }

    /// The directories being watched, in precedence order — last one wins.
    #[must_use]
    pub fn dirs(&self) -> &[PathBuf] {
        &self.dirs
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

    /// Re-reads the directories and swaps the result in. Errors are logged and
    /// the previous snapshot is kept: a transiently unreadable directory must not
    /// blank the UI.
    ///
    /// # Panics
    ///
    /// Panics if the lock was poisoned by a previous panic while reloading.
    pub fn reload(&self) {
        match read(&self.dirs, &self.http, &self.sessions) {
            Ok(config) => {
                info!(
                    models = config.models.len(),
                    providers = config.registry.descriptors().len(),
                    prompts = config.prompts.len(),
                    issues = config.issues().count(),
                    dirs = %describe(&self.dirs),
                    "configuration reloaded"
                );
                for issue in config.issues() {
                    warn!(%issue, "configuration issue");
                }
                *self.current.write().expect("config store lock") = Arc::new(config);
            }
            Err(error) => {
                error!(%error, "reload failed, keeping the previous configuration");
            }
        }
    }
}

/// The directory list as one field value, for a log line.
#[must_use]
pub fn describe(dirs: &[PathBuf]) -> String {
    dirs.iter()
        .map(|dir| dir.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn read(dirs: &[PathBuf], http: &Client, sessions: &Arc<SessionStore>) -> std::io::Result<Config> {
    // The one thing worth refusing to start over. Everything *inside* a
    // configuration directory is reported and skipped, but a directory that is
    // not there at all is a typo in `--config-dir`, and carrying on would show an
    // empty UI as if the typo were a fact about the machine. The error names the
    // directory: with a list, "no such file or directory" does not say which.
    for dir in dirs {
        std::fs::read_dir(dir).map_err(|error| {
            std::io::Error::new(error.kind(), format!("`{}`: {error}", dir.display()))
        })?;
    }

    Ok(Config {
        models: loader::load_dirs(dirs),
        registry: AuthRegistry::load_dirs(dirs, http, sessions),
        mcp: McpRegistry::load_dirs(dirs, http),
        prompts: PromptRegistry::load_dirs(dirs),
    })
}

/// Watches every configuration directory and reloads `store` on change.
///
/// The returned [`RecommendedWatcher`] must be kept alive: dropping it stops the
/// watch. Hand it to the caller rather than leaking it, so shutdown is clean.
///
/// # Errors
///
/// Fails if the platform watcher cannot be created or a directory cannot be watched.
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
    // Recursive: what changes is a file in `models/` or `auth/`, one level down.
    for dir in store.dirs() {
        watcher.watch(dir, RecursiveMode::Recursive)?;
    }

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

    /// A configuration directory with every subdirectory in place, the way one
    /// somebody keeps looks.
    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-config-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for kind in [layout::MODELS, layout::AUTH, layout::MCP, layout::PROMPTS] {
            std::fs::create_dir_all(dir.join(kind)).unwrap();
        }
        dir
    }

    fn write(dir: &Path, kind: &str, name: &str, body: &str) {
        std::fs::write(dir.join(kind).join(name), body).unwrap();
    }

    const MODEL: &str =
        "name: late\nkind: chat\nurl: https://models.internal/late\nrequest:\n  template: '{}'\n";

    #[test]
    fn a_reload_picks_up_a_new_model() {
        let dir = temp_dir("model");
        let store = ConfigStore::load(std::slice::from_ref(&dir), Client::new()).unwrap();
        assert!(store.snapshot().models.is_empty());

        write(&dir, layout::MODELS, "late.yaml", MODEL);
        store.reload();

        assert_eq!(store.snapshot().models.len(), 1);
    }

    #[test]
    fn a_reload_picks_up_a_new_auth_provider() {
        let dir = temp_dir("auth");
        let store = ConfigStore::load(std::slice::from_ref(&dir), Client::new()).unwrap();
        assert!(store.snapshot().registry.get("gateway").is_none());

        write(
            &dir,
            layout::AUTH,
            "gateway.yaml",
            "name: gateway\nkind: token\nvalue:\n  env: MODEL_TOKEN\n",
        );
        store.reload();

        assert!(store.snapshot().registry.get("gateway").is_some());
    }

    #[test]
    fn a_reload_picks_up_a_new_prompt() {
        let dir = temp_dir("prompt");
        let store = ConfigStore::load(std::slice::from_ref(&dir), Client::new()).unwrap();
        assert!(store.snapshot().prompts.is_empty());

        write(
            &dir,
            layout::PROMPTS,
            "ping.yaml",
            "name: ping\ntext: ping\n",
        );
        store.reload();

        assert_eq!(store.snapshot().prompts.prompts()[0].text, "ping");
    }

    /// A directory that is not there at all is a typo in `--config-dir`, and the
    /// one thing in here worth refusing to start over.
    #[test]
    fn a_directory_that_cannot_be_read_is_named_in_the_error() {
        let base = temp_dir("missing");
        let missing = base.join("nowhere");

        let error = ConfigStore::load(&[&base, &missing], Client::new()).unwrap_err();

        assert!(error.to_string().contains("nowhere"), "{error}");
    }

    /// The subdirectories are another matter: a configuration directory holding
    /// only models is the ordinary case, not a broken one.
    #[test]
    fn a_directory_with_no_subdirectories_still_loads() {
        let dir = std::env::temp_dir().join(format!("mire-config-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let store = ConfigStore::load(std::slice::from_ref(&dir), Client::new()).unwrap();

        let config = store.snapshot();
        assert!(config.models.is_empty());
        assert!(config.prompts.is_empty());
        assert_eq!(config.issues().count(), 0);
        // The one thing that exists without being declared anywhere.
        assert!(config.registry.get("anonymous").is_some());
    }

    #[test]
    fn a_broken_auth_provider_leaves_anonymous_working_and_reports_the_problem() {
        let dir = temp_dir("broken-auth");
        write(&dir, layout::AUTH, "broken.yaml", "name: [unclosed\n");

        let store = ConfigStore::load(std::slice::from_ref(&dir), Client::new()).unwrap();
        let config = store.snapshot();

        assert!(config.registry.get("anonymous").is_some());
        assert_eq!(config.issues().count(), 1);
    }

    #[test]
    fn models_and_providers_swap_together() {
        let dir = temp_dir("atomic");
        let store = ConfigStore::load(std::slice::from_ref(&dir), Client::new()).unwrap();

        write(&dir, layout::MODELS, "late.yaml", MODEL);
        write(
            &dir,
            layout::AUTH,
            "gateway.yaml",
            "name: gateway\nkind: anonymous\n",
        );
        store.reload();

        let config = store.snapshot();
        assert!(config.models.get("late").is_some());
        assert!(config.registry.get("gateway").is_some());
    }

    #[test]
    fn a_later_directory_wins_a_name_and_both_are_watched() {
        let base = temp_dir("layer-base");
        let mine = temp_dir("layer-mine");
        write(&base, layout::MODELS, "late.yaml", MODEL);
        write(
            &mine,
            layout::MODELS,
            "late.yaml",
            &MODEL.replace(
                "https://models.internal/late",
                "https://staging.internal/late",
            ),
        );

        let store = ConfigStore::load(&[&base, &mine], Client::new()).unwrap();

        assert_eq!(store.dirs(), [base, mine.clone()]);
        let config = store.snapshot();
        assert_eq!(config.models.len(), 1);
        assert_eq!(
            config.models.get("late").unwrap().url.as_str(),
            "https://staging.internal/late"
        );

        // And the win survives a reload, rather than depending on which
        // directory happened to be read first.
        write(
            &mine,
            layout::MODELS,
            "late.yaml",
            &MODEL.replace(
                "https://models.internal/late",
                "https://other.internal/late",
            ),
        );
        store.reload();
        assert_eq!(
            store.snapshot().models.get("late").unwrap().url.as_str(),
            "https://other.internal/late"
        );
    }

    #[test]
    fn a_reload_does_not_sign_you_out() {
        use std::time::Duration;

        use crate::auth::session::Tokens;
        use crate::redact::Secret;

        let dir = temp_dir("session");
        write(
            &dir,
            layout::AUTH,
            "kc.yaml",
            "name: kc\nkind: oidc_browser\nissuer: https://idp.internal/realms/mire\nclient_id: mire-ui\n",
        );
        let store = ConfigStore::load(std::slice::from_ref(&dir), Client::new()).unwrap();

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

        // Editing a model rebuilds the registry from scratch. The session must
        // not be collateral damage — that is the whole reason it lives outside.
        write(&dir, layout::MODELS, "late.yaml", MODEL);
        store.reload();

        assert_eq!(store.snapshot().models.len(), 1);
        assert!(store.sessions().access_token("kc").is_some());
    }
}
