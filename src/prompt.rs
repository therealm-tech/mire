//! Saved prompts: one file per prompt, in a configuration directory's
//! `prompts/`.
//!
//! A model says how to reach an endpoint. A prompt says what to send it — and
//! that half is worth keeping for the same reason the first one is. The question
//! that used to make it call the tool, the one that used to make it refuse, the
//! paragraph that reproduces the bug: retyping any of those from memory is how a
//! comparison quietly stops being one.
//!
//! Read-only, like the rest of this directory. The file is the source of truth,
//! your editor writes it, the watcher picks the change up — and the same loading
//! policy applies: a bad entry is reported and skipped, the rest still work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use validator::Validate;

use crate::config::layout;
use crate::issue::LoadIssue;

/// One saved prompt: a name, and what it puts in the box.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    /// What it is called, unique within the directory.
    ///
    /// The whole of the metadata, deliberately: a prompt *is* its text, and the
    /// name is only what lets you ask for it by something shorter.
    #[validate(length(min = 1, message = "a prompt needs a name"))]
    pub name: String,
    /// The text itself, dropped in the box exactly as written.
    ///
    /// Never sent on its own — what a message becomes on the wire is the
    /// model's template's decision, here as everywhere else.
    #[validate(length(min = 1, message = "a prompt with no text puts nothing in the box"))]
    pub text: String,
}

/// Every prompt the `prompts/` directories declare, plus the entries that did
/// not load.
///
/// The order is the directory listing's: file names sorted, directories in
/// precedence order. A library is a list somebody arranged, and naming the files
/// `01-ping.yaml`, `02-refusal.yaml` is how that arrangement is written down.
#[derive(Debug, Default)]
pub struct PromptRegistry {
    prompts: Vec<Prompt>,
    /// Which file each prompt came from — see [`crate::auth::AuthRegistry`] for
    /// why the file matters and not just the name.
    sources: BTreeMap<String, PathBuf>,
    issues: Vec<LoadIssue>,
}

impl PromptRegistry {
    /// Loads every prompt file in each configuration directory's `prompts/`, in
    /// order.
    ///
    /// Never fails: a `prompts/` that is not there means no saved prompts —
    /// which is how every directory starts — and a broken file is an issue you
    /// can read in the UI rather than a refusal to start.
    #[must_use]
    pub fn load_dirs(dirs: &[impl AsRef<Path>]) -> Self {
        let mut registry = Self::default();
        for dir in dirs {
            registry.read_dir(dir.as_ref());
        }
        registry
    }

