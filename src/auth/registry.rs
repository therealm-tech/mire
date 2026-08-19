//! The auth registry: `auth.yaml`, sitting next to the profiles.
//!
//! Separate from the profiles so that one model can be replayed against every mode
//! without duplicating its file. Credentials are never *in* here — only where to
//! find them.
//!
//! Loading follows the same policy as profiles: one bad entry is reported and
//! skipped, the rest still work, and [`ANONYMOUS`] always exists. A registry you
//! cannot load at all is exactly when you most want the tool to come up and tell
//! you why. Layered directories follow the profiles' rule too: a provider
//! declared in two of them is the later one's, said out loud in the log.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use reqwest::Client;
use reqwest::header::HeaderName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use url::Url;

use super::browser::{OidcBrowserAuth, OidcBrowserConfig};
use super::oidc::{ClientCredential, OidcAuth, OidcConfig};
use super::session::{SessionStore, SessionView};
use super::token::{TokenAuth, TokenValue};
use super::{Anonymous, Auth, AuthProvider};
use crate::issue::LoadIssue;
use crate::profile::loader::AUTH_REGISTRY_FILE;

/// Name of the always-available anonymous provider.
pub const ANONYMOUS: &str = "anonymous";

const DEFAULT_HEADER: &str = "authorization";
const DEFAULT_SCHEME: &str = "Bearer";

/// What kind of credential a provider sends. Surfaced to the UI's auth selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuthKind {
    /// Sends nothing.
    Anonymous,
    /// Sends a static token.
    Token,
    /// Fetches an access token with `client_credentials`.
    Oidc,
    /// Obtains an access token by sending a human through their browser.
    OidcBrowser,
}

/// A provider as advertised to the UI. Carries no credential.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthDescriptor {
    /// Registry name, used in `POST /api/call`.
    pub name: String,
    /// What it sends.
    pub kind: AuthKind,
    /// `true` when the UI must prompt for the value: the registry declares neither
    /// `value.env` nor `value.file`.
    pub needs_value: bool,
    /// `true` when using this provider requires signing in through a browser
    /// first, so the UI knows to offer the button.
    pub needs_login: bool,
    /// Hosts this credential may be sent to. Empty — the default — means
    /// anywhere.
    ///
    /// Advertised so the UI can stop offering a provider against a profile it
    /// could never authenticate. The rule itself is enforced here, on every
    /// call; this is the same statement said early enough to be a choice rather
    /// than an error.
    pub allowed_hosts: Vec<String>,
    /// The live session, for a browser provider that has one. Filled in per
    /// request rather than at load time — the session outlives the registry.
    /// Never carries a token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionView>,
    /// Why the last browser login failed, when one did. The callback happens in
    /// a tab the UI does not control, so this is how the reason gets home.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl AuthDescriptor {
    /// A descriptor for a provider of `kind`, pinned to `allowed_hosts`.
    ///
    /// `needs_login` follows from the kind rather than being set alongside it:
    /// a browser flow is the only credential a human has to go and fetch, so
    /// there is nothing here for the two to disagree about.
    fn new(name: &str, kind: AuthKind, allowed_hosts: &[String]) -> Self {
        Self {
            name: name.to_owned(),
            kind,
            needs_value: false,
            needs_login: matches!(kind, AuthKind::OidcBrowser),
            allowed_hosts: allowed_hosts.to_vec(),
            session: None,
            last_error: None,
        }
    }

    /// The registry declares no source, so the UI has to prompt.
    fn prompting(mut self) -> Self {
        self.needs_value = true;
        self
    }
}

/// Every declared auth provider, keyed by name.
#[derive(Debug)]
pub struct AuthRegistry {
    providers: BTreeMap<String, Auth>,
    descriptors: Vec<AuthDescriptor>,
    /// Which file each provider came from, so that a second declaration can be
    /// told apart from a second *file* declaring it. One is a typo, the other is
    /// a layered directory doing its job.
    sources: BTreeMap<String, PathBuf>,
    issues: Vec<LoadIssue>,
}

