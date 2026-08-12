//! Rhai scripts: the escape hatch for endpoints the declarative level cannot
//! describe.
//!
//! The order matters and is not negotiable: a `MiniJinja` template and a `JSONPath`
//! cascade first, a script only when the shape genuinely does not fit. A script
//! is code in a config file — it is harder to read, harder to review, and it
//! survives worse. It earns its place exactly when the alternative is not
//! supporting the endpoint at all.
//!
//! # Sandbox
//!
//! Rhai has no file, network or process access to begin with — there is nothing
//! to take away. What is left to bound is how long a script may run and how much
//! it may allocate, and that is what [`engine`] configures:
//!
//! * [`MAX_OPERATIONS`] — the primary bound. Deterministic, and since every
//!   operation is cheap it bounds wall-clock time too.
//! * [`MAX_RUNTIME`] — a belt-and-braces deadline for the pathological case,
//!   checked from Rhai's progress callback.
//! * caps on string, array and map sizes, on call depth, and on expression
//!   nesting, so a script cannot exhaust memory or the stack.
//! * `eval` is disabled: a script that builds and runs more script is beyond
//!   anything this tool needs.
//!
//! Scripts are compiled when the profile loads, so a syntax error names the file
//! at startup rather than at call time.

use std::cell::Cell;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use rhai::{AST, Dynamic, Engine, Scope};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Operations a single script may execute. Generous for reshaping a response,
/// far below anything that would hang the process.
const MAX_OPERATIONS: u64 = 500_000;

/// Wall-clock ceiling for one script run.
const MAX_RUNTIME: Duration = Duration::from_secs(1);

/// Longest string a script may build — enough for a large request body.
const MAX_STRING_SIZE: usize = 1_000_000;

/// Largest array a script may build. An embedding response is the reason this is
/// not smaller.
const MAX_ARRAY_SIZE: usize = 100_000;

/// Largest map a script may build.
const MAX_MAP_SIZE: usize = 10_000;

/// Deepest function call nesting.
const MAX_CALL_LEVELS: usize = 32;

thread_local! {
    /// When the script running on this thread must be given up on.
    ///
    /// A thread-local rather than engine state because the engine is shared: the
    /// deadline belongs to the run, not to the interpreter.
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// The one sandboxed engine, shared by every script.
fn engine() -> &'static Engine {
    static ENGINE: LazyLock<Engine> = LazyLock::new(|| {
        let mut engine = Engine::new();
        engine.set_max_operations(MAX_OPERATIONS);
        engine.set_max_string_size(MAX_STRING_SIZE);
        engine.set_max_array_size(MAX_ARRAY_SIZE);
        engine.set_max_map_size(MAX_MAP_SIZE);
        engine.set_max_call_levels(MAX_CALL_LEVELS);
        engine.set_max_expr_depths(64, 64);
        // Building and running more script is beyond anything a decode needs.
        engine.disable_symbol("eval");

        engine.on_progress(|_operations| {
            let expired = DEADLINE.with(|deadline| {
                deadline
                    .get()
                    .is_some_and(|deadline| Instant::now() >= deadline)
            });
            expired.then(|| Dynamic::from("the script ran for too long"))
        });

        engine
    });
    &ENGINE
}

/// A script, compiled when its profile loaded.
///
/// Mirrors [`crate::profile::JsonPathExpr`]: the source survives for display, the
/// compiled form is what runs, and a mistake is a startup error naming the file.
#[derive(Debug, Clone)]
pub struct ScriptSource {
    source: String,
    ast: AST,
}

impl ScriptSource {
    /// The script as written in the YAML.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Runs the script with `scope` bound, and returns whatever it evaluated to.
    ///
    /// # Errors
    ///
    /// Returns [`ScriptError::Runtime`] for anything the script did wrong, the
    /// sandbox limits included.
    pub fn run(&self, scope: &mut Scope<'_>) -> Result<Dynamic, ScriptError> {
        DEADLINE.with(|deadline| deadline.set(Some(Instant::now() + MAX_RUNTIME)));
        let outcome = engine().eval_ast_with_scope::<Dynamic>(scope, &self.ast);
        DEADLINE.with(|deadline| deadline.set(None));

        outcome.map_err(|error| ScriptError::Runtime {
            message: error.to_string(),
        })
    }
}

impl std::str::FromStr for ScriptSource {
    type Err = ScriptError;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let ast = engine()
            .compile(source)
            .map_err(|error| ScriptError::Compile {
                message: error.to_string(),
            })?;
        Ok(Self {
            source: source.to_owned(),
            ast,
        })
    }
}

