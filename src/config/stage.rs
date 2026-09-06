//! Stages: one file, one entry, several environments.
//!
//! `dev`, `preprod`, `prod` — the same endpoint at three addresses, with three
//! client ids and two timeouts. Written as three files, the three stop being
//! copies of each other the day somebody fixes a decode path in one of them. A
//! stage is the alternative: the file declares what varies, under `stages:`, and
//! reads it back with `${ stage.… }`.
//!
//! ```yaml
//! url: ${ stage.base }/v1/chat/completions
//! default_stage: dev
//! stages:
//!   dev:
//!     base: http://127.0.0.1:11435
//!   prod:
//!     base: https://models.internal
//! ```
//!
//! # Two syntaxes, two moments
//!
//! `${ … }` is resolved **when the file loads**, and sees exactly one thing:
//! `stage`, that stage's variables. `{{ … }}` is resolved **when a call goes
//! out**, and sees what it always saw — `messages`, `params`, `uploads`, `env`
//! (the process environment), `auth`, `vars`. The delimiters differ because both
//! live in the same `request.template:`, and a load-time pass that rendered
//! `{{ messages | tojson }}` would render it against a context holding no
//! messages.
//!
//! A file that declares no `stages:` is not substituted at all, so a `${` in a
//! saved prompt stays the text somebody wrote. In a file that does declare them,
//! `$${` is a literal `${`.
//!
//! # One entry per stage, validated at load
//!
//! A file declaring two stages is read twice, and each reading is a whole entry:
//! the URL is parsed, the `JSONPath`s compile, the header names are checked. A
//! typo in the `prod` address is then a startup issue naming the file and the
//! stage, rather than something you find out the afternoon you first pick prod.
//!
//! It is all or nothing, per file: a stage that does not expand takes the entry
//! down with it. Loading the rest would mean a stage missing from the picker
//! with the reason visible only in the log, which is the shape of thing this
//! whole module exists to avoid.
//!
//! # `name@stage`
//!
//! An entry declaring stages is addressed as `name@stage` — in `POST /api/call`,
//! in a `mcpServers:` list, in another file's `auth:`. A bare `name` is that
//! entry's default stage, which is what makes every file and every request
//! written before stages existed mean exactly what it meant. `@` is therefore not
//! allowed in a `name:`.
//!
//! Stages are **independent across entries**: a model in `prod`, an auth provider
//! in `preprod` and an MCP server in `dev` is an ordinary run. `prod` on one
//! entry is related to `prod` on another only by both being spelled that way —
//! nothing checks that two files declare the same stage names, because nothing
//! should.

use std::sync::LazyLock;

use minijinja::{Environment, UndefinedBehavior};
use serde_yaml_ng::Value;

/// Separates an entry's name from its stage, in every reference.
pub const SEPARATOR: char = '@';

/// The key holding one map of variables per stage.
const STAGES: &str = "stages";

/// The key naming the stage a bare reference means.
const DEFAULT_STAGE: &str = "default_stage";

/// Expressions are compiled one at a time and thrown away; only the
/// configuration — strict undefined — is worth keeping.
///
/// Separate from [`crate::render`]'s environment for the reason its own docs
/// give the other way round: body templates rely on undefined being falsy
/// (`{% if tools %}`), and a stage variable nobody declared must not quietly
/// render as nothing. `| default(…)` still covers the ones that really are
/// optional.
static EXPRESSIONS: LazyLock<Environment<'static>> = LazyLock::new(|| {
    let mut environment = Environment::new();
    environment.set_undefined_behavior(UndefinedBehavior::Strict);
    environment
});

/// One reading of a file: the document as it expands in one stage.
#[derive(Debug, Clone)]
pub struct Staged<T> {
    /// The stage this reading belongs to, `None` for a file declaring none.
    pub stage: Option<String>,
    /// `true` when a bare reference to the entry means this reading.
    pub default: bool,
    /// The entry itself.
    pub value: T,
}

/// How an entry is addressed: `name`, or `name@stage`.
#[must_use]
pub fn id(name: &str, stage: Option<&str>) -> String {
    match stage {
        Some(stage) => format!("{name}{SEPARATOR}{stage}"),
        None => name.to_owned(),
    }
}

/// Splits a reference into the entry's name and the stage it asks for.
///
/// `None` for the stage is a bare name, which every registry resolves to the
/// entry's default stage rather than to a missing one.
#[must_use]
pub fn split(reference: &str) -> (&str, Option<&str>) {
    match reference.split_once(SEPARATOR) {
        Some((name, stage)) => (name, Some(stage)),
        None => (reference, None),
    }
}