impl Default for AuthRegistry {
    fn default() -> Self {
        Self::with_builtins()
    }
}

impl AuthRegistry {
    /// Loads `auth.yaml` from each of the profile directories, in order.
    ///
    /// `http` is the shared client, handed to OIDC providers so that discovery and
    /// the token exchange go through the same CA bundle and redirect policy as
    /// every other outbound call. `sessions` is the browser-login store, which
    /// deliberately outlives this registry: a reload must not sign anyone out.
    ///
    /// Never fails. No file anywhere gives you [`ANONYMOUS`] alone; an unreadable
    /// or malformed one gives you the same plus an issue explaining it; a single
    /// bad provider is skipped and reported while the others load.
    #[must_use]
    pub fn load_dirs(
        dirs: &[impl AsRef<Path>],
        http: &Client,
        sessions: &Arc<SessionStore>,
    ) -> Self {
        let mut registry = Self::with_builtins();
        for dir in dirs {
            registry.read(&dir.as_ref().join(AUTH_REGISTRY_FILE), http, sessions);
        }
        registry
    }

    /// Loads `auth.yaml` from a single profiles directory.
    #[must_use]
    pub fn load(dir: &Path, http: &Client, sessions: &Arc<SessionStore>) -> Self {
        Self::load_dirs(&[dir], http, sessions)
    }

    /// Folds one `auth.yaml` in, on top of whatever earlier directories declared.
    fn read(&mut self, path: &Path, http: &Client, sessions: &Arc<SessionStore>) {
        if !path.exists() {
            debug!(path = %path.display(), "no auth registry, anonymous only");
            return;
        }

        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                self.issues.push(LoadIssue::new(path, error.to_string()));
                return;
            }
        };

        let file: RegistryFile = match serde_yaml_ng::from_str(&text) {
            Ok(file) => file,
            Err(error) => {
                self.issues.push(LoadIssue::from_yaml(path, &error));
                return;
            }
        };

        for config in file.providers {
            if let Err(issue) = self.insert(path, config, http, sessions) {
                self.issues.push(issue);
            }
        }
    }

    /// A registry holding only the built-in anonymous provider.
    #[must_use]
    pub fn with_builtins() -> Self {
        let mut registry = Self {
            providers: BTreeMap::new(),
            descriptors: Vec::new(),
            sources: BTreeMap::new(),
            issues: Vec::new(),
        };
        registry.providers.insert(
            ANONYMOUS.to_owned(),
            Auth::Anonymous(Anonymous::new(ANONYMOUS, Vec::new())),
        );
        registry
            .descriptors
            .push(AuthDescriptor::new(ANONYMOUS, AuthKind::Anonymous, &[]));
        registry
    }

    /// Looks a provider up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Auth> {
        self.providers.get(name)
    }

    /// Every provider, for the UI's auth selector.
    #[must_use]
    pub fn descriptors(&self) -> &[AuthDescriptor] {
        &self.descriptors
    }

    /// Entries that could not be loaded, and why.
    #[must_use]
    pub fn issues(&self) -> &[LoadIssue] {
        &self.issues
    }

    fn insert(
        &mut self,
        path: &Path,
        config: ProviderConfig,
        http: &Client,
        sessions: &Arc<SessionStore>,
    ) -> Result<(), LoadIssue> {
        let (name, provider, descriptor) = build(path, config, http, sessions)?;

        match self.sources.get(&name) {
            // Same file twice. Redeclaring `anonymous` is allowed — it is how you
            // scope it with `allowed_hosts`. Any other collision is a mistake.
            Some(previous) if previous == path => {
                if name != ANONYMOUS {
                    return Err(LoadIssue::new(
                        path,
                        format!("duplicate auth provider `{name}`"),
                    ));
                }
            }
            // A later directory redeclaring it, which is what layering is for.
            // Not an issue, but not silent either: an OIDC provider quietly
            // swapped for a token one is a long afternoon.
            Some(previous) => warn!(
                %name,
                path = %path.display(),
                shadowed = %previous.display(),
                "auth provider overridden by a later directory"
            ),
            None => {}
        }

        debug!(name = %provider.name(), "auth provider registered");
        self.descriptors.retain(|existing| existing.name != name);
        self.descriptors.push(descriptor);
        self.descriptors.sort_by(|a, b| a.name.cmp(&b.name));
        self.sources.insert(name.clone(), path.to_path_buf());
        self.providers.insert(name, provider);
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    #[serde(default)]
    providers: Vec<ProviderConfig>,
}

