//! What a tool call leaves behind, and the file that declares it once.
//!
//! A capture rule says that a tool answers something worth keeping — a session
//! id, a job handle, the path a server just wrote — and where to find it. See
//! [`crate::vars`] for what happens with the value afterwards.
//!
//! # Why there is a registry
//!
//! A rule is a statement about a **tool**, and a tool does not belong to a
//! model: `create_session` answers a session id at `$.sessionId` whether the
//! model that called it is `qwen3` or the one you are comparing it against.
//! Writing it in each profile's `agent.capture:` means the comparison is between
//! two files that have to be kept identical by hand, which is exactly the shape
//! of thing that quietly stops being identical.
//!
//! So `captures.yaml` sits next to the profiles, `auth.yaml`, `mcp.yaml` and
//! `prompts.yaml`, and declares named sets of rules:
//!
//! ```yaml
//! captures:
//!   - name: session
//!     rules:
//!       - tools:
//!           - create_session
//!         vars:
//!           session:
//!             - $.sessionId
//! ```
//!
//! A profile then names one instead of repeating it, in the same list as the
//! rules it does keep to itself:
//!
//! ```yaml
//! agent:
//!   capture:
//!     - use: session
//!     - tools:
//!         - read_file
//!       vars:
//!         path:
//!           - $.path
//! ```
//!
//! The list stays one list, and the order stays the file's: a `use:` expands
//! where it is written, so "later rules win" reads the same whether a rule came
//! from here or from there.
//!
//! Same loading policy as every other registry — a bad entry is reported and
//! skipped, the rest still work, and a set declared in two layered directories
//! is the later one's, with the one it displaced named in the log.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use validator::{Validate, ValidationError, ValidationErrors};

use crate::issue::LoadIssue;
use crate::pattern::NamePattern;
use crate::profile::JsonPathExpr;

/// File declaring the shared capture sets, in the profiles directory.
pub const CAPTURE_REGISTRY_FILE: &str = "captures.yaml";

/// Variables to pull out of a tool's result, and the tools they come from.
///
/// One rule is one `tools:`-plus-`vars:` pair rather than a flat map of
/// name-to-path, because the tool a value comes from is part of what the value
/// *means*: `$.id` is a different thing on `create_session` than on `read_file`,
/// and a bag keyed only by path would make that a coincidence.
///
/// See [`crate::vars`] for what happens with them.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = usable_variable_names))]
pub struct CaptureRule {
    /// Tools it applies to, as patterns matched against the whole name. Empty —
    /// the default — is every tool the run offers.
    #[serde(default)]
    pub tools: Vec<NamePattern>,
    /// Variable name to the `JSONPath` cascade that fills it. Tried in order,
    /// first hit wins, exactly like a `decode:` field.
    #[validate(length(min = 1, message = "a capture rule with no `vars` captures nothing"))]
    pub vars: BTreeMap<String, Vec<JsonPathExpr>>,
}

impl CaptureRule {
    /// Whether this rule has anything to say about `tool`.
    #[must_use]
    pub fn covers(&self, tool: &str) -> bool {
        NamePattern::any_matches(&self.tools, tool)
    }
}

/// A captured name has to be one a template can actually write.
///
/// `{{ vars.session }}` needs an identifier, and a name that only works as
/// `{{ vars["my var"] }}` is a trap set at load time and sprung in a URL — so it
/// is refused where it was written instead.
fn usable_variable_names(rule: &CaptureRule) -> Result<(), ValidationError> {
    for (name, cascade) in &rule.vars {
        let usable = !name.is_empty()
            && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !usable {
            return Err(ValidationError::new("unusable_variable_name").with_message(
                format!(
                    "`{name}` cannot be read as `vars.{name}`: use letters, digits and underscores"
                )
                .into(),
            ));
        }
        if cascade.is_empty() {
            return Err(ValidationError::new("empty_capture_cascade")
                .with_message(format!("`{name}` needs at least one JSONPath").into()));
        }
    }
    Ok(())
}

/// One entry of a profile's `agent.capture:` list.
///
/// Either a rule written on the spot, or the name of a set `captures.yaml`
/// declares. Both in the same list, because they are the same thing to the run:
/// what makes a set worth naming is that several profiles want it, not that it
/// applies differently.
#[derive(Debug, Clone)]
pub enum CaptureEntry {
    /// `- use: session` — every rule of that set, expanded here.
    Use(String),
    /// A rule this profile keeps to itself.
    Rule(CaptureRule),
}

