//! `prompts.yaml`, sitting next to the profiles, `auth.yaml` and `mcp.yaml`.
//!
//! A profile says how to reach an endpoint. A prompt says what to send it — and
//! that half is worth keeping for the same reason the first one is. The question
//! that used to make it call the tool, the one that used to make it refuse, the
//! paragraph that reproduces the bug: retyping any of those from memory is how a
//! comparison quietly stops being one.
//!
//! Read-only, like the rest of this directory. The file is the source of truth,
//! your editor writes it, the watcher picks the change up — and the same loading
//! policy applies: a bad entry is reported and skipped, the rest still work.

use std::collections::BTreeSet;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::debug;
use validator::Validate;

use crate::issue::LoadIssue;

/// File declaring the saved prompts, in the profiles directory.
pub const PROMPT_REGISTRY_FILE: &str = "prompts.yaml";

/// One saved prompt: a name, and what it puts in the box.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct Prompt {
    /// What it is called, unique within the file.
    ///
    /// The whole of the metadata, deliberately: a prompt *is* its text, and the
    /// name is only what lets you ask for it by something shorter.
    #[validate(length(min = 1, message = "a prompt needs a name"))]
    pub name: String,
    /// The text itself, dropped in the box exactly as written.
    ///
    /// Never sent on its own — what a message becomes on the wire is the
    /// profile's template's decision, here as everywhere else.
    #[validate(length(min = 1, message = "a prompt with no text puts nothing in the box"))]
    pub text: String,
}

/// Every prompt `prompts.yaml` declares, plus the entries that did not load.
///
/// The order is the file's rather than alphabetical: a library is a list
/// somebody arranged, and re-sorting it here would throw that arrangement away
/// with nothing to show for it.
#[derive(Debug, Default)]
pub struct PromptRegistry {
    prompts: Vec<Prompt>,
    issues: Vec<LoadIssue>,
}

impl PromptRegistry {
    /// Loads `prompts.yaml` from the profiles directory.
    ///
    /// Never fails: a missing file means no saved prompts — which is how every
    /// directory starts — and a broken one is an issue you can read in the UI
    /// rather than a refusal to start.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(PROMPT_REGISTRY_FILE);
        let mut registry = Self::default();

        if !path.exists() {
            debug!(path = %path.display(), "no saved prompts");
            return registry;
        }

        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                registry
                    .issues
                    .push(LoadIssue::new(&path, error.to_string()));
                return registry;
            }
        };

        let file: RegistryFile = match serde_yaml_ng::from_str(&text) {
            Ok(file) => file,
            Err(error) => {
                registry.issues.push(LoadIssue::from_yaml(&path, &error));
                return registry;
            }
        };

        let mut seen = BTreeSet::new();
        for prompt in file.prompts {
            if let Err(errors) = prompt.validate() {
                // Named where there is a name to name it by. An entry that has
                // none is the one case where the position in the file is all
                // anybody has to go on, so the message says which entry it is.
                let subject = if prompt.name.is_empty() {
                    "a prompt".to_owned()
                } else {
                    format!("prompt `{}`", prompt.name)
                };
                registry
                    .issues
                    .push(LoadIssue::new(&path, format!("{subject}: {errors}")));
                continue;
            }

            if !seen.insert(prompt.name.clone()) {
                registry.issues.push(LoadIssue::new(
                    &path,
                    format!("duplicate prompt `{}`", prompt.name),
                ));
                continue;
            }

            debug!(name = %prompt.name, "prompt loaded");
            registry.prompts.push(prompt);
        }

        registry
    }

    /// Every prompt that loaded, in the order the file declares them.
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

/// The document itself.
///
/// One key rather than a bare list, so that the day this file needs a second
/// thing to say it gains a key instead of changing shape under everyone.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    #[serde(default)]
    prompts: Vec<Prompt>,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-prompts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, body: &str) {
        std::fs::write(dir.join(PROMPT_REGISTRY_FILE), body).unwrap();
    }

    #[test]
    fn no_file_is_no_prompts_and_no_complaint() {
        let registry = PromptRegistry::load(&temp_dir("missing"));

        assert!(registry.is_empty());
        assert!(registry.issues().is_empty());
    }

    #[test]
    fn prompts_keep_the_order_the_file_wrote_them_in() {
        let dir = temp_dir("order");
        write(
            &dir,
            "prompts:\n  - name: zebra\n    text: ping\n  - name: alpha\n    text: pong\n",
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
        let dir = temp_dir("multiline");
        write(
            &dir,
            "prompts:\n  - name: two lines\n    text: |\n      one\n      two\n",
        );

        let registry = PromptRegistry::load(&dir);

        assert_eq!(registry.prompts()[0].text, "one\ntwo\n");
    }

    #[test]
    fn a_duplicate_name_is_reported_not_silently_overwritten() {
        let dir = temp_dir("dup");
        write(
            &dir,
            "prompts:\n  - name: ping\n    text: first\n  - name: ping\n    text: second\n",
        );

        let registry = PromptRegistry::load(&dir);

        assert_eq!(registry.len(), 1);
        assert_eq!(registry.prompts()[0].text, "first");
        assert!(registry.issues()[0].message.contains("duplicate prompt"));
    }

    #[test]
    fn an_empty_prompt_is_skipped_and_the_others_still_load() {
        let dir = temp_dir("empty-text");
        write(
            &dir,
            "prompts:\n  - name: hollow\n    text: ''\n  - name: real\n    text: ping\n",
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
    fn a_syntax_error_carries_a_position_and_costs_the_file() {
        let dir = temp_dir("syntax");
        write(&dir, "prompts: [unclosed\n");

        let registry = PromptRegistry::load(&dir);

        assert!(registry.is_empty());
        assert!(registry.issues()[0].line.is_some());
    }

    #[test]
    fn an_unknown_key_is_rejected_by_name() {
        let dir = temp_dir("typo");
        write(&dir, "prompts:\n  - name: ping\n    txet: ping\n");

        let registry = PromptRegistry::load(&dir);

        assert!(registry.is_empty());
        assert!(
            registry.issues()[0].message.contains("txet"),
            "{:?}",
            registry.issues()
        );
    }
}
