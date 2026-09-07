//! Named decodes: one file per entry in a configuration directory's `decodes/`,
//! layered on top of the ones compiled into the binary.
//!
//! A decode is the half of a model file that describes the *answer*. Endpoints
//! that answer alike can share one instead of repeating it, and a model reaches
//! it with `decode.from`. What runs afterwards is an ordinary cascade — the
//! reference is resolved once, at load, so nothing below this layer knows the
//! difference and the trace still names the path that won.
//!
//! The built-ins are the base layer, and they are layer *zero*: a
//! `decodes/openai-chat.yaml` of your own displaces the one shipped here,
//! exactly as a later configuration directory displaces an earlier one.
//!
//! What they describe is the endpoint's own answer and nothing else. A gateway
//! refusing the call, an identity provider explaining why, an MCP server having
//! a bad day: none of those are the model talking, and reading their bodies as
//! if they were is how a decode ends up asserting things about software this
//! tool does not control. Their side of the exchange is the status, the headers
//! and the raw body, which are all shown anyway.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};
use validator::{Validate, ValidationError};

use crate::config::layout;
use crate::issue::LoadIssue;
use crate::model::{DecodeSpec, JsonPathExpr, ModelKind};

/// The decodes compiled into the binary, in the order they are loaded.
///
/// Six shapes rather than a vendor list: `vllm`, `tgi`, `groq`, `together` and
/// the rest of the OpenAI-compatible crowd all answer `openai-chat`, and an
/// entry per vendor would say they differ when they do not. Ollama gets two of
/// its own because it genuinely serves two shapes — the `OpenAI` one on `/v1`, and
/// the native one below.
const BUILTIN: &[&str] = &[
    include_str!("builtin/openai-chat.yaml"),
    include_str!("builtin/openai-embeddings.yaml"),
    include_str!("builtin/anthropic-chat.yaml"),
    include_str!("builtin/gemini-chat.yaml"),
    include_str!("builtin/ollama-native-chat.yaml"),
    include_str!("builtin/ollama-native-embeddings.yaml"),
];

/// One named decode, as declared in one YAML file.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Validate)]
#[serde(deny_unknown_fields)]
#[validate(schema(function = shareable))]
pub struct NamedDecode {
    /// How a model asks for it, unique across the directories.
    #[validate(length(min = 1, message = "a decode needs a name"))]
    pub name: String,
    /// Which kind of model this describes the answer of.
    ///
    /// A decode is not portable across kinds — `content` is meaningless to an
    /// embedding endpoint and `vectors` to a chat one — so a model reaching for
    /// the wrong one fails to load rather than decoding nothing and looking like
    /// an endpoint that answered badly.
    pub kind: ModelKind,
    /// The cascades themselves, in the shape a model file writes them.
    ///
    /// Nested rather than flattened so that the block can be lifted out of here
    /// and pasted straight under a model's `decode:` — and so that a typo in a
    /// field name is still refused instead of silently ignored.
    #[validate(nested)]
    pub decode: DecodeSpec,
    /// File it was read from, `None` for one compiled into the binary. Set by
    /// the loader, never present in YAML.
    #[serde(skip_deserializing, default)]
    pub source: Option<PathBuf>,
}

impl NamedDecode {
    /// Where this one came from, for a message somebody has to read.
    #[must_use]
    pub fn origin(&self) -> String {
        self.source.as_ref().map_or_else(
            || "the built-in decodes".to_owned(),
            |p| p.display().to_string(),
        )
    }
}

/// A shared decode is cascades and the vocabulary that goes with them, nothing
/// else.
///
/// `script:` would take over the whole decode of every model naming it, and
/// `from:` would make one reference two files deep. Both turn "which decode is
/// actually running here" into a question you answer by opening three documents.
fn shareable(entry: &NamedDecode) -> Result<(), ValidationError> {
    if entry.decode.script.is_some() {
        return Err(ValidationError::new("scripted_decode").with_message(
            "a shared decode is paths only — a `script:` belongs in the model that needs it".into(),
        ));
    }
    if !entry.decode.from.is_empty() {
        return Err(ValidationError::new("chained_decode")
            .with_message("a shared decode cannot itself declare `from:`".into()));
    }
    Ok(())
}

