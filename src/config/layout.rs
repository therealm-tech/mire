//! What a configuration directory holds, and how each kind of thing is read out
//! of it.
//!
//! One subdirectory per kind, one file per entry: `models/qwen3.yaml` declares
//! the model named `qwen3`, `auth/keycloak-user.yaml` the provider named
//! `keycloak-user`. The file is a single document with the entry's fields at the
//! top level — the same shape a model file has always had, now the shape
//! everything has.
//!
//! The name is still the `name:` field rather than the file name. The file name
//! is a convenience for whoever is looking at the directory; renaming a file must
//! not silently rename the thing every other file references.
//!
//! Two absences are deliberately not failures. A subdirectory that is not there
//! declares nothing, which is what a directory layered on top of another one to
//! add two prompts looks like. And a document that declares nothing — an empty
//! file, or one that is entirely commented out — is skipped, which is what makes
//! a commented-out example file a usable way to ship one.

use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use tracing::debug;

use crate::issue::LoadIssue;

/// Subdirectory holding one model per file.
pub const MODELS: &str = "models";

/// Subdirectory holding one auth provider per file.
pub const AUTH: &str = "auth";

/// Subdirectory holding one MCP server per file.
pub const MCP: &str = "mcp";

/// Subdirectory holding one saved prompt per file.
pub const PROMPTS: &str = "prompts";

/// Every `*.yaml` / `*.yml` file in `dir/<kind>`, sorted by path.
///
/// Sorted so that the order is the directory listing's rather than the
/// filesystem's, which is neither stable nor the same on two machines. It is the
/// order saved prompts appear in, and the order two entries fighting over a name
/// are reported in.
///
/// Not recursive: a subdirectory of `models/` is somebody's own arrangement, and
/// walking into it would turn `models/old/qwen3.yaml` into a live model.
///
/// # Errors
///
/// Returns an issue when the subdirectory exists but cannot be read. Not being
/// there at all is not an error — it declares nothing.
pub fn entries(dir: &Path, kind: &str) -> Result<Vec<PathBuf>, LoadIssue> {
    let dir = dir.join(kind);
    let listing = match std::fs::read_dir(&dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            debug!(path = %dir.display(), "no such subdirectory, nothing declared");
            return Ok(Vec::new());
        }
        Err(error) => return Err(LoadIssue::new(&dir, error.to_string())),
    };

    let mut paths: Vec<PathBuf> = listing
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_file() && is_yaml(path))
        .collect();
    paths.sort();
    Ok(paths)
}

/// Reads one file as a single `T`.
///
/// `Ok(None)` for a document that declares nothing: YAML reads an empty file, or
/// one holding only comments, as `null`, and [`Option`] is what turns that into
/// an answer rather than a complaint about a missing field.
///
/// # Errors
///
/// Returns an issue for an unreadable file, a syntax error, or a field the entry
/// does not have. The position comes across when the parser reports one.
pub fn read<T: DeserializeOwned>(path: &Path) -> Result<Option<T>, LoadIssue> {
    let text =
        std::fs::read_to_string(path).map_err(|error| LoadIssue::new(path, error.to_string()))?;
    serde_yaml_ng::from_str(&text).map_err(|error| LoadIssue::from_yaml(path, &error))
}

fn is_yaml(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("yaml" | "yml")
    )
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Deserialize)]
    struct Entry {
        #[expect(dead_code, reason = "parsed to prove the document was read")]
        name: String,
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mire-layout-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(MODELS)).unwrap();
        dir
    }

    #[test]
    fn a_subdirectory_that_is_not_there_declares_nothing() {
        let dir = temp_dir("absent");

        assert!(entries(&dir, AUTH).unwrap().is_empty());
    }

    #[test]
    fn only_yaml_files_are_entries_and_they_come_back_sorted() {
        let dir = temp_dir("listing");
        for name in ["b.yaml", "a.yml", "notes.md"] {
            std::fs::write(dir.join(MODELS).join(name), "name: x\n").unwrap();
        }
        std::fs::create_dir(dir.join(MODELS).join("old")).unwrap();

        let paths = entries(&dir, MODELS).unwrap();

        let names: Vec<_> = paths
            .iter()
            .map(|path| path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["a.yml", "b.yaml"]);
    }

    /// A file shipped entirely commented out is how an example lives next to the
    /// thing it is an example of. Uncomment it and it is a declaration.
    #[test]
    fn a_document_that_declares_nothing_is_skipped_rather_than_rejected() {
        let dir = temp_dir("empty");
        let path = dir.join(MODELS).join("example.yaml");
        std::fs::write(
            &path,
            "# - name: files\n#   url: https://mcp.internal/mcp\n",
        )
        .unwrap();

        assert!(read::<Entry>(&path).unwrap().is_none());
    }

    #[test]
    fn a_syntax_error_carries_its_position() {
        let dir = temp_dir("syntax");
        let path = dir.join(MODELS).join("broken.yaml");
        std::fs::write(&path, "name: [unclosed\n").unwrap();

        let issue = read::<Entry>(&path).unwrap_err();

        assert!(issue.line.is_some(), "{issue}");
    }
}