/// Whether a document declares stages at all.
///
/// Read before expanding rather than as part of it: a file that declares none is
/// deserialised straight from its text, which is what keeps a bad field pointing
/// at the line it is on.
#[must_use]
pub fn declared(document: &Value) -> bool {
    document
        .as_mapping()
        .is_some_and(|map| map.contains_key(STAGES) || map.contains_key(DEFAULT_STAGE))
}

/// Reads `document` once per stage it declares.
///
/// A document declaring no `stages:` comes back as itself, untouched and unread:
/// nothing is substituted, and a `${` in it is text.
///
/// # Errors
///
/// Returns the message to put in a [`crate::issue::LoadIssue`] — a malformed
/// `stages:` block, a `default_stage:` naming a stage nobody declared, or an
/// expression that could not be resolved, naming the stage and the expression.
pub fn expand(document: Value) -> Result<Vec<Staged<Value>>, String> {
    let Value::Mapping(mut map) = document else {
        // Not a mapping at all: leave it alone and let the entry's own
        // deserialisation say what it expected, at the position it expected it.
        return Ok(vec![Staged {
            stage: None,
            default: true,
            value: document,
        }]);
    };

    check_name(&map)?;

    let stages = map.remove(STAGES);
    let default = map.remove(DEFAULT_STAGE);

    let Some(stages) = stages else {
        if default.is_some() {
            return Err(format!("`{DEFAULT_STAGE}` without `{STAGES}`"));
        }
        return Ok(vec![Staged {
            stage: None,
            default: true,
            value: Value::Mapping(map),
        }]);
    };

    let stages = stage_variables(stages)?;
    let default = default_stage(default, &stages)?;
    let document = Value::Mapping(map);

    stages
        .iter()
        .map(|(name, variables)| {
            let value =
                render(&document, variables).map_err(|error| format!("stage `{name}`: {error}"))?;
            Ok(Staged {
                stage: Some(name.clone()),
                default: *name == default,
                value,
            })
        })
        .collect()
}

/// Refuses a `name:` carrying the separator, whichever kind of entry this is.
///
/// Every kind is named by a top-level `name:`, and `qwen3@prod` as a name would
/// make `qwen3@prod` mean two things depending on which file you read first.
fn check_name(map: &serde_yaml_ng::Mapping) -> Result<(), String> {
    let Some(Value::String(name)) = map.get("name") else {
        return Ok(());
    };
    if name.contains(SEPARATOR) {
        return Err(format!(
            "`{name}` cannot be a name: `{SEPARATOR}` separates a name from its stage"
        ));
    }
    Ok(())
}

/// The `stages:` block, checked into the map of maps it has to be.
fn stage_variables(stages: Value) -> Result<Vec<(String, Value)>, String> {
    let Value::Mapping(stages) = stages else {
        return Err(format!(
            "`{STAGES}` must be a mapping of stage to variables"
        ));
    };
    if stages.is_empty() {
        return Err(format!("`{STAGES}` declares none"));
    }

    stages
        .into_iter()
        .map(|(name, variables)| {
            let Value::String(name) = name else {
                return Err(format!("`{STAGES}` holds a stage whose name is not a string"));
            };
            if name.is_empty() || name.contains(SEPARATOR) {
                return Err(format!(
                    "`{name}` cannot be a stage name: it is empty, or carries the `{SEPARATOR}` that separates it from a name"
                ));
            }
            if !matches!(variables, Value::Mapping(_)) {
                return Err(format!("stage `{name}` must hold a mapping of variables"));
            }
            Ok((name, variables))
        })
        .collect()
}