/// The three fields an entry can carry, so that the complaint about a wrong
/// combination names the fields rather than saying no variant matched.
///
/// `#[serde(untagged)]` would do the dispatch in one line and report `data did
/// not match any variant of untagged enum CaptureEntry`, which is true and
/// useless: the reader wrote `use:` next to `vars:` and wants to be told that.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EntryFields {
    #[serde(default, rename = "use")]
    using: Option<String>,
    #[serde(default)]
    tools: Option<Vec<NamePattern>>,
    #[serde(default)]
    vars: Option<BTreeMap<String, Vec<JsonPathExpr>>>,
}

impl<'de> Deserialize<'de> for CaptureEntry {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        let fields = EntryFields::deserialize(deserializer)?;
        match (fields.using, fields.vars) {
            (Some(_), Some(_)) => Err(D::Error::custom(
                "a capture entry is either `use:` or a rule of its own, not both",
            )),
            // A set brings its own `tools:`, one per rule. Narrowing it from the
            // outside looks like it would work and would have to pick a meaning
            // — intersect? replace? — so it is refused instead of guessed.
            (Some(_), None) if fields.tools.is_some() => Err(D::Error::custom(
                "`use:` names a shared set, which carries the `tools:` of each of its rules",
            )),
            (Some(name), None) => Ok(Self::Use(name)),
            (None, Some(vars)) => Ok(Self::Rule(CaptureRule {
                tools: fields.tools.unwrap_or_default(),
                vars,
            })),
            (None, None) => Err(D::Error::custom(
                "a capture entry needs `vars:`, or `use:` to name a set from captures.yaml",
            )),
        }
    }
}

impl Serialize for CaptureEntry {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Use(name) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("use", name)?;
                map.end()
            }
            Self::Rule(rule) => rule.serialize(serializer),
        }
    }
}

impl JsonSchema for CaptureEntry {
    fn schema_name() -> Cow<'static, str> {
        "CaptureEntry".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let rule = generator.subschema_for::<CaptureRule>();
        json_schema!({
            "description": "A capture rule, or `use:` naming a set declared in captures.yaml",
            "oneOf": [
                {
                    "type": "object",
                    "properties": {
                        "use": {
                            "type": "string",
                            "description": "Name of a set declared in captures.yaml"
                        }
                    },
                    "required": ["use"],
                    "additionalProperties": false
                },
                rule
            ]
        })
    }
}

impl Validate for CaptureEntry {
    fn validate(&self) -> Result<(), ValidationErrors> {
        match self {
            // Nothing to check here that the registry does not check better: a
            // name is only wrong because nothing declares it, and this profile
            // is loaded without the registry in front of it.
            Self::Use(_) => Ok(()),
            Self::Rule(rule) => rule.validate(),
        }
    }
}

/// A named set of capture rules, declared once and used by several profiles.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
pub struct CaptureSet {
    /// What a profile writes after `use:`.
    #[validate(length(min = 1, message = "a capture set needs a name"))]
    pub name: String,
    /// The rules it stands for, expanded in the order written.
    #[validate(
        length(min = 1, message = "a capture set with no `rules` captures nothing"),
        nested
    )]
    pub rules: Vec<CaptureRule>,
}

/// A profile named a set nothing declares.
///
/// A run-time failure rather than a load issue, for the same reason an unknown
/// MCP server name is: a profile is read on its own, and the registry beside it
/// may well be edited into place a second later.
#[derive(Debug, Clone, thiserror::Error)]
#[error("`captures.yaml` declares no capture set named `{0}`")]
pub struct UnknownCaptureSet(pub String);

/// Every capture set `captures.yaml` declares, plus the entries that did not
/// load.
#[derive(Debug, Default)]
pub struct CaptureRegistry {
    sets: BTreeMap<String, CaptureSet>,
    /// Which file each set came from — see [`crate::auth::AuthRegistry`] for why
    /// the file matters and not just the name.
    sources: BTreeMap<String, PathBuf>,
    issues: Vec<LoadIssue>,
}