/// One declared entry, turned into a live provider and what the UI is told.
fn build(
    path: &Path,
    config: ProviderConfig,
    http: &Client,
    sessions: &Arc<SessionStore>,
) -> Result<(String, Auth, AuthDescriptor), LoadIssue> {
    match config {
        ProviderConfig::Anonymous {
            name,
            allowed_hosts,
        } => {
            let descriptor = AuthDescriptor::new(&name, AuthKind::Anonymous, &allowed_hosts);
            let provider = Auth::Anonymous(Anonymous::new(name.clone(), allowed_hosts));
            Ok((name, provider, descriptor))
        }
        ProviderConfig::Token {
            name,
            header,
            scheme,
            value,
            allowed_hosts,
        } => {
            let header = header_name(path, &name, &header)?;
            let mut descriptor = AuthDescriptor::new(&name, AuthKind::Token, &allowed_hosts);
            if value.env.is_none() && value.file.is_none() {
                descriptor = descriptor.prompting();
            }
            let provider = Auth::Token(TokenAuth::new(
                name.clone(),
                header,
                scheme,
                value,
                allowed_hosts,
            ));
            Ok((name, provider, descriptor))
        }
        ProviderConfig::Oidc {
            name,
            issuer,
            token_endpoint,
            client_id,
            client_secret,
            client_assertion,
            scope,
            audience,
            header,
            scheme,
            allowed_hosts,
        } => {
            let header = header_name(path, &name, &header)?;
            let credential = client_credential(path, &name, client_secret, client_assertion)?;
            // A machine identity: never something the UI could sensibly ask a
            // human to paste, and never something to sign in to.
            let descriptor = AuthDescriptor::new(&name, AuthKind::Oidc, &allowed_hosts);
            let provider = Auth::Oidc(Box::new(OidcAuth::new(
                OidcConfig {
                    name: name.clone(),
                    issuer,
                    token_endpoint,
                    client_id,
                    credential,
                    scope,
                    audience,
                    header,
                    scheme,
                    allowed_hosts,
                },
                http.clone(),
            )));
            Ok((name, provider, descriptor))
        }
        ProviderConfig::OidcBrowser {
            name,
            issuer,
            authorization_endpoint,
            token_endpoint,
            client_id,
            client_secret,
            scope,
            audience,
            header,
            scheme,
            allowed_hosts,
        } => {
            let header = header_name(path, &name, &header)?;
            // The credential is fetched, not typed: prompting for one would be
            // asking the human to do the flow's job.
            let descriptor = AuthDescriptor::new(&name, AuthKind::OidcBrowser, &allowed_hosts);
            let provider = Auth::OidcBrowser(Box::new(OidcBrowserAuth::new(
                OidcBrowserConfig {
                    name: name.clone(),
                    issuer,
                    authorization_endpoint,
                    token_endpoint,
                    client_id,
                    client_secret,
                    scope,
                    audience,
                    header,
                    scheme,
                    allowed_hosts,
                },
                http.clone(),
                Arc::clone(sessions),
            )));
            Ok((name, provider, descriptor))
        }
    }
}

/// Picks the one client credential an OIDC provider is allowed to declare.
fn client_credential(
    path: &Path,
    provider: &str,
    secret: Option<TokenValue>,
    assertion: Option<AssertionSource>,
) -> Result<ClientCredential, LoadIssue> {
    match (secret, assertion) {
        (Some(secret), None) => Ok(ClientCredential::Secret(secret)),
        (None, Some(assertion)) => Ok(ClientCredential::Assertion {
            file: assertion.file,
        }),
        (Some(_), Some(_)) => Err(LoadIssue::new(
            path,
            format!(
                "auth `{provider}`: set either `client_secret` or `client_assertion`, not both"
            ),
        )),
        (None, None) => Err(LoadIssue::new(
            path,
            format!(
                "auth `{provider}`: `client_credentials` needs a `client_secret` or a `client_assertion`"
            ),
        )),
    }
}

