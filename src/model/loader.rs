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
use crate::config::stage::Staged;
use crate::decode::registry::DecodeRegistry;
use crate::issue::LoadIssue;

/// Everything loaded from the `models/` subdirectories: what parsed, and what did
/// not.
///
/// Keyed by [`Model::id`] rather than by name, so the two stages of one file are
/// two entries — `qwen3@dev` and `qwen3@prod` — sharing a name and nothing else.
#[derive(Debug, Clone, Default)]
pub struct ModelSet {
    models: BTreeMap<String, Arc<Model>>,
    /// Name to the id a bare reference means. One entry per file, whether or not
    /// it declares stages.
    defaults: BTreeMap<String, String>,
    issues: Vec<LoadIssue>,
}

impl ModelSet {
    /// Looks a model up by `name@stage`, or by name for its default stage.
    #[must_use]
    pub fn get(&self, reference: &str) -> Option<&Arc<Model>> {
        self.models.get(reference).or_else(|| {
            self.defaults
                .get(reference)
                .and_then(|id| self.models.get(id))
        })
    }

    /// Every model that parsed and validated, ordered by name.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<Model>> {
        self.models.values()
    }

    /// Whether a bare reference to this model's name resolves to this entry.
    ///
    /// Always true for a file that declares no stages, which is its own default
    /// by having nothing to choose between.
    #[must_use]
    pub fn is_default(&self, model: &Model) -> bool {
        self.defaults
            .get(&model.name)
            .is_some_and(|id| *id == model.id())
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
pub fn load_dirs(dirs: &[impl AsRef<Path>], decodes: &DecodeRegistry) -> ModelSet {
    let mut set = ModelSet::default();
    for dir in dirs {
        read_dir_into(&mut set, dir.as_ref(), decodes);
    }
    set
}

/// Reads every model file in one configuration directory.
#[must_use]
pub fn load_dir(dir: &Path, decodes: &DecodeRegistry) -> ModelSet {
    load_dirs(&[dir], decodes)
}

/// Folds one directory into `set`, which may already hold earlier directories.
///
/// Two names collide for two different reasons, and they are not the same event.
/// Twice in *this* directory is a mistake nobody meant to make: it is reported,
/// and the first file keeps the name. Once here and once in a directory read
/// earlier is a deliberate override: this one takes it, and the one it displaced
/// is named in the log so that a model behaving unexpectedly has somewhere to be
/// explained.
fn read_dir_into(set: &mut ModelSet, dir: &Path, decodes: &DecodeRegistry) {
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
        let staged = match load_file(&path, decodes) {
            Ok(staged) => staged,
            Err(issue) => {
                warn!(%issue, "model rejected");
                set.issues.push(issue);
                continue;
            }
        };
        let Some(name) = staged.first().map(|entry| entry.value.name.clone()) else {
            debug!(path = %path.display(), "file declares no model");
            continue;
        };

        if let Some(previous) = here.insert(name.clone(), path.clone()) {
            set.issues.push(LoadIssue::new(
                &path,
                format!(
                    "duplicate model name `{name}`, already declared in {}",
                    previous.display()
                ),
            ));
            continue;
        }

        // The whole name is displaced, stages and all: a later directory
        // declaring `qwen3` with one stage does not leave the base directory's
        // other two standing beside it. Overriding an entry means replacing what
        // that entry is, not merging two files that never saw each other.
        if let Some(shadowed) = set.models.values().find(|model| model.name == name) {
            warn!(
                %name,
                path = %path.display(),
                shadowed = %shadowed.source.display(),
                "model overridden by a later directory"
            );
            set.models.retain(|_, model| model.name != name);
        }

        for entry in staged {
            let model = entry.value;
            let id = model.id();
            if entry.default {
                set.defaults.insert(name.clone(), id.clone());
            }
            debug!(%id, kind = ?model.kind, path = %model.source.display(), "model loaded");
            set.models.insert(id, Arc::new(model));
        }
    }
}