/// Every decode available to a model: the compiled-in ones, then whatever the
/// `decodes/` directories add on top.
#[derive(Debug, Clone, Default)]
pub struct DecodeRegistry {
    decodes: BTreeMap<String, NamedDecode>,
    issues: Vec<LoadIssue>,
}

impl DecodeRegistry {
    /// Just the decodes compiled into the binary.
    ///
    /// # Panics
    ///
    /// Panics if one of them does not parse or validate. They are compiled in,
    /// so that is a broken build rather than a broken configuration directory,
    /// and the test below catches it long before this runs.
    #[must_use]
    pub fn builtin() -> Self {
        let mut registry = Self::default();
        for text in BUILTIN {
            let entry: NamedDecode =
                serde_yaml_ng::from_str(text).expect("a built-in decode must parse");
            entry.validate().expect("a built-in decode must validate");
            registry.decodes.insert(entry.name.clone(), entry);
        }
        registry
    }

    /// Loads the built-ins, then every `decodes/` directory in the order given.
    ///
    /// Never fails: a `decodes/` that is not there adds nothing, which is what
    /// every directory looks like until somebody needs a decode of their own.
    #[must_use]
    pub fn load_dirs(dirs: &[impl AsRef<Path>]) -> Self {
        let mut registry = Self::builtin();
        for dir in dirs {
            registry.read_dir(dir.as_ref());
        }
        registry
    }

    /// Loads the built-ins plus a single configuration directory.
    #[must_use]
    pub fn load(dir: &Path) -> Self {
        Self::load_dirs(&[dir])
    }

    /// Looks a decode up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&NamedDecode> {
        self.decodes.get(name)
    }

    /// Every decode, ordered by name.
    pub fn iter(&self) -> impl Iterator<Item = &NamedDecode> {
        self.decodes.values()
    }

    /// How many are available.
    #[must_use]
    pub fn len(&self) -> usize {
        self.decodes.len()
    }

    /// Returns `true` when there is not even a built-in, which cannot happen.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.decodes.is_empty()
    }

    /// Files that failed to load, and why.
    #[must_use]
    pub fn issues(&self) -> &[LoadIssue] {
        &self.issues
    }

    /// Flattens a model's `decode:` into the cascades that will actually run.
    ///
    /// Per field, the paths the model wrote itself come first and the named
    /// decodes follow in the order they were listed — most specific wins, which
    /// is the same rule as everywhere else. `terminal_reasons` is the one field
    /// that unions instead: it is a vocabulary, not a cascade. `from:` is kept
    /// on the result rather than consumed, so what ran can still be traced back
    /// to what asked for it.
    ///
    /// # Errors
    ///
    /// Returns a message naming the unknown decode, or the one whose `kind:`
    /// does not match the model's. Both are a load-time refusal on purpose: a
    /// mistyped name resolves nothing, and a decode that resolves nothing is
    /// indistinguishable from an endpoint that answered nothing.
    pub fn resolve(&self, spec: &DecodeSpec, kind: ModelKind) -> Result<DecodeSpec, String> {
        if spec.from.is_empty() {
            return Ok(spec.clone());
        }

        let mut resolved = spec.clone();
        for name in &spec.from {
            let Some(entry) = self.get(name) else {
                return Err(format!(
                    "`decode.from` names `{name}`, which is not a decode — available: {}",
                    self.names()
                ));
            };
            if entry.kind != kind {
                return Err(format!(
                    "`decode.from` names `{name}`, which describes a `{}` answer, on a `{}` model",
                    kind_name(entry.kind),
                    kind_name(kind),
                ));
            }

            let from = &entry.decode;
            append(&mut resolved.content, &from.content);
            append(&mut resolved.delta, &from.delta);
            append(&mut resolved.tool_calls, &from.tool_calls);
            append(&mut resolved.finish_reason, &from.finish_reason);
            append(&mut resolved.usage, &from.usage);
            append(&mut resolved.error, &from.error);
            append(&mut resolved.vectors, &from.vectors);
            append_values(&mut resolved.terminal_reasons, &from.terminal_reasons);
        }

        Ok(resolved)
    }

    /// The available names, for the message a typo produces.
    fn names(&self) -> String {
        self.decodes
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Folds one directory's `decodes/` in, on top of whatever came before.
    fn read_dir(&mut self, dir: &Path) {
        let paths = match layout::entries(dir, layout::DECODES) {
            Ok(paths) => paths,
            Err(issue) => {
                warn!(%issue, "decodes directory rejected");
                self.issues.push(issue);
                return;
            }
        };

        // Which names *this* directory has already used, as opposed to the ones
        // an earlier one — or the binary — declared: the first is a typo, the
        // second is layering.
        let mut here: BTreeMap<String, PathBuf> = BTreeMap::new();

        for path in paths {
            let staged = match layout::read::<NamedDecode>(&path) {
                Ok(staged) => staged,
                Err(issue) => {
                    warn!(%issue, "decode rejected");
                    self.issues.push(issue);
                    continue;
                }
            };
            let Some(entry) = staged.into_iter().next() else {
                debug!(path = %path.display(), "file declares no decode");
                continue;
            };
            // A decode is a description of a response shape, and a shape does not
            // have environments. The endpoint that answers differently per stage
            // is the model's business, and its `from:` can differ per stage.
            if entry.stage.is_some() {
                self.issues.push(LoadIssue::new(
                    &path,
                    "a decode declares no `stages:` — a response shape is the same everywhere"
                        .to_owned(),
                ));
                continue;
            }

            let mut entry = entry.value;
            if let Err(error) = entry.validate() {
                warn!(path = %path.display(), %error, "decode rejected");
                self.issues.push(LoadIssue::new(&path, error.to_string()));
                continue;
            }

            if let Some(previous) = here.insert(entry.name.clone(), path.clone()) {
                self.issues.push(LoadIssue::new(
                    &path,
                    format!(
                        "duplicate decode name `{}`, already declared in {}",
                        entry.name,
                        previous.display()
                    ),
                ));
                continue;
            }

            entry.source = Some(path.clone());
            if let Some(shadowed) = self.decodes.get(&entry.name) {
                warn!(
                    name = %entry.name,
                    path = %path.display(),
                    shadowed = %shadowed.origin(),
                    "decode overridden by a later directory"
                );
            }
            debug!(name = %entry.name, kind = ?entry.kind, path = %path.display(), "decode loaded");
            self.decodes.insert(entry.name.clone(), entry);
        }
    }
}

