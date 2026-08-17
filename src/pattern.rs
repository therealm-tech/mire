//! A regex matched against a whole name.
//!
//! Two places name tools by pattern — a hook's `tools:` in `mcp.yaml` and a
//! capture rule's `tools:` in a profile — and both mean the same thing by it, so
//! both compile the same type rather than each anchoring a regex its own way.
//!
//! # Why anchored
//!
//! A plain `write_file` means that one tool. That is what a list of names meant
//! before it took patterns, and a gate that silently widened to
//! `overwrite_file_backup` the day patterns landed would be a hole nobody opened
//! on purpose. The same goes for a `files:` entry naming `report.pdf`: it must
//! not quietly pick up `report.pdf.bak`. Widening is available by asking for it:
//! `write_.*`, or `.*` for everything.
//!
//! Anchoring wraps the whole pattern rather than its first branch, so
//! `read_.*|write_.*` is two anchored alternatives and not one anchored
//! alternative beside a free-floating one.

use regex::Regex;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// One name pattern, compiled when the file that declared it loads.
#[derive(Debug, Clone)]
pub struct NamePattern {
    /// What the file wrote. Kept for the descriptor: a UI listing the compiled
    /// form would be showing anchors nobody typed.
    source: String,
    /// The anchored form, which is what actually decides.
    matcher: Regex,
}

impl NamePattern {
    /// Compiles one pattern.
    ///
    /// # Errors
    ///
    /// A one-line reason quoting the pattern, for the load issue.
    pub fn compile(pattern: &str) -> Result<Self, String> {
        Regex::new(&format!("^(?:{pattern})$"))
            .map(|matcher| Self {
                source: pattern.to_owned(),
                matcher,
            })
            // Recompiled as written, so the complaint quotes the pattern rather
            // than the anchors this put around it.
            .map_err(|_| match Regex::new(pattern) {
                Err(error) => format!("`{pattern}` is not a regex: {}", why(&error)),
                Ok(_) => format!("`{pattern}` is not a regex once anchored to a whole name"),
            })
    }

    /// The pattern as the file wrote it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Whether it names `candidate`, whole.
    #[must_use]
    pub fn matches(&self, candidate: &str) -> bool {
        self.matcher.is_match(candidate)
    }

    /// Whether an empty list would have covered `candidate` anyway.
    ///
    /// Empty means every name, in both places that take these. Written once
    /// here so the two cannot drift.
    #[must_use]
    pub fn any_matches(patterns: &[Self], candidate: &str) -> bool {
        patterns.is_empty() || patterns.iter().any(|pattern| pattern.matches(candidate))
    }
}

/// Compiled on the way in, exactly like [`crate::profile::JsonPathExpr`]: a
/// typo is a startup issue naming the file and the field, not a rule that
/// silently matches nothing at call time.
impl<'de> Deserialize<'de> for NamePattern {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let source = String::deserialize(deserializer)?;
        Self::compile(&source).map_err(serde::de::Error::custom)
    }
}

impl Serialize for NamePattern {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.source)
    }
}

impl JsonSchema for NamePattern {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "NamePattern".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = String::json_schema(generator);
        schema.insert(
            "description".into(),
            "A regex matched against a whole name, e.g. `write_.*`".into(),
        );
        schema
    }
}

/// The one useful line of a regex complaint.
///
/// `regex` renders a caret diagram across several lines, which is a fine thing
/// to read in a terminal and a bad thing to put in a one-line load issue. The
/// last line is the reason itself.
fn why(error: &regex::Error) -> String {
    error
        .to_string()
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map_or_else(
            || error.to_string(),
            |line| line.trim_start_matches("error: ").to_owned(),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(source: &str) -> NamePattern {
        NamePattern::compile(source).expect("the pattern compiles")
    }

    #[test]
    fn a_pattern_covers_the_tools_it_describes() {
        let write = pattern("write_.*");

        assert!(write.matches("write_file"));
        assert!(write.matches("write_anything_at_all"));
        assert!(!write.matches("read_file"));
    }

    #[test]
    fn a_pattern_matches_the_whole_name_or_not_at_all() {
        let exact = pattern("write_file");

        assert!(exact.matches("write_file"));
        // The widening a bare name must never do on its own.
        assert!(!exact.matches("write_file_backup"));
        assert!(!exact.matches("overwrite_file"));
    }

    #[test]
    fn an_alternation_is_anchored_as_a_whole_rather_than_by_its_first_branch() {
        let either = pattern("read_.*|write_.*");

        assert!(either.matches("read_file"));
        assert!(either.matches("write_file"));
        assert!(!either.matches("delete_file"));
    }

    #[test]
    fn a_pattern_that_is_not_a_regex_says_so_quoting_what_was_written() {
        let error = NamePattern::compile("write_(").expect_err("an unclosed group is not a regex");

        assert!(error.contains("write_("), "{error}");
        assert!(
            !error.contains("^(?:"),
            "the anchors are ours, not theirs: {error}"
        );
    }

    #[test]
    fn an_empty_list_covers_every_name() {
        assert!(NamePattern::any_matches(&[], "anything"));
        assert!(NamePattern::any_matches(
            &[pattern("write_.*")],
            "write_file"
        ));
        assert!(!NamePattern::any_matches(
            &[pattern("write_.*")],
            "read_file"
        ));
    }
}