impl CaptureRegistry {
    /// Loads `captures.yaml` from each of the profile directories, in order.
    ///
    /// Never fails: a missing file means no shared sets — which is how every
    /// directory starts — and a broken one is an issue you can read rather than
    /// a refusal to start.
    #[must_use]
    pub fn load_dirs(dirs: &[impl AsRef<Path>]) -> Self {
        let mut registry = Self::default();
        for dir in dirs {
            registry.read(&dir.as_ref().join(CAPTURE_REGISTRY_FILE));
        }
        registry
    }

    /// Loads `captures.yaml` from a single profiles directory.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        Self::load_dirs(&[dir])
    }

    /// The set called `name`, if one is declared.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&CaptureSet> {
        self.sets.get(name)
    }

    /// Every set that loaded, ordered by name.
    pub fn iter(&self) -> impl Iterator<Item = &CaptureSet> {
        self.sets.values()
    }

    /// Entries of `captures.yaml` that did not load.
    #[must_use]
    pub fn issues(&self) -> &[LoadIssue] {
        &self.issues
    }

    /// Flattens a profile's `agent.capture:` into the rules a run applies.
    ///
    /// The order is the profile's, with each `use:` expanded where it was
    /// written: a shared set and a local rule have to be orderable against each
    /// other, and the only order anybody can reason about is the one in the file.
    ///
    /// # Errors
    ///
    /// Fails on the first `use:` naming a set nothing declares. Not a warning:
    /// the profile has said those variables get captured, and the alternative to
    /// stopping is a run that goes ahead and fails three turns later in a
    /// rendered URL, which is the failure this whole module exists to avoid.
    pub fn resolve(&self, entries: &[CaptureEntry]) -> Result<Vec<CaptureRule>, UnknownCaptureSet> {
        let mut rules = Vec::new();
        for entry in entries {
            match entry {
                CaptureEntry::Rule(rule) => rules.push(rule.clone()),
                CaptureEntry::Use(name) => {
                    let set = self
                        .get(name)
                        .ok_or_else(|| UnknownCaptureSet(name.clone()))?;
                    debug!(set = %name, rules = set.rules.len(), "capture set expanded");
                    rules.extend(set.rules.iter().cloned());
                }
            }
        }
        Ok(rules)
    }

    /// Folds one `captures.yaml` in, on top of whatever earlier directories said.
    fn read(&mut self, path: &Path) {
        if !path.exists() {
            debug!(path = %path.display(), "no shared capture sets");
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

        // Two names collide for two different reasons, exactly as they do for a
        // profile: twice in *this* file is a mistake, once here and once in a
        // directory read earlier is the point of layering.
        let mut here: BTreeSet<String> = BTreeSet::new();

        for set in file.captures {
            if let Err(errors) = set.validate() {
                let subject = if set.name.is_empty() {
                    "a capture set".to_owned()
                } else {
                    format!("capture set `{}`", set.name)
                };
                self.issues
                    .push(LoadIssue::new(path, format!("{subject}: {errors}")));
                continue;
            }

            if !here.insert(set.name.clone()) {
                self.issues.push(LoadIssue::new(
                    path,
                    format!("duplicate capture set `{}`", set.name),
                ));
                continue;
            }

            if let Some(shadowed) = self.sources.get(&set.name) {
                warn!(
                    name = %set.name,
                    path = %path.display(),
                    shadowed = %shadowed.display(),
                    "capture set overridden by a later directory"
                );
            }
            debug!(name = %set.name, rules = set.rules.len(), path = %path.display(), "capture set loaded");
            self.sources.insert(set.name.clone(), path.to_owned());
            self.sets.insert(set.name.clone(), set);
        }
    }
}

/// The document `captures.yaml` is.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryFile {
    #[serde(default)]
    captures: Vec<CaptureSet>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-captures-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, body: &str) {
        std::fs::write(dir.join(CAPTURE_REGISTRY_FILE), body).unwrap();
    }

    const SESSION: &str = "\
captures:
  - name: session
    rules:
      - tools:
          - create_session
        vars:
          session:
            - $.sessionId
            - $.session.id
