//! What we say when a file in the configuration directory does not load.
//!
//! Shared by the profile loader and the auth registry, because the policy is the
//! same for both: never refuse to start over one bad file. You reach for `mire`
//! when something is already wrong; a tool that will not come up until its own
//! config is perfect is a tool you cannot use to find out what is wrong.

use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;

/// One file (or one entry in it) that could not be loaded.
#[derive(Debug, Clone, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoadIssue {
    /// Path of the offending file.
    pub file: PathBuf,
    /// What went wrong, naming the field where the underlying parser tells us.
    pub message: String,
    /// 1-based line, when the parser reports a position.
    pub line: Option<usize>,
    /// 1-based column, when the parser reports a position.
    pub column: Option<usize>,
}

impl LoadIssue {
    /// An issue with no position information.
    #[must_use]
    pub fn new(file: impl AsRef<Path>, message: impl Into<String>) -> Self {
        Self {
            file: file.as_ref().to_owned(),
            message: message.into(),
            line: None,
            column: None,
        }
    }

    /// Attaches a 1-based position.
    #[must_use]
    pub fn at(mut self, line: Option<usize>, column: Option<usize>) -> Self {
        self.line = line;
        self.column = column;
        self
    }

    /// Builds an issue from a YAML parse error, carrying its position across.
    #[must_use]
    pub fn from_yaml(file: impl AsRef<Path>, error: &serde_yaml_ng::Error) -> Self {
        let location = error.location();
        Self::new(file, error.to_string()).at(
            location.as_ref().map(serde_yaml_ng::Location::line),
            location.as_ref().map(serde_yaml_ng::Location::column),
        )
    }
}

impl std::fmt::Display for LoadIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.file.display())?;
        if let (Some(line), Some(column)) = (self.line, self.column) {
            write!(f, ":{line}:{column}")?;
        }
        write!(f, ": {}", self.message)
    }
}