/// Parses a header name, or explains which provider declared a bad one.
fn header_name(path: &Path, provider: &str, header: &str) -> Result<HeaderName, LoadIssue> {
    HeaderName::try_from(header.to_ascii_lowercase()).map_err(|_| {
        LoadIssue::new(
            path,
            format!("auth `{provider}`: `{header}` is not a valid HTTP header name"),
        )
    })
}

/// A `client_assertion` source. Only a file makes sense: this is a projected
/// service account token, and it is re-read on every exchange.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AssertionSource {
    file: PathBuf,
}

/// Deserialisation-only: parsed once at load, destructured immediately into
/// providers. Nothing is ever stored or moved as a `ProviderConfig`.
#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ProviderConfig {
    Anonymous {
        name: String,
        #[serde(default)]
        allowed_hosts: Vec<String>,
    },
    Token {
        name: String,
        #[serde(default = "default_header")]
        header: String,
        #[serde(default = "default_scheme")]
        scheme: Option<String>,
        #[serde(default)]
        value: TokenValue,
        #[serde(default)]
        allowed_hosts: Vec<String>,
    },
    Oidc {
        name: String,
        issuer: Url,
        #[serde(default)]
        token_endpoint: Option<Url>,
        client_id: String,
        #[serde(default)]
        client_secret: Option<TokenValue>,
        #[serde(default)]
        client_assertion: Option<AssertionSource>,
        #[serde(default)]
        scope: Vec<String>,
        #[serde(default)]
        audience: Option<String>,
        #[serde(default = "default_header")]
        header: String,
        #[serde(default = "default_scheme")]
        scheme: Option<String>,
        #[serde(default)]
        allowed_hosts: Vec<String>,
    },
    /// Authorization code + PKCE. No client credential is required: `mire` runs
    /// from a directory of YAML files and has no secret to keep, which is exactly
    /// the case PKCE exists for.
    OidcBrowser {
        name: String,
        issuer: Url,
        #[serde(default)]
        authorization_endpoint: Option<Url>,
        #[serde(default)]
        token_endpoint: Option<Url>,
        client_id: String,
        #[serde(default)]
        client_secret: Option<TokenValue>,
        #[serde(default)]
        scope: Vec<String>,
        #[serde(default)]
        audience: Option<String>,
        #[serde(default = "default_header")]
        header: String,
        #[serde(default = "default_scheme")]
        scheme: Option<String>,
        #[serde(default)]
        allowed_hosts: Vec<String>,
    },
}

fn default_header() -> String {
    DEFAULT_HEADER.to_owned()
}

