//! Reads and validates every model in a configuration directory's `models/`.
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

use super::Model;
use crate::config::layout;
use crate::issue::LoadIssue;

/// Everything loaded from the `models/` subdirectories: what parsed, and what did
/// not.
#[derive(Debug, Clone, Default)]
pub struct ModelSet {
    models: BTreeMap<String, Arc<Model>>,
    issues: Vec<LoadIssue>,
}

impl ModelSet {
    /// Looks a model up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<Model>> {
        self.models.get(name)
    }

    /// Every model that parsed and validated, ordered by name.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Model>> {
        self.models.values()
    }

    /// Number of usable models.
    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Returns `true` when nothing usable was loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Files that failed to load, and why.
    #[must_use]
    pub fn issues(&self) -> &[LoadIssue] {
        &self.issues
    }
}

/// Reads every model file in each of `dirs`, layered in the order given.
///
/// A name declared in two of them is the later one's, and the shadowed model is
/// logged rather than kept. That is the whole point of listing more than one — a
/// directory you cannot edit, and one of your own on top of it.
///
/// Never fails: a `models/` that is not there declares nothing, and one that
/// cannot be read is an issue you can see in the UI. Whether the configuration
/// directory itself exists is settled once, by [`crate::config`], before any of
/// this runs.
#[must_use]
pub fn load_dirs(dirs: &[impl AsRef<Path>]) -> ModelSet {
    let mut set = ModelSet::default();
    for dir in dirs {
        read_dir_into(&mut set, dir.as_ref());
    }
    set
}

/// Reads every model file in one configuration directory.
#[must_use]
pub fn load_dir(dir: &Path) -> ModelSet {
    load_dirs(&[dir])
}

/// Folds one directory into `set`, which may already hold earlier directories.
///
/// Two names collide for two different reasons, and they are not the same event.
/// Twice in *this* directory is a mistake nobody meant to make: it is reported,
/// and the first file keeps the name. Once here and once in a directory read
/// earlier is a deliberate override: this one takes it, and the one it displaced
/// is named in the log so that a model behaving unexpectedly has somewhere to be
/// explained.
fn read_dir_into(set: &mut ModelSet, dir: &Path) {
    let paths = match layout::entries(dir, layout::MODELS) {
        Ok(paths) => paths,
        Err(issue) => {
            warn!(%issue, "models directory rejected");
            set.issues.push(issue);
            return;
        }
    };

    let mut here: BTreeMap<String, PathBuf> = BTreeMap::new();

    for path in paths {
        match load_file(&path) {
            Ok(None) => debug!(path = %path.display(), "file declares no model"),
            Ok(Some(model)) => {
                if let Some(previous) = here.insert(model.name.clone(), path.clone()) {
                    set.issues.push(LoadIssue::new(
                        &path,
                        format!(
                            "duplicate model name `{}`, already declared in {}",
                            model.name,
                            previous.display()
                        ),
                    ));
                    continue;
                }
                if let Some(shadowed) = set.models.get(&model.name) {
                    warn!(
                        name = %model.name,
                        path = %path.display(),
                        shadowed = %shadowed.source.display(),
                        "model overridden by a later directory"
                    );
                }
                debug!(name = %model.name, kind = ?model.kind, path = %model.source.display(), "model loaded");
                set.models.insert(model.name.clone(), Arc::new(model));
            }
            Err(issue) => {
                warn!(%issue, "model rejected");
                set.issues.push(issue);
            }
        }
    }
}

/// Reads and validates a single model file.
///
/// `Ok(None)` for a file that declares nothing — an example shipped commented
/// out, for instance.
///
/// # Errors
///
/// Returns a [`LoadIssue`] for an unreadable file, a YAML syntax error, an
/// unknown or malformed field, or a failed validation rule.
pub fn load_file(path: &Path) -> Result<Option<Model>, LoadIssue> {
    let Some(mut model) = layout::read::<Model>(path)? else {
        return Ok(None);
    };

    model
        .validate()
        .map_err(|error| LoadIssue::new(path, error.to_string()))?;

    path.clone_into(&mut model.source);
    Ok(Some(model))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(layout::MODELS).join(name), body).unwrap();
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-loader-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(layout::MODELS)).unwrap();
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

        let set = load_dir(&dir);
        assert_eq!(set.len(), 1);
        assert!(set.get("good").is_some());
        assert_eq!(set.issues().len(), 1);
        assert!(set.issues()[0].file.ends_with("broken.yaml"));
    }

    /// The registries are next door rather than in here, so nothing has to be
    /// skipped by name any more: `auth/` is simply not `models/`.
    #[test]
    fn the_other_subdirectories_are_not_read_as_models() {
        let dir = temp_dir("neighbours");
        write(&dir, "good.yaml", GOOD);
        std::fs::create_dir_all(dir.join(layout::AUTH)).unwrap();
        std::fs::write(
            dir.join(layout::AUTH).join("gateway.yaml"),
            "name: gateway\nkind: token\n",
        )
        .unwrap();

        let set = load_dir(&dir);

        assert_eq!(set.len(), 1);
        assert!(set.issues().is_empty(), "{:?}", set.issues());
    }

    #[test]
    fn a_directory_with_no_models_at_all_is_not_a_problem() {
        let dir = std::env::temp_dir().join(format!("mire-loader-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let set = load_dir(&dir);

        assert!(set.is_empty());
        assert!(set.issues().is_empty(), "{:?}", set.issues());
    }

    #[test]
    fn duplicate_names_are_reported_not_silently_overwritten() {
        let dir = temp_dir("dup");
        write(&dir, "a.yaml", GOOD);
        write(&dir, "b.yaml", GOOD);

        let set = load_dir(&dir);
        assert_eq!(set.len(), 1);
        assert!(set.issues()[0].message.contains("duplicate model name"));
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

        let set = load_dirs(&[&base, &mine]);

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

        let set = load_dirs(&[&base, &mine]);

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

        let set = load_dirs(&[&base, &mine]);

        assert_eq!(set.len(), 1);
        assert_eq!(set.issues().len(), 1);
        assert!(set.issues()[0].message.contains("duplicate model name"));
        // Named against the file that actually holds the name, not the base's.
        assert!(
            set.issues()[0].message.contains("a.yaml"),
            "{:?}",
            set.issues()
        );
    }

    #[test]
    fn a_syntax_error_carries_a_position() {
        let dir = temp_dir("syntax");
        write(&dir, "bad.yaml", "name: [unclosed\n");

        let set = load_dir(&dir);
        assert!(set.is_empty());
        assert!(set.issues()[0].line.is_some());
    }
}