impl<'de> Deserialize<'de> for ScriptSource {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let source = String::deserialize(deserializer)?;
        source.parse().map_err(serde::de::Error::custom)
    }
}

impl Serialize for ScriptSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.source)
    }
}

impl JsonSchema for ScriptSource {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ScriptSource".into()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        let mut schema = String::json_schema(generator);
        schema.insert(
            "description".into(),
            "A Rhai script, compiled when the profile loads.".into(),
        );
        schema
    }
}

/// Why a script did not produce a usable answer.
#[derive(Debug, thiserror::Error)]
pub enum ScriptError {
    /// The script does not compile.
    #[error("the script does not compile: {message}")]
    Compile {
        /// Rhai's message, with its position.
        message: String,
    },

    /// The script ran and failed — including on hitting a sandbox limit.
    #[error("the script failed: {message}")]
    Runtime {
        /// Rhai's message, with its position.
        message: String,
    },

    /// The script ran but returned something unusable.
    #[error("the script returned {found}, expected {expected}")]
    WrongShape {
        /// What came back.
        found: String,
        /// What was wanted.
        expected: &'static str,
    },
}

/// Turns a JSON value into something a script can walk.
///
/// # Errors
///
/// Fails only on a value Rhai cannot represent, which `serde_json` does not produce.
pub fn to_dynamic(value: &serde_json::Value) -> Result<Dynamic, ScriptError> {
    rhai::serde::to_dynamic(value).map_err(|error| ScriptError::Runtime {
        message: error.to_string(),
    })
}

/// Reads a script's return value back into a typed shape.
///
/// # Errors
///
/// Returns [`ScriptError::WrongShape`] when the script returned something the
/// target type cannot be built from, naming what it wanted.
pub fn from_dynamic<T: serde::de::DeserializeOwned>(
    value: &Dynamic,
    expected: &'static str,
) -> Result<T, ScriptError> {
    let found = value.type_name().to_owned();
    rhai::serde::from_dynamic(value).map_err(|error| ScriptError::WrongShape {
        found: format!("{found} ({error})"),
        expected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn script(source: &str) -> ScriptSource {
        source.parse().unwrap()
    }

    #[test]
    fn a_script_sees_its_scope_and_returns_a_value() {
        let script = script("`hello ${name}`");
        let mut scope = Scope::new();
        scope.push("name", "world".to_string());

        let value = script.run(&mut scope).unwrap();
        assert_eq!(value.into_string().unwrap(), "hello world");
    }

    #[test]
    fn a_syntax_error_is_caught_at_compile_time() {
        let error = "let x = ;".parse::<ScriptSource>().unwrap_err();
        assert!(matches!(error, ScriptError::Compile { .. }));
    }

    #[test]
    fn an_endless_loop_is_stopped_by_the_operation_limit() {
        let script = script("let n = 0; loop { n += 1; }");
        let error = script.run(&mut Scope::new()).unwrap_err();

        let ScriptError::Runtime { message } = error else {
            panic!("expected a runtime error, got {error:?}");
        };
        assert!(
            message.to_lowercase().contains("operation"),
            "expected the operation limit to bite, got: {message}"
        );
    }

    #[test]
    fn a_runaway_allocation_is_stopped() {
        let script = script("let s = \"x\"; loop { s += s; }");
        assert!(script.run(&mut Scope::new()).is_err());
    }

    #[test]
    fn there_is_no_way_to_reach_the_filesystem_or_the_network() {
        // Rhai ships none of these to begin with; the test is here so that
        // enabling a package that does would fail loudly.
        for forbidden in [
            "open_file(\"/etc/passwd\")",
            "read_file(\"/etc/passwd\")",
            "http_get(\"http://example.com\")",
            "System.exec(\"ls\")",
        ] {
            let outcome = forbidden
                .parse::<ScriptSource>()
                .and_then(|script| script.run(&mut Scope::new()));
            assert!(outcome.is_err(), "`{forbidden}` should not resolve");
        }
    }

    #[test]
    fn eval_is_disabled() {
        let outcome = "eval(\"1 + 1\")"
            .parse::<ScriptSource>()
            .and_then(|script| script.run(&mut Scope::new()));
        assert!(outcome.is_err(), "eval should not be reachable");
    }

    #[test]
    fn json_crosses_into_a_script_and_back() {
        let raw = serde_json::json!({"deep": {"list": [1, 2, 3]}});
        let script = script("raw.deep.list.len()");

        let mut scope = Scope::new();
        scope.push("raw", to_dynamic(&raw).unwrap());

        let value = script.run(&mut scope).unwrap();
        assert_eq!(value.as_int().unwrap(), 3);
    }
}