";

    fn entry(yaml: &str) -> Result<CaptureEntry, String> {
        serde_yaml_ng::from_str(yaml).map_err(|error| error.to_string())
    }

    #[test]
    fn a_set_loads_with_its_patterns_and_its_cascades() {
        let dir = temp_dir("load");
        write(&dir, SESSION);

        let registry = CaptureRegistry::load(&dir);

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let set = registry.get("session").expect("it is declared");
        assert_eq!(set.rules.len(), 1);
        assert!(set.rules[0].covers("create_session"));
        assert!(!set.rules[0].covers("read_file"));
        assert_eq!(set.rules[0].vars["session"].len(), 2);
    }

    #[test]
    fn a_missing_file_is_the_ordinary_case_and_not_an_issue() {
        let dir = temp_dir("missing");

        let registry = CaptureRegistry::load(&dir);

        assert!(registry.issues().is_empty());
        assert_eq!(registry.iter().count(), 0);
    }

    #[test]
    fn a_broken_entry_is_reported_and_the_rest_still_load() {
        let dir = temp_dir("broken");
        write(
            &dir,
            "\
captures:
  - name: broken
    rules:
      - vars:
          'my id':
            - $.id
  - name: session
    rules:
      - vars:
          session:
            - $.id
",
        );

        let registry = CaptureRegistry::load(&dir);

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("my id"));
        assert!(registry.get("session").is_some());
    }

    #[test]
    fn a_set_with_no_rules_captures_nothing_and_says_so() {
        let dir = temp_dir("empty");
        write(&dir, "captures:\n  - name: session\n    rules: []\n");

        let registry = CaptureRegistry::load(&dir);

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.issues()[0].message.contains("rules"));
    }

    #[test]
    fn a_duplicate_name_in_one_file_is_reported_not_silently_overwritten() {
        let dir = temp_dir("dup");
        write(
            &dir,
            &format!("{SESSION}{}", SESSION.trim_start_matches("captures:\n")),
        );

        let registry = CaptureRegistry::load(&dir);

        assert_eq!(registry.issues().len(), 1);
        assert!(
            registry.issues()[0]
                .message
                .contains("duplicate capture set")
        );
    }

    #[test]
    fn a_later_directory_takes_a_name_the_earlier_one_declared() {
        let base = temp_dir("layer-base");
        let mine = temp_dir("layer-mine");
        write(&base, SESSION);
        write(&mine, &SESSION.replace("$.sessionId", "$.mine"));

        let registry = CaptureRegistry::load_dirs(&[&base, &mine]);

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let set = registry.get("session").expect("it is declared");
        assert_eq!(set.rules[0].vars["session"][0].source(), "$.mine");
    }

    #[test]
    fn resolving_expands_a_set_where_it_was_written() {
        let dir = temp_dir("resolve");
        write(&dir, SESSION);
        let registry = CaptureRegistry::load(&dir);

        let entries = vec![
            entry("use: session").unwrap(),
            entry("tools: [read_file]\nvars:\n  path: [$.path]\n").unwrap(),
        ];

        let rules = registry.resolve(&entries).expect("both resolve");
        assert_eq!(rules.len(), 2);
        assert!(rules[0].covers("create_session"));
        assert!(rules[1].covers("read_file"));
    }

    #[test]
    fn resolving_a_set_nothing_declares_names_the_set() {
        let dir = temp_dir("unknown");
        let registry = CaptureRegistry::load(&dir);

        let error = registry
            .resolve(&[entry("use: session").unwrap()])
            .expect_err("nothing declares it");

        assert!(error.to_string().contains("session"), "{error}");
    }

    #[test]
    fn an_entry_that_is_both_a_reference_and_a_rule_is_refused() {
        let error = entry("use: session\nvars:\n  id: [$.id]\n").expect_err("it is one or other");

        assert!(error.contains("not both"), "{error}");
    }

    #[test]
    fn a_reference_cannot_narrow_the_tools_of_the_set_it_names() {
        let error =
            entry("use: session\ntools: [create_session]\n").expect_err("the set carries those");

        assert!(error.contains("tools"), "{error}");
    }

    #[test]
    fn an_entry_that_says_neither_is_refused() {
        let error = entry("tools: [create_session]\n").expect_err("it captures nothing");

        assert!(error.contains("use"), "{error}");
    }

    /// The profile is round-tripped by `GET /api/profiles/<name>`, so a `use:`
    /// has to come back out as a `use:` rather than as the rules it stands for.
    #[test]
    fn a_reference_serialises_back_to_what_was_written() {
        let entry = CaptureEntry::Use("session".to_owned());

        let yaml = serde_yaml_ng::to_string(&entry).unwrap();

        assert_eq!(yaml.trim(), "use: session");
    }
}