    /// Loads the prompts of a single configuration directory.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        Self::load_dirs(&[dir])
    }

    /// Folds one directory's `prompts/` in, on top of whatever earlier
    /// directories said.
    ///
    /// An overridden prompt keeps its place in the list rather than moving to the
    /// end: the order is somebody's arrangement, and replacing one text should
    /// not reshuffle the library around it.
    fn read_dir(&mut self, dir: &Path) {
        let paths = match layout::entries(dir, layout::PROMPTS) {
            Ok(paths) => paths,
            Err(issue) => {
                self.issues.push(issue);
                return;
            }
        };

        // Which names *this* directory has already used, as opposed to the ones
        // an earlier one declared: the first is a typo, the second is layering.
        let mut here: BTreeMap<String, PathBuf> = BTreeMap::new();

        for path in paths {
            let staged = match layout::read::<Prompt>(&path) {
                Ok(staged) => staged,
                Err(issue) => {
                    self.issues.push(issue);
                    continue;
                }
            };
            let Some(entry) = staged.into_iter().next() else {
                debug!(path = %path.display(), "file declares no prompt");
                continue;
            };
            // Stages are for the things that talk to an endpoint — a model, a
            // credential, a server. A saved prompt is text somebody wrote, and
            // the same text in three stages is one prompt, so the block is
            // refused here rather than quietly producing three entries fighting
            // over one name.
            if entry.stage.is_some() {
                self.issues.push(LoadIssue::new(
                    &path,
                    "a saved prompt declares no `stages:`".to_owned(),
                ));
                continue;
            }
            let prompt = entry.value;

            if let Err(errors) = prompt.validate() {
                // Named where there is a name to name it by. An entry that has
                // none leaves the file as the only thing anybody has to go on.
                let subject = if prompt.name.is_empty() {
                    "a prompt".to_owned()
                } else {
                    format!("prompt `{}`", prompt.name)
                };
                self.issues
                    .push(LoadIssue::new(&path, format!("{subject}: {errors}")));
                continue;
            }

            if let Some(previous) = here.insert(prompt.name.clone(), path.clone()) {
                self.issues.push(LoadIssue::new(
                    &path,
                    format!(
                        "duplicate prompt `{}`, already declared in {}",
                        prompt.name,
                        previous.display()
                    ),
                ));
                continue;
            }
            if let Some(previous) = self.sources.get(&prompt.name) {
                warn!(
                    name = %prompt.name,
                    path = %path.display(),
                    shadowed = %previous.display(),
                    "prompt overridden by a later directory"
                );
            }

            debug!(name = %prompt.name, "prompt loaded");
            self.sources.insert(prompt.name.clone(), path.clone());
            match self
                .prompts
                .iter_mut()
                .find(|existing| existing.name == prompt.name)
            {
                Some(existing) => *existing = prompt,
                None => self.prompts.push(prompt),
            }
        }
    }

    /// Every prompt that loaded, in the order the directory listing gives them.
    #[must_use]
    pub fn prompts(&self) -> &[Prompt] {
        &self.prompts
    }

    /// Number of usable prompts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.prompts.len()
    }

    /// Returns `true` when nothing usable was loaded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.prompts.is_empty()
    }

    /// Entries that did not load, and why.
    #[must_use]
    pub fn issues(&self) -> &[LoadIssue] {
        &self.issues
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// A configuration directory whose `prompts/` holds one file per prompt.
    fn write(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-prompts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(layout::PROMPTS)).unwrap();
        for (name, body) in files {
            std::fs::write(dir.join(layout::PROMPTS).join(name), body).unwrap();
        }
        dir
    }

    #[test]
    fn no_directory_is_no_prompts_and_no_complaint() {
        let dir = std::env::temp_dir().join(format!("mire-prompts-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let registry = PromptRegistry::load(&dir);

        assert!(registry.is_empty());
        assert!(registry.issues().is_empty());
    }

    /// The arrangement is the file names, which is why they are not sorted by
    /// prompt name: `01-` before `02-` puts a library in somebody's order.
    #[test]
    fn prompts_come_in_the_order_the_file_names_put_them_in() {
        let dir = write(
            "order",
            &[
                ("01-zebra.yaml", "name: zebra\ntext: ping\n"),
                ("02-alpha.yaml", "name: alpha\ntext: pong\n"),
            ],
        );

        let registry = PromptRegistry::load(&dir);

        let names: Vec<&str> = registry
            .prompts()
            .iter()
            .map(|prompt| prompt.name.as_str())
            .collect();
        assert_eq!(names, ["zebra", "alpha"]);
    }

    #[test]
    fn multiline_text_survives_the_round_trip() {
        let dir = write(
            "multiline",
            &[("two-lines.yaml", "name: two lines\ntext: |\n  one\n  two\n")],
        );

        let registry = PromptRegistry::load(&dir);

        assert_eq!(registry.prompts()[0].text, "one\ntwo\n");
    }

    #[test]
    fn a_duplicate_name_is_reported_not_silently_overwritten() {
        let dir = write(
            "dup",
            &[
                ("a.yaml", "name: ping\ntext: first\n"),
                ("b.yaml", "name: ping\ntext: second\n"),
            ],
        );

        let registry = PromptRegistry::load(&dir);

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.prompts()[0].text, "first");
        assert!(registry.issues()[0].message.contains("duplicate prompt"));
    }

    #[test]
    fn an_empty_prompt_is_skipped_and_the_others_still_load() {
        let dir = write(
            "empty-text",
            &[
                ("hollow.yaml", "name: hollow\ntext: ''\n"),
                ("real.yaml", "name: real\ntext: ping\n"),
            ],
        );

        let registry = PromptRegistry::load(&dir);

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.prompts()[0].name, "real");
        assert!(
            registry.issues()[0].message.contains("hollow"),
            "{:?}",
            registry.issues()
        );
    }

    #[test]
    fn a_file_that_declares_nothing_is_skipped() {
        let dir = write(
            "commented",
            &[
                ("example.yaml", "# name: ping\n# text: ping\n"),
                ("real.yaml", "name: real\ntext: ping\n"),
            ],
        );

        let registry = PromptRegistry::load(&dir);

        assert_eq!(registry.len(), 1);
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
    }

    #[test]
    fn a_later_directory_takes_a_prompt_the_earlier_one_declared() {
        let base = write("layer-base", &[("ping.yaml", "name: ping\ntext: first\n")]);
        let mine = write("layer-mine", &[("ping.yaml", "name: ping\ntext: second\n")]);

        let registry = PromptRegistry::load_dirs(&[&base, &mine]);

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.prompts()[0].text, "second");
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
    }

    /// An overridden prompt keeps its place. The order is somebody's
    /// arrangement; swapping one text should not send it to the bottom.
    #[test]
    fn an_overridden_prompt_keeps_its_place_in_the_list() {
        let base = write(
            "layer-order-base",
            &[
                ("01-ping.yaml", "name: ping\ntext: first\n"),
                ("02-pong.yaml", "name: pong\ntext: first\n"),
            ],
        );
        let mine = write(
            "layer-order-mine",
            &[("ping.yaml", "name: ping\ntext: second\n")],
        );

        let registry = PromptRegistry::load_dirs(&[&base, &mine]);

        let names: Vec<&str> = registry
            .prompts()
            .iter()
            .map(|prompt| prompt.name.as_str())
            .collect();
        assert_eq!(names, ["ping", "pong"]);
        assert_eq!(registry.prompts()[0].text, "second");
    }

    /// Overriding is only ever *across* directories. Two files in one directory
    /// claiming the same name is still the typo it always was.
    #[test]
    fn a_duplicate_inside_the_later_directory_is_still_reported() {
        let base = write(
            "layer-dup-base",
            &[("ping.yaml", "name: ping\ntext: first\n")],
        );
        let mine = write(
            "layer-dup-mine",
            &[
                ("a.yaml", "name: ping\ntext: second\n"),
                ("b.yaml", "name: ping\ntext: third\n"),
            ],
        );

        let registry = PromptRegistry::load_dirs(&[&base, &mine]);

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.prompts()[0].text, "second");
        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("duplicate prompt"));
    }

    #[test]
    fn a_syntax_error_carries_a_position_and_costs_the_file() {
        let dir = write("syntax", &[("bad.yaml", "name: [unclosed\n")]);

        let registry = PromptRegistry::load(&dir);

        assert!(registry.is_empty());
        assert!(registry.issues()[0].line.is_some());
    }

    #[test]
    fn an_unknown_key_is_rejected_by_name() {
        let dir = write("typo", &[("ping.yaml", "name: ping\ntxet: ping\n")]);

        let registry = PromptRegistry::load(&dir);

        assert!(registry.is_empty());
        assert!(
            registry.issues()[0].message.contains("txet"),
            "{:?}",
            registry.issues()
        );
    }
}