/// Which stage a bare reference means.
///
/// Required as soon as there is a choice to make. A single stage is its own
/// default: there is nothing to disambiguate, and a file made of one stage plus
/// a line naming it is a line that says nothing.
fn default_stage(declared: Option<Value>, stages: &[(String, Value)]) -> Result<String, String> {
    match declared {
        Some(Value::String(name)) => {
            if stages.iter().any(|(stage, _)| *stage == name) {
                Ok(name)
            } else {
                Err(format!(
                    "`{DEFAULT_STAGE}: {name}` names a stage that is not declared, among {}",
                    stages
                        .iter()
                        .map(|(stage, _)| format!("`{stage}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ))
            }
        }
        Some(_) => Err(format!("`{DEFAULT_STAGE}` must be a stage name")),
        None => match stages {
            [(only, _)] => Ok(only.clone()),
            _ => Err(format!(
                "`{DEFAULT_STAGE}` is needed to say which of {} a bare name means",
                stages
                    .iter()
                    .map(|(stage, _)| format!("`{stage}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        },
    }
}

/// Substitutes every string in `value`, recursively.
fn render(value: &Value, variables: &Value) -> Result<Value, String> {
    let context = minijinja::Value::from_serialize(serde_yaml_ng::Mapping::from_iter([(
        Value::String("stage".to_owned()),
        variables.clone(),
    )]));
    walk(value, &context)
}

fn walk(value: &Value, context: &minijinja::Value) -> Result<Value, String> {
    match value {
        Value::String(source) => substitute(source, context),
        Value::Sequence(items) => items
            .iter()
            .map(|item| walk(item, context))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Sequence),
        Value::Mapping(entries) => entries
            .iter()
            .map(|(key, value)| Ok((walk(key, context)?, walk(value, context)?)))
            .collect::<Result<serde_yaml_ng::Mapping, String>>()
            .map(Value::Mapping),
        Value::Tagged(tagged) => Ok(Value::Tagged(Box::new(serde_yaml_ng::value::TaggedValue {
            tag: tagged.tag.clone(),
            value: walk(&tagged.value, context)?,
        }))),
        other => Ok(other.clone()),
    }
}

/// One string, with every `${ … }` in it resolved.
///
/// A string that is *exactly* one expression keeps the expression's own type, so
/// `allowed_hosts: ${ stage.hosts }` is a list and `timeout_ms: ${ stage.timeout }`
/// is a number. With anything around it, the result is text — which is the only
/// thing a URL with a host substituted into it could be.
fn substitute(source: &str, context: &minijinja::Value) -> Result<Value, String> {
    let parts = scan(source)?;
    if let [Part::Expression(expression)] = parts.as_slice() {
        return typed(expression, context);
    }

    let mut rendered = String::with_capacity(source.len());
    for part in &parts {
        match part {
            Part::Literal(text) => rendered.push_str(text),
            Part::Expression(expression) => {
                rendered.push_str(&evaluate(expression, context)?.to_string());
            }
        }
    }
    Ok(Value::String(rendered))
}

/// The value of a lone expression, with its type.
fn typed(expression: &str, context: &minijinja::Value) -> Result<Value, String> {
    let value = evaluate(expression, context)?;
    serde_yaml_ng::to_value(&value).map_err(|error| format!("`${{{expression}}}`: {error}"))
}

fn evaluate(expression: &str, context: &minijinja::Value) -> Result<minijinja::Value, String> {
    let compiled = EXPRESSIONS
        .compile_expression(expression)
        .map_err(|error| format!("`${{{expression}}}`: {error}"))?;
    let value = compiled
        .eval(context)
        .map_err(|error| format!("`${{{expression}}}`: {}", reason(&error)))?;
    if value.is_undefined() {
        return Err(format!("`${{{expression}}}` is not declared by this stage"));
    }
    Ok(value)
}

/// A `MiniJinja` error's own sentence, without the "expression" preamble and the
/// line number of a one-line expression nobody can see.
fn reason(error: &minijinja::Error) -> String {
    error
        .detail()
        .map_or_else(|| error.kind().to_string(), ToOwned::to_owned)
}

/// A piece of a string: text, or something to evaluate.
#[derive(Debug, PartialEq, Eq)]
enum Part {
    Literal(String),
    Expression(String),
}

/// Splits a string into literals and `${ … }` expressions.
///
/// The closing brace is the one that closes it: braces, brackets and parentheses
/// nest, and a `}` inside a quoted string is a character rather than the end —
/// `${ stage.host | default("a}b") }` is one expression. An unterminated `${` is
/// an error rather than text, because a template that silently stopped being one
/// is how you end up sending a URL with a `${` in it.
fn scan(source: &str) -> Result<Vec<Part>, String> {
    let mut parts = Vec::new();
    let mut literal = String::new();
    let mut rest = source;

    while let Some(start) = rest.find("${") {
        // `$${` is how a file that declares stages writes a literal `${`.
        if rest[..start].ends_with('$') {
            literal.push_str(&rest[..start - 1]);
            literal.push_str("${");
            rest = &rest[start + 2..];
            continue;
        }

        literal.push_str(&rest[..start]);
        let body = &rest[start + 2..];
        let end = closing(body).ok_or_else(|| format!("`${{` is never closed in `{source}`"))?;

        if !literal.is_empty() {
            parts.push(Part::Literal(std::mem::take(&mut literal)));
        }
        parts.push(Part::Expression(body[..end].trim().to_owned()));
        rest = &body[end + 1..];
    }

    literal.push_str(rest);
    if !literal.is_empty() {
        parts.push(Part::Literal(literal));
    }
    Ok(parts)
}

/// Offset of the `}` closing an expression that has just started.
fn closing(body: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut quote = None;
    let mut escaped = false;

    for (offset, character) in body.char_indices() {
        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == delimiter {
                quote = None;
            }
            continue;
        }
        match character {
            '\'' | '"' => quote = Some(character),
            '{' | '[' | '(' => depth += 1,
            ']' | ')' => depth = depth.saturating_sub(1),
            '}' if depth == 0 => return Some(offset),
            '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expands `text` and returns each reading as `(stage, document)`.
    fn expand_text(text: &str) -> Result<Vec<(Option<String>, Value)>, String> {
        let document: Value = serde_yaml_ng::from_str(text).expect("valid yaml");
        Ok(expand(document)?
            .into_iter()
            .map(|staged| (staged.stage, staged.value))
            .collect())
    }

    fn field<'a>(document: &'a Value, key: &str) -> &'a Value {
        document.get(key).unwrap_or_else(|| panic!("no `{key}`"))
    }

    /// The guarantee every file written before stages existed relies on: no
    /// `stages:`, no substitution, and a `${` is text somebody typed.
    #[test]
    fn a_document_declaring_no_stages_is_not_touched() {
        let readings =
            expand_text("name: qwen3\nurl: https://models.internal/${ nope }\n").unwrap();

        assert_eq!(readings.len(), 1);
        assert_eq!(readings[0].0, None);
        assert_eq!(
            field(&readings[0].1, "url").as_str(),
            Some("https://models.internal/${ nope }")
        );
    }

    #[test]
    fn one_file_is_read_once_per_stage() {
        let readings = expand_text(
            "name: qwen3\nurl: ${ stage.base }/v1/chat\ndefault_stage: dev\nstages:\n  dev:\n    base: http://127.0.0.1:11435\n  prod:\n    base: https://models.internal\n",
        )
        .unwrap();

        assert_eq!(readings.len(), 2);
        assert_eq!(readings[0].0.as_deref(), Some("dev"));
        assert_eq!(
            field(&readings[0].1, "url").as_str(),
            Some("http://127.0.0.1:11435/v1/chat")
        );
        assert_eq!(
            field(&readings[1].1, "url").as_str(),
            Some("https://models.internal/v1/chat")
        );
    }

    /// Neither `stages:` nor `default_stage:` survives into the entry: they are
    /// the recipe, not a field of the thing.
    #[test]
    fn the_stage_block_is_consumed_rather_than_handed_on() {
        let readings =
            expand_text("name: a\nstages:\n  dev:\n    base: x\ndefault_stage: dev\n").unwrap();

        let mapping = readings[0].1.as_mapping().unwrap();
        assert!(!mapping.contains_key("stages"));
        assert!(!mapping.contains_key("default_stage"));
    }

    /// A string that is exactly one expression keeps the variable's own type, so
    /// a list stays a list and a number stays a number.
    #[test]
    fn a_lone_expression_keeps_the_type_it_resolved_to() {
        let readings = expand_text(
            "timeout_ms: ${ stage.timeout }\nallowed_hosts: ${ stage.hosts }\nstages:\n  dev:\n    timeout: 60000\n    hosts:\n      - 127.0.0.1\n      - models.internal\n",
        )
        .unwrap();

        assert_eq!(field(&readings[0].1, "timeout_ms").as_u64(), Some(60_000));
        assert_eq!(
            field(&readings[0].1, "allowed_hosts")
                .as_sequence()
                .map(Vec::len),
            Some(2)
        );
    }

    /// The two syntaxes in one file, which is the whole reason they differ: the
    /// load-time pass resolves its own and leaves the call-time one alone.
    #[test]
    fn a_call_time_template_is_left_for_the_call() {
        let readings = expand_text(
            "request:\n  template: '{\"model\": \"${ stage.served_model }\", \"messages\": {{ messages | tojson }}}'\nstages:\n  dev:\n    served_model: qwen3:0.6b\n",
        )
        .unwrap();

        assert_eq!(
            field(field(&readings[0].1, "request"), "template").as_str(),
            Some(r#"{"model": "qwen3:0.6b", "messages": {{ messages | tojson }}}"#)
        );
    }

    #[test]
    fn a_variable_the_stage_does_not_declare_names_itself() {
        let error = expand_text("url: ${ stage.base }\nstages:\n  prod:\n    other: x\n")
            .expect_err("undefined");

        assert!(error.contains("prod"), "{error}");
        assert!(error.contains("stage.base"), "{error}");
    }

    /// Strict undefined is the rule; `default` is how a file says a variable is
    /// genuinely optional.
    #[test]
    fn an_optional_variable_takes_a_default() {
        let readings = expand_text(
            "timeout_ms: ${ stage.timeout | default(30000) }\nstages:\n  dev:\n    base: x\n",
        )
        .unwrap();

        assert_eq!(field(&readings[0].1, "timeout_ms").as_u64(), Some(30_000));
    }

    #[test]
    fn a_single_stage_is_its_own_default() {
        let document: Value =
            serde_yaml_ng::from_str("name: a\nstages:\n  dev:\n    base: x\n").unwrap();

        let readings = expand(document).unwrap();

        assert!(readings[0].default);
    }

    #[test]
    fn two_stages_need_one_of_them_named_as_the_default() {
        let error = expand_text("name: a\nstages:\n  dev:\n    base: x\n  prod:\n    base: y\n")
            .expect_err("ambiguous");

        assert!(error.contains("default_stage"), "{error}");
        assert!(
            error.contains("`dev`") && error.contains("`prod`"),
            "{error}"
        );
    }

    #[test]
    fn a_default_naming_a_stage_nobody_declared_is_refused() {
        let error = expand_text(
            "name: a\ndefault_stage: preprod\nstages:\n  dev:\n    base: x\n  prod:\n    base: y\n",
        )
        .expect_err("unknown default");

        assert!(error.contains("preprod"), "{error}");
    }

    #[test]
    fn a_default_stage_on_a_file_that_declares_none_is_refused() {
        let error = expand_text("name: a\ndefault_stage: dev\n").expect_err("no stages");

        assert!(error.contains("without `stages`"), "{error}");
    }

    /// `@` is the separator, so a name carrying one would make `qwen3@prod` mean
    /// two different things depending on which file was read first.
    #[test]
    fn a_name_carrying_the_separator_is_refused() {
        let error =
            expand_text("name: qwen3@prod\nstages:\n  dev:\n    base: x\n").expect_err("bad name");

        assert!(error.contains("qwen3@prod"), "{error}");
    }

    #[test]
    fn a_stage_holding_no_variables_is_refused() {
        let error = expand_text("name: a\nstages:\n  dev: not-a-mapping\n").expect_err("bad stage");

        assert!(error.contains("dev"), "{error}");
    }

    #[test]
    fn a_doubled_dollar_is_a_literal_one() {
        let readings = expand_text(
            "template: '$${ stage.base } ${ stage.base }'\nstages:\n  dev:\n    base: x\n",
        )
        .unwrap();

        assert_eq!(
            field(&readings[0].1, "template").as_str(),
            Some("${ stage.base } x")
        );
    }

    /// The brace that closes an expression is the one that closes it: a `}` in a
    /// quoted string is a character.
    #[test]
    fn a_brace_inside_a_string_does_not_close_an_expression() {
        let readings = expand_text(
            "header: ${ stage.missing | default(\"a}b\") }\nstages:\n  dev:\n    base: x\n",
        )
        .unwrap();

        assert_eq!(field(&readings[0].1, "header").as_str(), Some("a}b"));
    }

    #[test]
    fn an_expression_nobody_closed_is_an_error_rather_than_text() {
        let error = expand_text("url: ${ stage.base\nstages:\n  dev:\n    base: x\n")
            .expect_err("unterminated");

        assert!(error.contains("never closed"), "{error}");
    }

    #[test]
    fn a_reference_splits_into_a_name_and_a_stage() {
        assert_eq!(split("qwen3@prod"), ("qwen3", Some("prod")));
        assert_eq!(split("qwen3"), ("qwen3", None));
        assert_eq!(id("qwen3", Some("prod")), "qwen3@prod");
        assert_eq!(id("qwen3", None), "qwen3");
    }
}