/// Adds `extra` to `cascade`, skipping the paths already in it.
///
/// Two decodes agreeing on where a field lives is the normal case rather than a
/// clash — every one of them reads an error out of `$.error` — and a cascade
/// listing the same path twice would report a second path that could never be
/// reached, on every miss, for the rest of the model's life.
fn append(cascade: &mut Vec<JsonPathExpr>, extra: &[JsonPathExpr]) {
    for path in extra {
        if !cascade.iter().any(|held| held.source() == path.source()) {
            cascade.push(path.clone());
        }
    }
}

/// The terminal `finish_reason` values, merged the same way — a union rather
/// than a cascade, because every one of them is an answer to "is the model
/// done?" and there is no first-one-wins to arbitrate. A model naming two
/// decodes gets both vocabularies, which is the point of naming two.
fn append_values(values: &mut Vec<String>, extra: &[String]) {
    for value in extra {
        if !values.iter().any(|held| held == value) {
            values.push(value.clone());
        }
    }
}

fn kind_name(kind: ModelKind) -> &'static str {
    match kind {
        ModelKind::Chat => "chat",
        ModelKind::Embedding => "embedding",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-decodes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(layout::DECODES)).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(layout::DECODES).join(name), body).unwrap();
    }

    fn spec(yaml: &str) -> DecodeSpec {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    /// The built-ins are an asset compiled into the binary, so a typo in one is a
    /// broken build. This is the test that says so before a user finds out.
    #[test]
    fn every_built_in_decode_parses_and_validates() {
        let registry = DecodeRegistry::builtin();

        assert_eq!(registry.len(), BUILTIN.len());
        for name in [
            "openai-chat",
            "openai-embeddings",
            "anthropic-chat",
            "gemini-chat",
            "ollama-native-chat",
            "ollama-native-embeddings",
        ] {
            assert!(registry.get(name).is_some(), "missing `{name}`");
        }
    }

    /// Every path in every built-in is compiled by `JsonPathExpr`'s parser on the
    /// way in, so this passing means all six are valid RFC 9535 — including the
    /// filter selectors, which are the only reason `anthropic-chat` works at all.
    #[test]
    fn the_anthropic_decode_reads_text_blocks_rather_than_the_first_block() {
        let registry = DecodeRegistry::builtin();
        let entry = registry.get("anthropic-chat").unwrap();

        let raw = serde_json::json!({
            "content": [
                {"type": "thinking", "thinking": "…"},
                {"type": "text", "text": "pong"}
            ],
            "stop_reason": "end_turn"
        });
        let (completion, _) = crate::decode::chat::decode(&raw, &entry.decode);

        assert_eq!(completion.content.as_deref(), Some("pong"));
        assert_eq!(completion.finish_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn gemini_reads_its_own_spellings() {
        let registry = DecodeRegistry::builtin();
        let entry = registry.get("gemini-chat").unwrap();

        let raw = serde_json::json!({
            "candidates": [{
                "content": {"parts": [{"text": "pong"}], "role": "model"},
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 11,
                "candidatesTokenCount": 7,
                "totalTokenCount": 18
            }
        });
        let (completion, _) = crate::decode::chat::decode(&raw, &entry.decode);

        assert_eq!(completion.content.as_deref(), Some("pong"));
        assert_eq!(completion.finish_reason.as_deref(), Some("STOP"));
        let usage = completion.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(11));
        assert_eq!(usage.completion_tokens, Some(7));
    }

    /// Google nests the call under `functionCall` and calls the arguments
    /// `args`, which is a third spelling of the same two fields.
    #[test]
    fn gemini_tool_calls_normalise_like_everyone_else_s() {
        let registry = DecodeRegistry::builtin();
        let entry = registry.get("gemini-chat").unwrap();

        let raw = serde_json::json!({
            "candidates": [{
                "content": {"parts": [
                    {"text": "let me look"},
                    {"functionCall": {"name": "get_weather", "args": {"city": "Lyon"}}}
                ]},
                "finishReason": "STOP"
            }]
        });
        let (completion, _) = crate::decode::chat::decode(&raw, &entry.decode);

        assert_eq!(completion.tool_calls.len(), 1);
        assert_eq!(completion.tool_calls[0].name, "get_weather");
        assert_eq!(
            completion.tool_calls[0].arguments,
            serde_json::json!({"city": "Lyon"})
        );
        // The text part is still the answer; the call is not swept into it.
        assert_eq!(completion.content.as_deref(), Some("let me look"));
    }

    /// The case the whole feature exists for: one model, two shapes, one line.
    #[test]
    fn a_list_concatenates_the_cascades_in_order() {
        let registry = DecodeRegistry::builtin();
        let resolved = registry
            .resolve(
                &spec("from: [openai-chat, ollama-native-chat]"),
                ModelKind::Chat,
            )
            .unwrap();

        let sources: Vec<_> = resolved
            .content
            .iter()
            .map(crate::model::JsonPathExpr::source)
            .collect();
        assert_eq!(
            sources,
            ["$.choices[0].message.content", "$.message.content"]
        );
    }

    /// Every decode reads an error out of `$.error`, so the pair below agrees on
    /// that field and disagrees on every other one.
    #[test]
    fn two_decodes_agreeing_on_a_path_do_not_list_it_twice() {
        let registry = DecodeRegistry::builtin();
        let resolved = registry
            .resolve(
                &spec("from: [openai-chat, ollama-native-chat]"),
                ModelKind::Chat,
            )
            .unwrap();

        let sources: Vec<_> = resolved
            .error
            .iter()
            .map(crate::model::JsonPathExpr::source)
            .collect();
        assert_eq!(sources, ["$.error"]);
    }

    /// The one field that is a vocabulary rather than a cascade, so a model
    /// naming two shapes reads both — and neither contributes a value meaning
    /// "the model is asking for a tool", which is what makes the loop right.
    #[test]
    fn the_terminal_stop_reasons_of_two_decodes_are_pooled() {
        let registry = DecodeRegistry::builtin();
        let resolved = registry
            .resolve(
                &spec("from: [openai-chat, ollama-native-chat]"),
                ModelKind::Chat,
            )
            .unwrap();

        assert_eq!(
            resolved.terminal_reasons,
            ["stop", "length", "content_filter"]
        );
        assert!(!resolved.terminal_reasons.iter().any(|r| r == "tool_calls"));
    }

    /// Gemini answers `STOP` on the turn that asks for a function and on the turn
    /// that finishes, and Ollama's own API answers `stop` on both too. A decode
    /// claiming either is terminal would end every agent run at turn one.
    #[test]
    fn a_shape_that_cannot_tell_the_two_apart_claims_no_terminal_stop_reason() {
        let registry = DecodeRegistry::builtin();

        let gemini = &registry.get("gemini-chat").unwrap().decode;
        assert!(!gemini.terminal_reasons.iter().any(|r| r == "STOP"));
        assert!(gemini.terminal_reasons.iter().any(|r| r == "MAX_TOKENS"));

        let ollama = &registry.get("ollama-native-chat").unwrap().decode;
        assert_eq!(ollama.terminal_reasons, ["length"]);
    }

    #[test]
    fn a_path_written_beside_a_reference_is_tried_first() {
        let registry = DecodeRegistry::builtin();
        let resolved = registry
            .resolve(
                &spec("from: [openai-chat]\ncontent: [\"$.mine\"]"),
                ModelKind::Chat,
            )
            .unwrap();

        assert_eq!(resolved.content[0].source(), "$.mine");
        assert_eq!(resolved.content[1].source(), "$.choices[0].message.content");
    }

    #[test]
    fn an_unknown_name_is_refused_and_lists_what_there_is() {
        let registry = DecodeRegistry::builtin();
        let error = registry
            .resolve(&spec("from: [openai]"), ModelKind::Chat)
            .unwrap_err();

        assert!(error.contains("`openai`"), "{error}");
        assert!(error.contains("openai-chat"), "{error}");
    }

    /// Decoding nothing because the wrong shape was asked for looks exactly like
    /// an endpoint that answered nothing, so it is refused instead.
    #[test]
    fn an_embedding_decode_on_a_chat_model_is_refused() {
        let registry = DecodeRegistry::builtin();
        let error = registry
            .resolve(&spec("from: [openai-embeddings]"), ModelKind::Chat)
            .unwrap_err();

        assert!(error.contains("embedding"), "{error}");
        assert!(error.contains("chat"), "{error}");
    }

    #[test]
    fn a_directory_decode_displaces_the_built_in_of_the_same_name() {
        let dir = temp_dir("override");
        write(
            &dir,
            "openai-chat.yaml",
            "name: openai-chat\nkind: chat\ndecode:\n  content:\n    - $.mine\n",
        );

        let registry = DecodeRegistry::load(&dir);

        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let entry = registry.get("openai-chat").unwrap();
        assert_eq!(entry.decode.content[0].source(), "$.mine");
        assert!(entry.source.is_some());
    }

    #[test]
    fn a_shared_decode_may_not_carry_a_script() {
        let dir = temp_dir("scripted");
        write(
            &dir,
            "scripted.yaml",
            "name: scripted\nkind: chat\ndecode:\n  script: 'return #{};'\n",
        );

        let registry = DecodeRegistry::load(&dir);

        assert_eq!(registry.issues().len(), 1);
        assert!(
            registry.issues()[0].message.contains("paths only"),
            "{:?}",
            registry.issues()[0]
        );
    }

    #[test]
    fn a_broken_decode_does_not_take_the_others_down() {
        let dir = temp_dir("broken");
        write(
            &dir,
            "bad.yaml",
            "name: bad\nkind: chat\ndecode:\n  nope: []\n",
        );
        write(
            &dir,
            "good.yaml",
            "name: good\nkind: chat\ndecode:\n  content:\n    - $.a\n",
        );

        let registry = DecodeRegistry::load(&dir);

        assert_eq!(registry.issues().len(), 1);
        assert!(registry.get("good").is_some());
    }
}
