//! What a tool call leaves behind, declared next to the server that answers it.
//!
//! A capture rule says that a tool answers something worth keeping — a session
//! id, a job handle, the path a server just wrote — and where to find it. See
//! [`crate::vars`] for what happens with the value afterwards.
//!
//! # Why this lives on the server
//!
//! A rule is a statement about a **tool**, and a tool belongs to the server that
//! advertises it. `create_session` answers a session id at `$.sessionId`
//! whichever model happens to call it, so writing the rule into each model's
//! profile made a comparison between two files that had to be kept identical by
//! hand — exactly the shape of thing that quietly stops being identical.
//!
//! So the rule sits with the server, in `mcp.yaml`, beside the `headers:` and
//! `hooks:` that read what it captures:
//!
//! ```yaml
//! servers:
//!   - name: dev
//!     url: https://dev.internal/mcp
//!     capture:
//!       - tools:
//!           - create_session
//!         vars:
//!           session:
//!             - $.sessionId
//! ```
//!
//! Every chat profile that reaches that server captures the same thing, because
//! there is only one place it is written.
//!
//! # What this means for simulated tools
//!
//! Nothing: a profile's `tools:` are answered inside this process and belong to
//! no server, so they capture nothing. Capture is what a *real* server's answer
//! leaves behind.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use validator::{Validate, ValidationError};

use crate::pattern::NamePattern;
use crate::profile::JsonPathExpr;

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
    /// the default — is every tool the server offers.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(yaml: &str) -> Result<CaptureRule, String> {
        serde_yaml_ng::from_str(yaml).map_err(|error| error.to_string())
    }

    #[test]
    fn a_rule_loads_with_its_patterns_and_its_cascades() {
        let rule = rule("tools: [create_.*]\nvars:\n  session: [$.sessionId, $.session.id]\n")
            .expect("it loads");

        assert!(rule.covers("create_session"));
        assert!(!rule.covers("read_file"));
        assert_eq!(rule.vars["session"].len(), 2);
    }

    #[test]
    fn a_rule_with_no_tools_covers_every_tool() {
        let rule = rule("vars:\n  id: [$.id]\n").expect("it loads");

        assert!(rule.covers("anything_at_all"));
    }

    #[test]
    fn a_path_that_is_not_a_jsonpath_is_caught_where_it_was_written() {
        let error = rule("vars:\n  id: ['not a path']\n").expect_err("it is not a path");

        // Named down to the variable, because "a parser error somewhere in
        // mcp.yaml" is not a thing anybody can act on.
        assert!(error.contains("vars.id"), "{error}");
    }

    #[test]
    fn a_pattern_that_is_not_a_regex_is_caught_where_it_was_written() {
        let error = rule("tools: ['create_(']\nvars:\n  id: [$.id]\n").expect_err("it is no regex");

        assert!(error.contains("create_("), "{error}");
    }

    #[test]
    fn a_name_no_template_could_write_is_refused() {
        let rule = rule("vars:\n  'my id': [$.id]\n").expect("it parses");

        let errors = rule.validate().expect_err("it does not validate");

        assert!(errors.to_string().contains("my id"), "{errors}");
    }

    #[test]
    fn a_rule_that_captures_nothing_says_so() {
        let rule = rule("tools: [create_session]\nvars: {}\n").expect("it parses");

        let errors = rule.validate().expect_err("it does not validate");

        assert!(errors.to_string().contains("vars"), "{errors}");
    }
}
