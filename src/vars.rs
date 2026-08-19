//! What a tool call left behind.
//!
//! A tool answers, and something in that answer is the thing the *next* request
//! needs: a session id, a job handle, the path a server just wrote. `capture:`
//! on a server in `mcp.yaml` names those, by `JSONPath`, per tool:
//!
//! ```yaml
//! servers:
//!   - name: dev
//!     url: https://dev.internal/mcp
//!     capture:
//!       - tools: [create_session]
//!         vars:
//!           session: [$.sessionId]
//! ```
//!
//! and a hook on that same server reads them back as `vars`:
//!
//! ```yaml
//! hooks:
//!   - name: audit
//!     on: [after]
//!     action:
//!       kind: http
//!       url: https://audit.internal/sessions/{{ vars.session }}
//! ```
//!
//! # Why `JSONPath`
//!
//! Because [`crate::decode`] already reads responses that way, cascades and all:
//! a list of paths, tried in order, first hit wins. One notation for "the bit of
//! this JSON I mean", compiled when `mcp.yaml` loads, so a typo is a startup
//! issue naming the file and the field rather than a variable that is silently
//! never set.
//!
//! # What gets read
//!
//! `structuredContent` when the MCP server sent one, and the result text parsed
//! as JSON otherwise. A tool answering something that is not JSON captures
//! nothing — there is no path into prose.
//!
//! Only a real server's tools, at that. A profile's simulated `tools:` are
//! answered in this process and belong to no server, so nothing captures from
//! them — see [`crate::mcp::capture`].
//!
//! # When nothing is captured
//!
//! Not an error: the run carries on, and a hook naming the variable says so
//! itself. It is a warning, though, on both counts — a result no path could be
//! read out of, and a cascade that resolved none of its paths, the second
//! naming the paths it tried. A rule that covers this tool has said this tool
//! produces this variable, so a rule that comes back empty is a statement that
//! did not hold, and the alternative is finding out three turns later from a
//! URL with a hole in it.
//!
//! A rule with no `tools:` covers every tool, so it warns on every call that
//! does not carry the variable. That is the rule saying something untrue about
//! most of the run rather than the log being noisy — name the tools.
//!
//! Every call also reports what it set in its
//! [`ToolInvocation`](crate::agent::ToolInvocation), so a `vars` that turns out
//! empty is a fact you can read in the trace rather than a mystery in a
//! rendered URL.
//!
//! # Scope and lifetime
//!
//! One bag per agent run, shared by every server that run talks to, thrown away
//! with the run. The *rules* are per server; the bag they fill is not, so a
//! session opened on one server is an address another server's hook can render.
//! Not per turn either: the point is precisely to carry something from the turn
//! that produced it to the one that needs it. Two rules writing the same name is
//! last-write-wins, in declaration order — the same thing a second call to the
//! same tool does, which is what makes a captured value "the latest one" rather
//! than "the first one anybody managed to set".
//!
//! # When a hook sees its own call
//!
//! A `before` hook sees what earlier calls captured. An `after` hook also sees
//! what *its own* call just captured, because the capture happens as soon as
//! the result lands and before the `after` hooks fire — which is the only
//! ordering that lets a hook report on the session the call it wrapped has just
//! opened.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tracing::{debug, warn};

use crate::decode::paths::resolve_one;
use crate::mcp::capture::CaptureRule;
use crate::profile::JsonPathExpr;

/// Variables by name, in the order a template will see them.
pub type Captured = BTreeMap<String, Value>;

/// One run's variables, and the rules that fill them.
///
/// Shared by the loop and by every MCP client the run built, so a
/// [`Mutex`] rather than a `&mut` — and a plain one, like the journals next to
/// it, because every critical section here is a map operation with no `await`
/// in it.
#[derive(Debug)]
pub struct Vars {
    seen: Mutex<Captured>,
}

