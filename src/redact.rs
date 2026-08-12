//! Credential values and the redaction applied to everything that leaves the process.
//!
//! Two complementary mechanisms, because neither is sufficient alone:
//!
//! * [`Secret`] makes a credential impossible to print by accident — its `Debug`,
//!   `Display` and `Serialize` impls all emit [`MASK`].
//! * [`Redactor`] scrubs values that were *copied* somewhere out of our control: an
//!   upstream error body echoing the token back, a rendered request line, a trace.
//!
//! Every response, trace and log line goes through a [`Redactor`] before leaving.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;

/// What a redacted value is replaced with.
pub const MASK: &str = "***";

/// Needles shorter than this are not substring-matched: a two-character "secret"
/// would shred every unrelated body it happens to appear in.
const MIN_NEEDLE_LEN: usize = 4;

/// Header names whose value is masked wholesale, regardless of its content.
const SENSITIVE_HEADERS: &[&str] = &[
    "authorization",
    "proxy-authorization",
    "cookie",
    "set-cookie",
    "x-api-key",
    "api-key",
];

/// A credential value.
///
/// The inner string is only reachable through [`Secret::expose`], which makes every
/// call site that can leak it greppable.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a credential value.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the underlying value. Audit every call.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Returns `true` when the credential is the empty string.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(MASK)
    }
}

impl serde::Serialize for Secret {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(MASK)
    }
}

/// Deserialisation is allowed so that a credential typed in the UI lands in a
/// `Secret` immediately, rather than sitting in a plain `String` field that some
/// future `Debug` would happily print.
impl<'de> serde::Deserialize<'de> for Secret {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self)
    }
}

impl schemars::JsonSchema for Secret {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "Secret".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = String::json_schema(generator);
        schema.insert(
            "description".into(),
            "A credential. Write-only: it is always returned masked.".into(),
        );
        schema.insert("writeOnly".into(), true.into());
        schema
    }
}

/// Scrubs known credential values out of arbitrary text, JSON and headers.
///
/// Built per call from whatever auth provider ran, so it covers exactly the secrets
/// that request could have echoed back.
#[derive(Clone, Debug, Default)]
pub struct Redactor {
    /// Sorted longest-first so that a token is masked before any prefix of it.
    needles: Vec<String>,
}

impl Redactor {
    /// An empty redactor: masks sensitive headers, but has no values to scrub.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a credential to scrub from all output.
    pub fn add(&mut self, secret: &Secret) {
        let value = secret.expose();
        if value.len() < MIN_NEEDLE_LEN || self.needles.iter().any(|n| n == value) {
            return;
        }
        self.needles.push(value.to_owned());
        self.needles.sort_by_key(|n| std::cmp::Reverse(n.len()));
    }

    /// Builder form of [`Redactor::add`].
    #[must_use]
    pub fn with(mut self, secret: &Secret) -> Self {
        self.add(secret);
        self
    }

    /// Folds another redactor's credentials into this one.
    pub fn merge(&mut self, other: &Self) {
        for needle in &other.needles {
            self.add(&Secret::new(needle.clone()));
        }
    }

    /// Replaces every known credential occurrence with [`MASK`].
    #[must_use]
    pub fn text(&self, input: &str) -> String {
        let mut out = input.to_owned();
        for needle in &self.needles {
            if out.contains(needle.as_str()) {
                out = out.replace(needle.as_str(), MASK);
            }
        }
        out
    }

    /// Applies [`Redactor::text`] to every string in a JSON document, keys included.
    #[must_use]
    pub fn json(&self, input: &Value) -> Value {
        if self.needles.is_empty() {
            return input.clone();
        }
        match input {
            Value::String(s) => Value::String(self.text(s)),
            Value::Array(items) => Value::Array(items.iter().map(|v| self.json(v)).collect()),
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(k, v)| (self.text(k), self.json(v)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }

    /// Masks a header map: sensitive names lose their value entirely, the rest are
    /// scrubbed for known credentials.
    #[must_use]
    pub fn headers(&self, headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
        headers
            .iter()
            .map(|(name, value)| {
                let masked = if SENSITIVE_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
                    MASK.to_owned()
                } else {
                    self.text(value)
                };
                (name.clone(), masked)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_never_renders_its_value() {
        let secret = Secret::new("super-secret-token");
        assert_eq!(format!("{secret:?}"), MASK);
        assert_eq!(format!("{secret}"), MASK);
        assert_eq!(
            serde_json::to_string(&secret).unwrap(),
            format!("\"{MASK}\"")
        );
    }

    #[test]
    fn redactor_scrubs_text_and_json() {
        let redactor = Redactor::new().with(&Secret::new("super-secret-token"));
        assert_eq!(
            redactor.text("Bearer super-secret-token"),
            format!("Bearer {MASK}")
        );

        let raw = serde_json::json!({
            "error": "token super-secret-token is expired",
            "nested": ["super-secret-token"],
        });
        let scrubbed = redactor.json(&raw);
        assert!(
            !serde_json::to_string(&scrubbed)
                .unwrap()
                .contains("super-secret-token")
        );
    }

    #[test]
    fn sensitive_headers_are_masked_even_for_unknown_values() {
        let redactor = Redactor::new();
        let headers = BTreeMap::from([
            ("Authorization".to_owned(), "Bearer whatever".to_owned()),
            ("Content-Type".to_owned(), "application/json".to_owned()),
        ]);
        let masked = redactor.headers(&headers);
        assert_eq!(masked["Authorization"], MASK);
        assert_eq!(masked["Content-Type"], "application/json");
    }

    #[test]
    fn short_needles_are_not_substring_matched() {
        let redactor = Redactor::new().with(&Secret::new("ab"));
        assert_eq!(redactor.text("abstract"), "abstract");
    }

    #[test]
    fn longer_secret_is_masked_before_its_prefix() {
        let mut redactor = Redactor::new();
        redactor.add(&Secret::new("tok_abcd"));
        redactor.add(&Secret::new("tok_abcd_efgh"));
        assert_eq!(redactor.text("tok_abcd_efgh"), MASK);
    }
}