/// `scheme` is an `Option` so that `scheme: null` means "send the credential
/// bare"; serde needs the default in the field's own type, hence the wrap.
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde default for an Option field must return that Option"
)]
fn default_scheme() -> Option<String> {
    Some(DEFAULT_SCHEME.to_owned())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Every test gets its own session store: nothing here is signed in.
    fn load(dir: &Path) -> AuthRegistry {
        AuthRegistry::load(dir, &Client::new(), &Arc::new(SessionStore::default()))
    }

    fn write_registry(tag: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-registry-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(AUTH_REGISTRY_FILE), body).unwrap();
        dir
    }

    #[test]
    fn anonymous_is_available_without_any_registry() {
        let dir = std::env::temp_dir().join(format!("mire-registry-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let registry = load(&dir);
        assert!(registry.get(ANONYMOUS).is_some());
        assert_eq!(registry.descriptors().len(), 1);
        assert!(registry.issues().is_empty());
    }

    #[test]
    fn a_later_directory_takes_a_provider_the_earlier_one_declared() {
        let base = write_registry(
            "layer-base",
            "providers:\n  - name: gateway\n    kind: token\n    value:\n      env: MODEL_TOKEN\n",
        );
        let mine = write_registry(
            "layer-mine",
            "providers:\n  - name: gateway\n    kind: anonymous\n",
        );

        let registry = AuthRegistry::load_dirs(
            &[&base, &mine],
            &Client::new(),
            &Arc::new(SessionStore::default()),
        );

        assert!(matches!(registry.get("gateway"), Some(Auth::Anonymous(_))));
        // One descriptor, not two: the UI's selector must not offer the ghost.
        assert_eq!(
            registry
                .descriptors()
                .iter()
                .filter(|descriptor| descriptor.name == "gateway")
                .count(),
            1
        );
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
    }

    /// Scoping `anonymous` in a base directory must survive a second directory
    /// that says nothing about it — the built-in is not a redeclaration.
    #[test]
    fn a_later_directory_that_is_silent_leaves_a_scoped_anonymous_alone() {
        let base = write_registry(
            "layer-anon-base",
            "providers:\n  - name: anonymous\n    kind: anonymous\n    allowed_hosts:\n      - models.internal\n",
        );
        let mine = write_registry(
            "layer-anon-mine",
            "providers:\n  - name: gateway\n    kind: anonymous\n",
        );

        let registry = AuthRegistry::load_dirs(
            &[&base, &mine],
            &Client::new(),
            &Arc::new(SessionStore::default()),
        );

        let anonymous = registry
            .descriptors()
            .iter()
            .find(|descriptor| descriptor.name == ANONYMOUS)
            .unwrap();
        assert_eq!(anonymous.allowed_hosts, ["models.internal"]);
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
    }

    /// Overriding is only ever *across* directories. Twice in one file is still
    /// the typo it always was.
    #[test]
    fn a_duplicate_inside_the_later_directory_is_still_reported() {
        let base = write_registry(
            "layer-dup-base",
            "providers:\n  - name: gateway\n    kind: anonymous\n",
        );
        let mine = write_registry(
            "layer-dup-mine",
            "providers:\n  - name: gateway\n    kind: anonymous\n  - name: gateway\n    kind: anonymous\n",
        );

        let registry = AuthRegistry::load_dirs(
            &[&base, &mine],
            &Client::new(),
            &Arc::new(SessionStore::default()),
        );

        assert_eq!(registry.issues().len(), 1);
        assert!(
            registry.issues()[0]
                .message
                .contains("duplicate auth provider")
        );
    }

    #[test]
    fn parses_a_token_provider_with_defaults() {
        let dir = write_registry(
            "token",
            "providers:\n  - name: gateway\n    kind: token\n    value:\n      env: MODEL_TOKEN\n",
        );
        let registry = load(&dir);

        let descriptor = registry
            .descriptors()
            .iter()
            .find(|d| d.name == "gateway")
            .unwrap();
        assert_eq!(descriptor.kind, AuthKind::Token);
        assert!(!descriptor.needs_value);
        assert!(matches!(registry.get("gateway"), Some(Auth::Token(_))));
    }

    #[test]
    fn a_provider_without_a_declared_source_asks_the_ui_for_one() {
        let dir = write_registry("prompt", "providers:\n  - name: pasted\n    kind: token\n");
        let registry = load(&dir);

        let descriptor = registry
            .descriptors()
            .iter()
            .find(|d| d.name == "pasted")
            .unwrap();
        assert!(descriptor.needs_value);
    }

    #[test]
    fn one_bad_entry_is_reported_and_the_others_still_load() {
        let dir = write_registry(
            "partial",
            "providers:\n  \
             - name: bad\n    kind: token\n    header: \"not a header\"\n  \
             - name: good\n    kind: token\n    value:\n      env: MODEL_TOKEN\n",
        );
        let registry = load(&dir);

        assert!(registry.get("good").is_some());
        assert!(registry.get("bad").is_none());
        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("not a header"));
    }

    #[test]
    fn duplicate_names_are_reported() {
        let dir = write_registry(
            "dup",
            "providers:\n  - name: a\n    kind: anonymous\n  - name: a\n    kind: anonymous\n",
        );
        let registry = load(&dir);
        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("duplicate"));
    }

    #[test]
    fn a_registry_that_does_not_parse_still_leaves_anonymous_usable() {
        let dir = write_registry("syntax", "providers: [unclosed\n");
        let registry = load(&dir);

        assert!(registry.get(ANONYMOUS).is_some());
        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].line.is_some());
    }

    #[test]
    fn parses_an_oidc_provider_with_a_client_secret() {
        let dir = write_registry(
            "oidc-secret",
            r"
providers:
  - name: workload
    kind: oidc
    issuer: https://idp.internal/realms/models
    client_id: mire
    client_secret:
      env: OIDC_CLIENT_SECRET
    scope: [openid, models:read]
    audience: https://models.internal
",
        );
        let registry = load(&dir);

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        assert!(matches!(registry.get("workload"), Some(Auth::Oidc(_))));
        let descriptor = registry
            .descriptors()
            .iter()
            .find(|d| d.name == "workload")
            .unwrap();
        assert_eq!(descriptor.kind, AuthKind::Oidc);
        // A machine identity is never something to prompt a human for.
        assert!(!descriptor.needs_value);
    }

    #[test]
    fn parses_an_oidc_provider_with_a_projected_service_account_token() {
        let dir = write_registry(
            "oidc-assertion",
            r"
providers:
  - name: workload
    kind: oidc
    issuer: https://idp.internal/realms/models
    client_id: mire
    client_assertion:
      file: /var/run/secrets/kubernetes.io/serviceaccount/token
",
        );
        let registry = load(&dir);
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        assert!(registry.get("workload").is_some());
    }

    #[test]
    fn an_oidc_provider_needs_exactly_one_client_credential() {
        let neither = write_registry(
            "oidc-neither",
            r"
providers:
  - name: workload
    kind: oidc
    issuer: https://idp.internal/realms/models
    client_id: mire
",
        );
        let registry = load(&neither);
        assert!(registry.get("workload").is_none());
        assert!(
            registry.issues()[0]
                .message
                .contains("needs a `client_secret`")
        );

        let both = write_registry(
            "oidc-both",
            r"
providers:
  - name: workload
    kind: oidc
    issuer: https://idp.internal/realms/models
    client_id: mire
    client_secret:
      env: OIDC_CLIENT_SECRET
    client_assertion:
      file: /var/run/secrets/token
",
        );
        let registry = load(&both);
        assert!(registry.get("workload").is_none());
        assert!(registry.issues()[0].message.contains("not both"));
    }

    #[test]
    fn the_host_allow_list_reaches_the_descriptor() {
        let dir = write_registry(
            "hosts",
            "providers:\n  \
             - name: pinned\n    kind: token\n    value:\n      env: MODEL_TOKEN\n    allowed_hosts:\n      - models.internal\n  \
             - name: anywhere\n    kind: token\n    value:\n      env: MODEL_TOKEN\n",
        );
        let registry = load(&dir);

        let pinned = registry
            .descriptors()
            .iter()
            .find(|d| d.name == "pinned")
            .unwrap();
        assert_eq!(pinned.allowed_hosts, vec!["models.internal".to_owned()]);

        // Empty is the default and means anywhere, so the UI has nothing to
        // filter on — not "nowhere".
        let anywhere = registry
            .descriptors()
            .iter()
            .find(|d| d.name == "anywhere")
            .unwrap();
        assert!(anywhere.allowed_hosts.is_empty());
    }

    #[test]
    fn anonymous_can_be_redeclared_to_scope_it() {
        let dir = write_registry(
            "scoped",
            "providers:\n  - name: anonymous\n    kind: anonymous\n    allowed_hosts:\n      - models.internal\n",
        );
        let registry = load(&dir);
        assert!(registry.issues().is_empty());
        assert_eq!(registry.descriptors().len(), 1);
    }
}