impl Vars {
    /// An empty bag, for a run that is about to start filling it.
    ///
    /// It carries no rules: those belong to the server whose tools produce the
    /// values, and arrive with each call. One bag, many servers' rules — which
    /// is exactly the asymmetry the run has.
    #[must_use]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            seen: Mutex::new(Captured::new()),
        })
    }

    /// Everything captured so far, for a template about to render.
    #[must_use]
    pub fn snapshot(&self) -> Captured {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }

    /// Runs the `rules` that cover `tool` against what it answered, and returns
    /// what *this* call set — which is what the trace reports.
    ///
    /// `rules` are the ones the calling server declares, not the run's: a run
    /// reaching two servers applies each server's rules to its own tools, and
    /// pooling them would let one server's `$.sessionId` claim another's answer.
    ///
    /// `structured` is the server's `structuredContent` when there was one; it
    /// wins over `text`, because a server that went to the trouble of sending
    /// structured output has said which of the two is the data.
    pub fn capture(
        &self,
        rules: &[CaptureRule],
        tool: &str,
        structured: Option<&Value>,
        text: &str,
    ) -> Captured {
        let mut captured = Captured::new();
        let applicable: Vec<&CaptureRule> = rules.iter().filter(|rule| rule.covers(tool)).collect();
        if applicable.is_empty() {
            return captured;
        }

        // Parsed once for every rule that applies, and only once it is known
        // that one does.
        let Some(document) = structured
            .cloned()
            .or_else(|| serde_json::from_str::<Value>(text).ok())
        else {
            warn!(
                %tool,
                result = %head(text),
                "capture: the result is not JSON, so there is nothing for a path to select"
            );
            return captured;
        };

        for rule in applicable {
            for (name, cascade) in &rule.vars {
                if let Some((path, value)) = resolve_one(&document, cascade) {
                    debug!(%tool, var = %name, path = %path.source(), "captured");
                    captured.insert(name.clone(), value.clone());
                } else {
                    warn!(
                        %tool,
                        var = %name,
                        tried = %tried(cascade),
                        "capture: no path resolved, so the variable stays unset"
                    );
                }
            }
        }

        if let Ok(mut seen) = self.seen.lock() {
            seen.extend(captured.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        captured
    }
}

/// The paths a cascade tried, as written in `mcp.yaml`.
fn tried(cascade: &[JsonPathExpr]) -> String {
    cascade
        .iter()
        .map(JsonPathExpr::source)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The head of a result, short enough for a log line.
///
/// Quoted, because the answer to "why did nothing capture" is usually visible in
/// the first few words — an empty result, prose, a fenced block — and a line
/// saying only that the parse failed sends the reader back to the traffic pane
/// to guess.
fn head(text: &str) -> String {
    const MAX: usize = 120;

    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = collapsed.chars().take(MAX).collect();
    if collapsed.chars().count() > MAX {
        short.push('…');
    }
    format!("`{short}`")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::pattern::NamePattern;
    use crate::profile::JsonPathExpr;

    fn rules(tools: &[&str], vars: &[(&str, &[&str])]) -> Vec<CaptureRule> {
        vec![CaptureRule {
            tools: tools
                .iter()
                .map(|t| NamePattern::compile(t).expect("the pattern compiles"))
                .collect(),
            vars: vars
                .iter()
                .map(|(name, paths)| {
                    let cascade: Vec<JsonPathExpr> = paths
                        .iter()
                        .map(|p| p.parse().expect("the path compiles"))
                        .collect();
                    ((*name).to_owned(), cascade)
                })
                .collect(),
        }]
    }

    #[test]
    fn a_path_that_resolves_sets_the_variable() {
        let vars = Vars::new();
        let rules = rules(&["create_session"], &[("session", &["$.id"])]);

        let captured = vars.capture(&rules, "create_session", None, r#"{"id": "abc-123"}"#);

        assert_eq!(captured.get("session"), Some(&json!("abc-123")));
        assert_eq!(vars.snapshot().get("session"), Some(&json!("abc-123")));
    }

    #[test]
    fn a_rule_only_runs_for_the_tools_it_names() {
        let vars = Vars::new();
        let rules = rules(&["create_.*"], &[("session", &["$.id"])]);

        assert!(
            vars.capture(&rules, "read_file", None, r#"{"id": "abc"}"#)
                .is_empty()
        );
        assert!(vars.snapshot().is_empty());
    }

    #[test]
    fn no_tools_at_all_covers_every_tool() {
        let vars = Vars::new();
        let rules = rules(&[], &[("anything", &["$.id"])]);

        assert_eq!(
            vars.capture(&rules, "whatever", None, r#"{"id": 7}"#)
                .get("anything"),
            Some(&json!(7))
        );
    }

    #[test]
    fn the_first_resolving_path_of_a_cascade_wins() {
        let vars = Vars::new();
        let rules = rules(
            &["create_session"],
            &[("session", &["$.sessionId", "$.session.id"])],
        );

        let captured = vars.capture(
            &rules,
            "create_session",
            None,
            r#"{"session": {"id": "deep"}}"#,
        );

        assert_eq!(captured.get("session"), Some(&json!("deep")));
    }

    #[test]
    fn a_cascade_that_misses_leaves_the_variable_unset() {
        let vars = Vars::new();
        let rules = rules(&["t"], &[("session", &["$.nope"])]);

        assert!(
            vars.capture(&rules, "t", None, r#"{"id": "abc"}"#)
                .is_empty()
        );
        // Absent rather than empty: a template reading it says so, which beats
        // rendering a URL with a hole in it.
        assert!(!vars.snapshot().contains_key("session"));
    }

    #[test]
    fn a_result_that_is_not_json_captures_nothing_and_is_not_an_error() {
        let vars = Vars::new();
        let rules = rules(&["t"], &[("session", &["$.id"])]);

        assert!(
            vars.capture(&rules, "t", None, "it worked, thanks for asking")
                .is_empty()
        );
    }

    #[test]
    fn structured_content_wins_over_the_text_rendering_of_it() {
        let vars = Vars::new();
        let rules = rules(&["t"], &[("session", &["$.id"])]);
        let structured = json!({"id": "from-structured"});

        let captured = vars.capture(&rules, "t", Some(&structured), r#"{"id": "from-text"}"#);

        assert_eq!(captured.get("session"), Some(&json!("from-structured")));
    }

    #[test]
    fn a_later_call_replaces_what_an_earlier_one_captured() {
        let vars = Vars::new();
        let rules = rules(&["t"], &[("session", &["$.id"])]);

        vars.capture(&rules, "t", None, r#"{"id": "first"}"#);
        vars.capture(&rules, "t", None, r#"{"id": "second"}"#);

        assert_eq!(vars.snapshot().get("session"), Some(&json!("second")));
    }

    #[test]
    fn a_call_that_captures_nothing_leaves_an_earlier_value_alone() {
        let vars = Vars::new();
        let rules = rules(&["t"], &[("session", &["$.id"])]);

        vars.capture(&rules, "t", None, r#"{"id": "first"}"#);
        vars.capture(&rules, "t", None, r#"{"unrelated": true}"#);

        assert_eq!(vars.snapshot().get("session"), Some(&json!("first")));
    }

    #[test]
    fn a_captured_value_keeps_its_json_shape() {
        let vars = Vars::new();
        let rules = rules(&["t"], &[("files", &["$.files"])]);

        let captured = vars.capture(&rules, "t", None, r#"{"files": ["a", "b"]}"#);

        assert_eq!(captured.get("files"), Some(&json!(["a", "b"])));
    }

    #[test]
    fn a_server_with_no_rules_captures_nothing_at_all() {
        let vars = Vars::new();

        assert!(vars.capture(&[], "t", None, r#"{"id": "abc"}"#).is_empty());
        assert!(vars.snapshot().is_empty());
    }

    /// One bag, two servers' rules: what `dev` opened is what `audit`'s hook
    /// renders, and neither server's rules run against the other's tools.
    #[test]
    fn one_run_pools_what_two_servers_capture_without_pooling_their_rules() {
        let vars = Vars::new();
        let dev = rules(&["create_session"], &[("session", &["$.id"])]);
        let audit = rules(&["record"], &[("receipt", &["$.id"])]);

        vars.capture(&dev, "create_session", None, r#"{"id": "abc-123"}"#);
        // `audit` has a rule reading `$.id` too, and it does not fire on a tool
        // that is not its own.
        vars.capture(&audit, "create_session", None, r#"{"id": "abc-123"}"#);
        vars.capture(&audit, "record", None, r#"{"id": "r-1"}"#);

        let seen = vars.snapshot();
        assert_eq!(seen.get("session"), Some(&json!("abc-123")));
        assert_eq!(seen.get("receipt"), Some(&json!("r-1")));
    }
}
