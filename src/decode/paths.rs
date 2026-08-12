//! Cascading `JSONPath` resolution.
//!
//! A field is declared as a list of paths and they are tried in order. The first
//! one that resolves to something other than nothing-or-null wins. That is what
//! lets one profile cover a provider that moved its content field between
//! versions, without a script.

use serde_json::Value;

use crate::profile::JsonPathExpr;

/// The paths that were tried, for the decode trace.
#[must_use]
pub fn sources(paths: &[JsonPathExpr]) -> Vec<String> {
    paths.iter().map(|path| path.source().to_owned()).collect()
}

/// Runs the cascade and returns the winning path with every node it selected.
///
/// A path counts as resolved when it selects at least one node and the first node
/// is not `null` — an endpoint that answers `"content": null` has not answered,
/// and the next path in the cascade deserves its turn.
#[must_use]
pub fn resolve<'a>(
    raw: &'a Value,
    paths: &'a [JsonPathExpr],
) -> Option<(&'a JsonPathExpr, Vec<&'a Value>)> {
    for path in paths {
        let nodes: Vec<&Value> = path.compiled().query(raw).all();
        match nodes.first() {
            Some(first) if !first.is_null() => return Some((path, nodes)),
            _ => {}
        }
    }
    None
}

/// Resolves a cascade expected to yield a single value.
#[must_use]
pub fn resolve_one<'a>(
    raw: &'a Value,
    paths: &'a [JsonPathExpr],
) -> Option<(&'a JsonPathExpr, &'a Value)> {
    let (path, nodes) = resolve(raw, paths)?;
    nodes.first().map(|node| (path, *node))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(sources: &[&str]) -> Vec<JsonPathExpr> {
        sources.iter().map(|s| s.parse().unwrap()).collect()
    }

    #[test]
    fn the_first_resolving_path_wins() {
        let raw = serde_json::json!({"output": {"text": "from the fallback"}});
        let cascade = paths(&["$.choices[0].message.content", "$.output.text"]);

        let (path, value) = resolve_one(&raw, &cascade).unwrap();
        assert_eq!(path.source(), "$.output.text");
        assert_eq!(value, "from the fallback");
    }

    #[test]
    fn a_null_does_not_count_as_resolved() {
        let raw = serde_json::json!({"a": null, "b": "real"});
        let cascade = paths(&["$.a", "$.b"]);

        let (path, _) = resolve_one(&raw, &cascade).unwrap();
        assert_eq!(path.source(), "$.b");
    }

    #[test]
    fn a_cascade_that_misses_entirely_returns_nothing() {
        let raw = serde_json::json!({"unexpected": 1});
        assert!(resolve(&raw, &paths(&["$.a", "$.b"])).is_none());
    }

    #[test]
    fn a_wildcard_path_returns_every_node() {
        let raw = serde_json::json!({"content": [{"text": "one"}, {"text": "two"}]});
        let cascade = paths(&["$.content[*].text"]);
        let (_, nodes) = resolve(&raw, &cascade).unwrap();
        assert_eq!(nodes.len(), 2);
    }
}
