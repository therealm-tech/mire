//! Reads and validates every profile in a directory.
//!
//! A broken file never stops the others from loading — see [`crate::issue`] for
//! why.
//!
//! Several directories can be layered, and then a name declared twice is not a
//! mistake but the point: the last directory to declare it wins, loudly. Within
//! one directory it is still a mistake, and still reported as one.

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
const RESERVED: [&str; 3] = [
    AUTH_REGISTRY_FILE,
    crate::mcp::registry::MCP_REGISTRY_FILE,
    crate::prompt::PROMPT_REGISTRY_FILE,
];

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

/// Reads every `*.yaml` / `*.yml` file in each of `dirs` as a profile.
///
/// The directories are layered in the order given: a name declared in two of
/// them is the later one's, and the shadowed profile is logged rather than kept.
/// That is the whole point of listing more than one — a directory you cannot
/// edit, and one of your own on top of it.
///
/// # Errors
///
/// Fails when one of the directories cannot be read. The error names it: with a
/// list, "no such file or directory" on its own does not say which.
pub fn load_dirs(dirs: &[impl AsRef<Path>]) -> std::io::Result<ProfileSet> {
    let mut set = ProfileSet::default();
    for dir in dirs {
        let dir = dir.as_ref();
        read_dir_into(&mut set, dir).map_err(|error| {
            std::io::Error::new(error.kind(), format!("`{}`: {error}", dir.display()))
        })?;
    }
    Ok(set)
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
    read_dir_into(&mut set, dir)?;
    Ok(set)
}

/// Folds one directory into `set`, which may already hold earlier directories.
///
/// Two names collide for two different reasons, and they are not the same event.
/// Twice in *this* directory is a mistake nobody meant to make: it is reported,
/// and the first file keeps the name. Once here and once in a directory read
/// earlier is a deliberate override: this one takes it, and the one it displaced
/// is named in the log so that a profile behaving unexpectedly has somewhere to
/// be explained.
fn read_dir_into(set: &mut ProfileSet, dir: &Path) -> std::io::Result<()> {
    let mut here: BTreeMap<String, PathBuf> = BTreeMap::new();

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
                if let Some(previous) = here.insert(profile.name.clone(), path.clone()) {
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
                if let Some(shadowed) = set.profiles.get(&profile.name) {
                    warn!(
                        name = %profile.name,
                        path = %path.display(),
                        shadowed = %shadowed.source.display(),
                        "profile overridden by a later directory"
                    );
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

    Ok(())
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
        std::fs::write(dir.join("prompts.yaml"), "prompts: []\n").unwrap();

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
    fn a_later_directory_takes_a_name_the_earlier_one_declared() {
        let base = temp_dir("layer-base");
        let mine = temp_dir("layer-mine");
        write(&base, "good.yaml", GOOD);
        write(
            &mine,
            "override.yaml",
            &GOOD.replace(
                "https://models.internal/good",
                "https://staging.internal/good",
            ),
        );

        let set = load_dirs(&[&base, &mine]).unwrap();

        assert_eq!(set.len(), 1);
        assert_eq!(
            set.get("good").unwrap().url.as_str(),
            "https://staging.internal/good"
        );
        assert!(set.get("good").unwrap().source.starts_with(&mine));
        // The override is a warning, not a load failure: nothing here is broken.
        assert!(set.issues().is_empty(), "{:?}", set.issues());
    }

    #[test]
    fn layered_directories_add_up_rather_than_replace_each_other() {
        let base = temp_dir("layer-add-base");
        let mine = temp_dir("layer-add-mine");
        write(&base, "good.yaml", GOOD);
        write(
            &mine,
            "other.yaml",
            &GOOD.replace("name: good", "name: other"),
        );

        let set = load_dirs(&[&base, &mine]).unwrap();

        assert_eq!(set.len(), 2);
        assert!(set.get("good").is_some());
        assert!(set.get("other").is_some());
    }

    /// Overriding is only ever *across* directories. Two files in the same one
    /// still cannot both hold the name — nobody writes that on purpose.
    #[test]
    fn a_duplicate_inside_the_later_directory_is_still_reported() {
        let base = temp_dir("layer-dup-base");
        let mine = temp_dir("layer-dup-mine");
        write(&base, "good.yaml", GOOD);
        write(&mine, "a.yaml", GOOD);
        write(&mine, "b.yaml", GOOD);

        let set = load_dirs(&[&base, &mine]).unwrap();

        assert_eq!(set.len(), 1);
        assert_eq!(set.issues().len(), 1);
        assert!(set.issues()[0].message.contains("duplicate profile name"));
        // Named against the file that actually holds the name, not the base's.
        assert!(
            set.issues()[0].message.contains("a.yaml"),
            "{:?}",
            set.issues()
        );
    }

    #[test]
    fn a_directory_that_cannot_be_read_is_named_in_the_error() {
        let base = temp_dir("layer-missing");
        let missing = base.join("nowhere");

        let error = load_dirs(&[&base, &missing]).unwrap_err();

        assert!(error.to_string().contains("nowhere"), "{error}");
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
