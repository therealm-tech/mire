//! Reads and validates every profile in a directory.
//!
//! A broken file never stops the others from loading — see [`crate::issue`] for
//! why.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tracing::{debug, warn};
use validator::Validate;

use super::Profile;
use crate::issue::LoadIssue;

/// File name reserved for the auth registry, never read as a profile.
pub const AUTH_REGISTRY_FILE: &str = "auth.yaml";

/// Every file name in the directory that is a registry rather than a profile.
///
/// A registry that got read as a profile would come back as a load issue about a
/// field nobody wrote — confusing, and it would happen the first time anyone
/// added `mcp.yaml`.
const RESERVED: [&str; 2] = [AUTH_REGISTRY_FILE, crate::mcp::registry::MCP_REGISTRY_FILE];

/// Everything loaded from the profiles directory: what parsed, and what did not.
#[derive(Debug, Clone, Default)]
pub struct ProfileSet {
    profiles: BTreeMap<String, Arc<Profile>>,
    issues: Vec<LoadIssue>,
}

impl ProfileSet {
    /// Looks a profile up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<Profile>> {
        self.profiles.get(name)
    }

    /// Every profile that parsed and validated, ordered by name.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Profile>> {
        self.profiles.values()
    }

    /// Number of usable profiles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// Returns `true` when nothing usable was loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    /// Files that failed to load, and why.
    #[must_use]
    pub fn issues(&self) -> &[LoadIssue] {
        &self.issues
    }
}

/// Reads every `*.yaml` / `*.yml` file in `dir` as a profile.
///
/// Not recursive, and the registry files are skipped. Returns the set even when
/// every file is broken; the caller decides what to do about the issues.
///
/// # Errors
///
/// Fails only when the directory itself cannot be read.
pub fn load_dir(dir: &Path) -> std::io::Result<ProfileSet> {
    let mut set = ProfileSet::default();
    let mut names: BTreeMap<String, PathBuf> = BTreeMap::new();

    let mut paths: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && has_yaml_extension(path))
        .filter(|path| {
            path.file_name()
                .is_none_or(|name| !RESERVED.iter().any(|reserved| name == *reserved))
        })
        .collect();
    paths.sort();

    for path in paths {
        match load_file(&path) {
            Ok(profile) => {
                if let Some(previous) = names.insert(profile.name.clone(), path.clone()) {
                    set.issues.push(LoadIssue::new(
                        &path,
                        format!(
                            "duplicate profile name `{}`, already declared in {}",
                            profile.name,
                            previous.display()
                        ),
                    ));
                    continue;
                }
                debug!(name = %profile.name, kind = ?profile.kind, path = %profile.source.display(), "profile loaded");
                set.profiles.insert(profile.name.clone(), Arc::new(profile));
            }
            Err(issue) => {
                warn!(%issue, "profile rejected");
                set.issues.push(issue);
            }
        }
    }

    Ok(set)
}

/// Reads and validates a single profile file.
///
/// # Errors
///
/// Returns a [`LoadIssue`] for an unreadable file, a YAML syntax error, an
/// unknown or malformed field, or a failed validation rule.
pub fn load_file(path: &Path) -> Result<Profile, LoadIssue> {
    let text =
        std::fs::read_to_string(path).map_err(|error| LoadIssue::new(path, error.to_string()))?;

    let mut profile: Profile =
        serde_yaml_ng::from_str(&text).map_err(|error| LoadIssue::from_yaml(path, &error))?;

    profile
        .validate()
        .map_err(|error| LoadIssue::new(path, error.to_string()))?;

    path.clone_into(&mut profile.source);
    Ok(profile)
}

fn has_yaml_extension(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("yaml" | "yml")
    )
}

#[cfg(test)]
mod tests {
    /// The registries live in the same directory and are not profiles. Reading
    /// one as a profile produces a baffling complaint about `servers`.
    #[test]
    fn the_registry_files_are_not_read_as_profiles() {
        let dir = std::env::temp_dir().join(format!("mire-reserved-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("auth.yaml"), "providers: []\n").unwrap();
        std::fs::write(dir.join("mcp.yaml"), "servers: []\n").unwrap();

        let set = load_dir(&dir).unwrap();
        assert!(set.is_empty());
        assert!(set.issues().is_empty(), "{:?}", set.issues());
    }

    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-loader-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const GOOD: &str = r#"
name: good
kind: chat
url: https://models.internal/good
request:
  template: '{"messages": {{ messages | tojson }}}'
"#;

    #[test]
    fn a_broken_file_does_not_stop_the_others() {
        let dir = temp_dir("broken");
        write(&dir, "good.yaml", GOOD);
        write(&dir, "broken.yaml", "name: broken\nkind: nope\n");

        let set = load_dir(&dir).unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.get("good").is_some());
        assert_eq!(set.issues().len(), 1);
        assert!(set.issues()[0].file.ends_with("broken.yaml"));
    }

    #[test]
    fn the_auth_registry_is_not_a_profile() {
        let dir = temp_dir("registry");
        write(&dir, "good.yaml", GOOD);
        write(&dir, AUTH_REGISTRY_FILE, "providers: []\n");

        let set = load_dir(&dir).unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.issues().is_empty());
    }

    #[test]
    fn duplicate_names_are_reported_not_silently_overwritten() {
        let dir = temp_dir("dup");
        write(&dir, "a.yaml", GOOD);
        write(&dir, "b.yaml", GOOD);

        let set = load_dir(&dir).unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.issues()[0].message.contains("duplicate profile name"));
    }

    #[test]
    fn a_syntax_error_carries_a_position() {
        let dir = temp_dir("syntax");
        write(&dir, "bad.yaml", "name: [unclosed\n");

        let set = load_dir(&dir).unwrap();
        assert!(set.is_empty());
        assert!(set.issues()[0].line.is_some());
    }
}