/// Reads and validates a single model file, once per stage it declares.
///
/// An empty list for a file that declares nothing — an example shipped commented
/// out, for instance. One entry for a file with no `stages:`, and one per stage
/// otherwise, each validated in full: a `prod` that does not hold together is a
/// load issue at startup rather than a surprise the day somebody picks it.
///
/// # Errors
///
/// Returns a [`LoadIssue`] for an unreadable file, a YAML syntax error, a
/// malformed `stages:` block, an unknown or malformed field, or a failed
/// validation rule. One bad stage fails the file: see [`crate::config::stage`].
pub fn load_file(path: &Path, decodes: &DecodeRegistry) -> Result<Vec<Staged<Model>>, LoadIssue> {
    layout::read::<Model>(path)?
        .into_iter()
        .map(|staged| {
            let mut model = staged.value;
            model.stage.clone_from(&staged.stage);
            path.clone_into(&mut model.source);
            let stage_prefix = |message: String| match &staged.stage {
                Some(stage) => format!("stage `{stage}`: {message}"),
                None => message,
            };
            model
                .validate()
                .map_err(|error| LoadIssue::new(path, stage_prefix(error.to_string())))?;
            // Resolved here, once, so that everything downstream — the executor,
            // the trace, the API — sees the flat cascade a model with no `from:`
            // would have written by hand.
            model.decode = decodes
                .resolve(&model.decode, model.kind)
                .map_err(|message| LoadIssue::new(path, stage_prefix(message)))?;
            Ok(Staged {
                stage: staged.stage,
                default: staged.default,
                value: model,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Nothing in these tests names a decode, so the built-ins alone are the
    /// registry every model here resolves against.
    fn decodes() -> DecodeRegistry {
        DecodeRegistry::builtin()
    }

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

        let set = load_dir(&dir, &decodes());
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

        let set = load_dir(&dir, &decodes());

        assert_eq!(set.len(), 1);
        assert!(set.issues().is_empty(), "{:?}", set.issues());
    }

    #[test]
    fn a_directory_with_no_models_at_all_is_not_a_problem() {
        let dir = std::env::temp_dir().join(format!("mire-loader-bare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let set = load_dir(&dir, &decodes());

        assert!(set.is_empty());
        assert!(set.issues().is_empty(), "{:?}", set.issues());
    }

    #[test]
    fn duplicate_names_are_reported_not_silently_overwritten() {
        let dir = temp_dir("dup");
        write(&dir, "a.yaml", GOOD);
        write(&dir, "b.yaml", GOOD);

        let set = load_dir(&dir, &decodes());
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

        let set = load_dirs(&[&base, &mine], &decodes());

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

        let set = load_dirs(&[&base, &mine], &decodes());

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

        let set = load_dirs(&[&base, &mine], &decodes());

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

    const STAGED: &str = r#"
name: staged
kind: chat
url: ${ stage.base }/v1/chat
timeout_ms: ${ stage.timeout | default(30000) }
stages:
  dev:
    base: http://127.0.0.1:11435
  prod:
    base: https://models.internal
    timeout: 60000
default_stage: dev
request:
  template: '{"messages": {{ messages | tojson }}}'
"#;

    #[test]
    fn a_staged_file_declares_one_model_per_stage() {
        let dir = temp_dir("staged");
        write(&dir, "staged.yaml", STAGED);

        let set = load_dir(&dir, &decodes());

        assert_eq!(set.len(), 2);
        assert_eq!(
            set.get("staged@prod").unwrap().url.as_str(),
            "https://models.internal/v1/chat"
        );
        assert_eq!(set.get("staged@prod").unwrap().timeout_ms, 60_000);
        assert_eq!(set.get("staged@dev").unwrap().timeout_ms, 30_000);
    }

    /// What every reference written before stages existed relies on.
    #[test]
    fn a_bare_name_means_the_default_stage() {
        let dir = temp_dir("staged-default");
        write(&dir, "staged.yaml", STAGED);

        let set = load_dir(&dir, &decodes());

        assert_eq!(set.get("staged").unwrap().id(), "staged@dev");
        // And the set says so of the entry, which is how the composer knows
        // where a row points before anybody picks a stage.
        assert!(set.is_default(set.get("staged@dev").unwrap()));
        assert!(!set.is_default(set.get("staged@prod").unwrap()));
    }

    /// The invariant the composer leans on: a name it lists always has exactly
    /// one entry a bare reference means. A file that leaves the choice open does
    /// not load at all, so there is no name in the list without a default.
    #[test]
    fn stages_with_no_default_between_them_take_the_file_down() {
        let dir = temp_dir("staged-no-default");
        write(
            &dir,
            "staged.yaml",
            &STAGED.replace("default_stage: dev\n", ""),
        );

        let set = load_dir(&dir, &decodes());

        assert!(set.is_empty());
        assert_eq!(set.issues().len(), 1);
        assert!(
            set.issues()[0].message.contains("default_stage"),
            "{:?}",
            set.issues()
        );
    }

    /// All or nothing: a picker missing a stage, with the reason in the log, is
    /// exactly the afternoon this is meant to save.
    #[test]
    fn one_broken_stage_takes_the_whole_file_down() {
        let dir = temp_dir("staged-broken");
        write(
            &dir,
            "staged.yaml",
            &STAGED.replace("base: https://models.internal", "base: \"not a url\""),
        );

        let set = load_dir(&dir, &decodes());

        assert!(set.is_empty());
        assert_eq!(set.issues().len(), 1);
        assert!(
            set.issues()[0].message.contains("prod"),
            "{:?}",
            set.issues()
        );
    }

    /// A later directory replaces the entry, stages and all — it does not merge
    /// its stages into the ones it displaced.
    #[test]
    fn an_override_displaces_every_stage_of_the_name() {
        let base = temp_dir("staged-layer-base");
        let mine = temp_dir("staged-layer-mine");
        write(&base, "staged.yaml", STAGED);
        write(
            &mine,
            "staged.yaml",
            &STAGED
                .replace(
                    "  prod:\n    base: https://models.internal\n    timeout: 60000\n",
                    "",
                )
                .replace("default_stage: dev\n", ""),
        );

        let set = load_dirs(&[&base, &mine], &decodes());

        assert_eq!(set.len(), 1);
        assert!(set.get("staged@prod").is_none());
        assert!(set.get("staged@dev").is_some());
    }

    #[test]
    fn a_syntax_error_carries_a_position() {
        let dir = temp_dir("syntax");
        write(&dir, "bad.yaml", "name: [unclosed\n");

        let set = load_dir(&dir, &decodes());
        assert!(set.is_empty());
        assert!(set.issues()[0].line.is_some());
    }
}
