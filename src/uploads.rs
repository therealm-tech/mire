//! Files handed over by the browser, and where they land.
//!
//! This is the one place in `mire` that writes to disk, and it is worth saying
//! why that is unusual here. Everything else is read-only by construction:
//! profiles are read, credentials are held in memory, and the container image
//! runs `--read-only` as UID 65532 with nothing to write to. An upload directory
//! is a deliberate hole in that, so the rules below are not decoration.
//!
//! * **The client never chooses a path.** What arrives is a display name, and it
//!   is treated as one: last segment only, non-portable characters replaced,
//!   leading dots stripped. `../../.ssh/authorized_keys` is a file called
//!   `authorized_keys`, in the upload directory, like everything else.
//! * **Nothing is ever overwritten.** Every file gets a random prefix, so two
//!   uploads of `report.pdf` are two files and neither is the other's problem.
//! * **The size cap is enforced here**, not only by the body limit on the route.
//!   A cap that lives in the router is a cap somebody removes by adding a second
//!   route.
//! * **The directory is created on first write**, not at startup. `mire` still
//!   starts on a read-only filesystem, and only fails when something is actually
//!   attached — which is the moment where the error means something.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64URL;
use minijinja::value::{Value as Rendered, ValueKind};
use serde::Serialize;
use tracing::{debug, info};

/// Largest file accepted, in bytes.
///
/// Not a knob. It is a bound on how much a single request can make `mire` hold
/// in memory and write to somebody's disk, and a tool that sends a known signal
/// to an endpoint has no business accepting a Blu-ray image.
pub const MAX_BYTES: usize = 25 * 1024 * 1024;

/// Longest stored name, before the random prefix.
///
/// Filesystems cap a name around 255 bytes; sanitising leaves ASCII only, so
/// bytes and characters are the same thing by the time this applies.
const MAX_NAME_LEN: usize = 120;

/// The name given to something that sanitises down to nothing at all.
const FALLBACK_NAME: &str = "file";

/// One file, as stored.
#[derive(Debug, Clone)]
pub struct StoredFile {
    /// The random prefix, on its own. The handle a caller keeps.
    pub id: String,
    /// The name the browser sent, untouched. For display, never for a path.
    pub original_name: String,
    /// What it is actually called on disk, prefix included.
    pub stored_name: String,
    /// Size in bytes, as written.
    pub size: u64,
    /// Content type the browser claimed, if it claimed one. Unverified — it is
    /// the client's word, and nothing here acts on it.
    pub content_type: Option<String>,
    /// Where it landed.
    pub path: PathBuf,
}

/// One stored file, as a template sees it.
///
/// Read back off the disk rather than remembered, which is the same choice made
/// everywhere else here: `mire` keeps no session, so a file attached before a
/// restart is still a file, and the process holds no index that could disagree
/// with the directory.
///
/// Two consequences worth knowing, both of them the price of that:
///
/// * **`name` is the sanitised name**, not necessarily what the browser called
///   it. The original travels in the answer to `POST /api/uploads` and is not
///   written anywhere, so it is gone by the time a template asks.
/// * **`contentType` is guessed from the extension.** What the browser claimed
///   was its own word about its own file and was never stored either. See
///   [`content_type_for`].
///
/// `camelCase`, like everything else `mire` puts on a wire, so the field names in
/// a template are the ones `POST /api/uploads` answered with.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadRef {
    /// The handle the caller asked for.
    pub id: String,
    /// File name without the prefix. Sanitised; see above.
    pub name: String,
    /// The full name on disk, prefix included.
    pub stored_as: String,
    /// Where it is, for a template that only wants to say so.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// Media type, guessed from the extension. `null` when the extension says
    /// nothing — which is a template's cue to decide for itself.
    pub content_type: Option<String>,
    /// The whole file, standard base64. What a multimodal request body wants.
    pub base64: String,
    /// The same bytes as `data:<type>;base64,…`, which is the shape most vision
    /// endpoints read. Falls back to `application/octet-stream` when the
    /// extension gave nothing away.
    pub data_url: String,
    /// The file decoded as UTF-8, when it decodes. `null` otherwise, so
    /// `{% if upload.text %}` is the test for "is this readable text".
    pub text: Option<String>,
}

/// The upload directory.
#[derive(Debug, Clone)]
pub struct UploadStore {
    dir: PathBuf,
}

impl UploadStore {
    /// Points a store at a directory. Creates nothing; see the module docs.
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The directory files are written to.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Writes `bytes` under a name derived from `original_name`.
    ///
    /// The whole body is held in memory before anything is written, which is
    /// what [`MAX_BYTES`] bounds. That is on purpose: streaming to the file
    /// would mean deciding what to do with the half-written one when the cap is
    /// hit mid-flight, and a partial file that looks like an upload is worse
    /// than a bounded buffer.
    ///
    /// # Errors
    ///
    /// [`UploadError::TooLarge`] past the cap, [`UploadError::Io`] when the
    /// directory or the file cannot be written.
    pub async fn store(
        &self,
        original_name: &str,
        content_type: Option<String>,
        bytes: Vec<u8>,
    ) -> Result<StoredFile, UploadError> {
        if bytes.len() > MAX_BYTES {
            return Err(UploadError::TooLarge {
                size: bytes.len(),
                limit: MAX_BYTES,
            });
        }

        let id = random_id();
        let stored_name = format!("{id}-{}", sanitise(original_name));
        let path = self.dir.join(&stored_name);

        tokio::fs::create_dir_all(&self.dir)
            .await
            .map_err(|source| UploadError::Io {
                path: self.dir.display().to_string(),
                source,
            })?;

        let size = bytes.len() as u64;
        debug!(%stored_name, size, dir = %self.dir.display(), "writing an upload");
        tokio::fs::write(&path, &bytes)
            .await
            .map_err(|source| UploadError::Io {
                path: path.display().to_string(),
                source,
            })?;

        info!(%stored_name, size, path = %path.display(), "upload stored");
        Ok(StoredFile {
            id,
            original_name: original_name.to_owned(),
            stored_name,
            size,
            content_type,
            path,
        })
    }

    /// Reads a stored file back, ready for a template.
    ///
    /// Looked up by **id**, never by name. That is the whole reason the id
    /// exists: a caller naming a file would be a caller choosing a path again,
    /// and every rule [`sanitise`] enforces on the way in would have to be
    /// enforced a second time on the way out. An id is checked against one
    /// character class and then matched against what is in the directory.
    ///
    /// The directory is scanned rather than indexed, because an index would be
    /// state, and state does not survive the restart that the file does.
    ///
    /// # Errors
    ///
    /// [`UploadError::InvalidId`] for something that is not an id at all,
    /// [`UploadError::UnknownUpload`] when nothing in the directory carries it,
    /// and [`UploadError::Io`] when the directory or the file cannot be read.
    pub async fn load(&self, id: &str) -> Result<UploadRef, UploadError> {
        if !is_id(id) {
            return Err(UploadError::InvalidId { id: id.to_owned() });
        }

        let stored_name = self.find(id).await?;
        let path = self.dir.join(&stored_name);
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|source| UploadError::Io {
                path: path.display().to_string(),
                source,
            })?;

        // The prefix and its separator, which `find` has already established are
        // there. What is left is the name the file was stored under.
        let name = stored_name
            .get(id.len() + 1..)
            .unwrap_or(FALLBACK_NAME)
            .to_owned();
        let content_type = content_type_for(&name);
        let base64 = BASE64.encode(&bytes);
        let data_url = format!(
            "data:{};base64,{base64}",
            content_type
                .as_deref()
                .unwrap_or("application/octet-stream")
        );

        debug!(%id, %stored_name, bytes = bytes.len(), "upload read back for a template");
        Ok(UploadRef {
            id: id.to_owned(),
            name,
            stored_as: stored_name,
            path: path.display().to_string(),
            size: bytes.len() as u64,
            content_type,
            base64,
            data_url,
            // Consumes `bytes`, so a text file is held once rather than twice.
            // A binary fails on its first invalid byte, which costs nothing.
            text: String::from_utf8(bytes).ok(),
        })
    }

    /// The name in the directory carrying this id, if any.
    async fn find(&self, id: &str) -> Result<String, UploadError> {
        let prefix = format!("{id}-");
        let mut entries =
            tokio::fs::read_dir(&self.dir)
                .await
                .map_err(|source| UploadError::Io {
                    path: self.dir.display().to_string(),
                    source,
                })?;

        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|source| UploadError::Io {
                path: self.dir.display().to_string(),
                source,
            })?
        {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) {
                return Ok(name);
            }
        }

        Err(UploadError::UnknownUpload { id: id.to_owned() })
    }
}

/// Whether this could be one of ours.
///
/// The check that makes [`UploadStore::load`] safe: no separator, no dot, no
/// anything that means something to a path. `Path::join` on what passes this can
/// only ever land inside the directory.
fn is_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 32
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

/// Media type for a file name, from its extension.
///
/// A guess, and deliberately a short list: what the browser said was never
/// written down, and the alternative to guessing is a sidecar metadata file next
/// to every upload — which would make the directory something other than the
/// plain directory of files it is meant to be.
///
/// The list covers what actually gets attached to a model request. Anything else
/// answers `None`, and a template that knows better writes the type itself.
fn content_type_for(name: &str) -> Option<String> {
    let extension = name.rsplit_once('.')?.1.to_ascii_lowercase();
    let media = match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "csv" => "text/csv",
        "md" => "text/markdown",
        "txt" | "log" => "text/plain",
        "yaml" | "yml" => "application/yaml",
        // Audio, because a transcriber is the whole reason a form body exists
        // here and every one of them reads the part's type. An endpoint handed
        // `application/octet-stream` for an MP3 is one that either guesses or
        // refuses, and both are worse than saying so. A profile that disagrees
        // still overrides it with `type:`.
        "mp3" | "mpga" => "audio/mpeg",
        "wav" => "audio/wav",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "webm" => "audio/webm",
        // A video type, because that is what it is — a transcriber that accepts
        // a recording in this container accepts it under this name.
        "mp4" => "video/mp4",
        _ => return None,
    };
    Some(media.to_owned())
}

/// Turns whatever the browser called a file into something safe to join onto a
/// directory.
///
/// Not an escaping scheme and not reversible — the original name travels in the
/// response, for display. This exists so that the result is one path segment, on
/// every platform, whatever arrived.
fn sanitise(name: &str) -> String {
    // Both separators, always: a Windows browser sends backslashes and a Unix
    // server does not treat them as separators, which is exactly how a name with
    // one in it ends up looking like a directory later on.
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();

    let cleaned: String = base
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
                character
            } else {
                '_'
            }
        })
        .collect();

    // Leading dots make a hidden file, and `..` is the reason this function
    // exists. Trailing ones are harmless but confuse enough tools to be worth
    // dropping in the same breath.
    let trimmed = cleaned.trim_matches('.');
    if trimmed.is_empty() {
        return FALLBACK_NAME.to_owned();
    }

    // Only ASCII survives the mapping above, so this cannot split a character.
    let capped = &trimmed[..trimmed.len().min(MAX_NAME_LEN)];
    capped.to_owned()
}

/// 9 random bytes, base64url: 12 characters, filename-safe by construction.
///
/// Long enough that two uploads never collide, which is the only thing asked of
/// it. Unguessable is a bonus rather than a security boundary — the file sits in
/// a directory on the machine that uploaded it.
fn random_id() -> String {
    use rand::Rng;

    let mut bytes = [0_u8; 9];
    rand::rng().fill_bytes(&mut bytes);
    BASE64URL.encode(bytes)
}

/// Why a file could not be stored.
#[derive(Debug, thiserror::Error)]
pub enum UploadError {
    /// The request carried no file part at all.
    #[error("the request carries no file")]
    NoFile,

    /// Past [`MAX_BYTES`].
    #[error("the file is {size} bytes, the limit is {limit}")]
    TooLarge {
        /// What arrived.
        size: usize,
        /// What is allowed.
        limit: usize,
    },

    /// The multipart body could not be read.
    #[error("the upload could not be read: {0}")]
    Malformed(String),

    /// Something was passed as an id that could not be one.
    #[error("`{id}` is not an upload id")]
    InvalidId {
        /// What was asked for.
        id: String,
    },

    /// No file in the directory carries that id.
    #[error("no upload `{id}` — it was never stored here, or the directory was emptied")]
    UnknownUpload {
        /// What was asked for.
        id: String,
    },

    /// The directory or the file could not be written.
    #[error("cannot write `{path}`: {source}")]
    Io {
        /// What was being written.
        path: String,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

/// The media type a file goes out as.
///
/// The extension's guess, made when the file was read back off the disk.
/// `application/octet-stream` when the extension gave nothing away, which is
/// what a part with no better idea is supposed to say.
#[must_use]
pub fn mime_of(upload: &UploadRef) -> &str {
    upload
        .content_type
        .as_deref()
        .unwrap_or("application/octet-stream")
}

/// The uploads one rendered template value names.
///
/// Three forms, because a template can sensibly produce three things: an upload
/// whole (`{{ uploads[0] }}`), a list of them (`{{ uploads }}`), or a string
/// naming one — its `path`, its `name` or its `id`. Anything else is an error
/// rather than a file quietly left out of a form.
///
/// Shared by a hook's `multipart:` and a profile's, so the two cannot end up
/// with different answers to "which file did you mean". In a tool whose whole
/// pitch is that what went out is written down, that is the divergence least
/// worth having.
///
/// `field` is only ever read back in an error message, naming the form field
/// that asked.
///
/// # Errors
///
/// Fails when the value is not a file, a list of them, or the name of one — and
/// when it names a file this run is not carrying.
pub fn resolve<'a>(
    value: &Rendered,
    uploads: &'a [UploadRef],
    field: &str,
) -> Result<Vec<&'a UploadRef>, String> {
    if let Some(text) = value.as_str() {
        return named(text, uploads, field).map(|upload| vec![upload]);
    }

    if value.kind() == ValueKind::Seq {
        let items = value
            .try_iter()
            .map_err(|error| format!("`{field}`: {error}"))?;
        let mut found = Vec::new();
        for item in items {
            found.extend(resolve(&item, uploads, field)?);
        }
        return Ok(found);
    }

    // An upload as the context carries it. Any of the three identifying fields
    // will do, and `path` is the one a file is most likely to have written.
    for attribute in ["path", "id", "name"] {
        if let Ok(inner) = value.get_attr(attribute)
            && let Some(text) = inner.as_str()
        {
            return named(text, uploads, field).map(|upload| vec![upload]);
        }
    }

    Err(format!(
        "`{field}`: that is not a file, and not the name of one"
    ))
}

/// The upload a path, name or id points at.
fn named<'a>(text: &str, uploads: &'a [UploadRef], field: &str) -> Result<&'a UploadRef, String> {
    uploads
        .iter()
        .find(|upload| upload.path == text || upload.name == text || upload.id == text)
        .ok_or_else(|| {
            format!(
                "`{field}`: `{text}` is not a file this run is carrying ({})",
                carrying(uploads)
            )
        })
}

/// What the run has to offer, for the error saying it had none of it.
#[must_use]
pub fn carrying(uploads: &[UploadRef]) -> String {
    if uploads.is_empty() {
        return "nothing was attached to this run".to_owned();
    }
    let names: Vec<&str> = uploads.iter().map(|upload| upload.name.as_str()).collect();
    format!("it is carrying {}", names.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_traversal_attempt_becomes_one_boring_file_name() {
        // The whole point: what comes back joins onto a directory without
        // reaching anything above it.
        assert_eq!(sanitise("../../.ssh/authorized_keys"), "authorized_keys");
        assert_eq!(sanitise("/etc/passwd"), "passwd");
        assert_eq!(sanitise(".."), FALLBACK_NAME);
        assert_eq!(sanitise("/"), FALLBACK_NAME);
    }

    /// A Windows browser sends `C:\Users\…\report.pdf`, and a Unix server does
    /// not read backslashes as separators — so without this the "name" keeps the
    /// whole path in it.
    #[test]
    fn a_windows_path_is_reduced_to_its_last_segment() {
        assert_eq!(sanitise(r"C:\Users\gleroy\report.pdf"), "report.pdf");
    }

    #[test]
    fn a_hidden_file_stops_being_hidden() {
        assert_eq!(sanitise(".env"), "env");
        assert_eq!(sanitise(".gitignore"), "gitignore");
    }

    #[test]
    fn the_ordinary_case_is_left_alone() {
        assert_eq!(sanitise("report.pdf"), "report.pdf");
        assert_eq!(sanitise("archive.tar.gz"), "archive.tar.gz");
        assert_eq!(sanitise("signal-1.png"), "signal-1.png");
    }

    /// Anything outside the safe set becomes `_` rather than disappearing, so
    /// two different names cannot quietly sanitise to the same one.
    #[test]
    fn awkward_characters_are_replaced_rather_than_dropped() {
        assert_eq!(sanitise("rapport d'été.pdf"), "rapport_d__t_.pdf");
        assert_eq!(sanitise("a;rm -rf b.txt"), "a_rm_-rf_b.txt");
    }

    #[test]
    fn a_very_long_name_is_capped() {
        let name = format!("{}.pdf", "a".repeat(400));
        assert_eq!(sanitise(&name).len(), MAX_NAME_LEN);
    }

    #[tokio::test]
    async fn a_file_lands_in_the_directory_under_a_prefixed_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = UploadStore::new(dir.path());

        let stored = store
            .store(
                "report.pdf",
                Some("application/pdf".to_owned()),
                b"hi".to_vec(),
            )
            .await
            .unwrap();

        assert_eq!(stored.original_name, "report.pdf");
        assert!(stored.stored_name.ends_with("-report.pdf"));
        assert_eq!(stored.size, 2);
        assert_eq!(std::fs::read(&stored.path).unwrap(), b"hi");
        assert_eq!(stored.path.parent().unwrap(), dir.path());
    }

    /// Two people testing the same endpoint attach the same `payload.json`. That
    /// is two files, not one file and a surprise.
    #[tokio::test]
    async fn the_same_name_twice_is_two_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = UploadStore::new(dir.path());

        let first = store.store("a.txt", None, b"one".to_vec()).await.unwrap();
        let second = store.store("a.txt", None, b"two".to_vec()).await.unwrap();

        assert_ne!(first.stored_name, second.stored_name);
        assert_eq!(std::fs::read(&first.path).unwrap(), b"one");
        assert_eq!(std::fs::read(&second.path).unwrap(), b"two");
    }

    /// The directory is not created until something is actually attached, so a
    /// read-only filesystem is only a problem for someone who uploads.
    #[tokio::test]
    async fn the_directory_appears_on_the_first_write_and_not_before() {
        let parent = tempfile::TempDir::new().unwrap();
        let dir = parent.path().join("uploads");
        let store = UploadStore::new(&dir);
        assert!(!dir.exists());

        store.store("a.txt", None, b"one".to_vec()).await.unwrap();

        assert!(dir.is_dir());
    }

    #[tokio::test]
    async fn a_stored_file_is_read_back_by_its_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = UploadStore::new(dir.path());
        let stored = store
            .store("notes.txt", Some("text/plain".to_owned()), b"ping".to_vec())
            .await
            .unwrap();

        let loaded = store.load(&stored.id).await.unwrap();

        assert_eq!(loaded.id, stored.id);
        assert_eq!(loaded.name, "notes.txt");
        assert_eq!(loaded.stored_as, stored.stored_name);
        assert_eq!(loaded.size, 4);
        assert_eq!(loaded.base64, "cGluZw==");
        assert_eq!(loaded.text.as_deref(), Some("ping"));
        assert_eq!(loaded.content_type.as_deref(), Some("text/plain"));
        assert_eq!(loaded.data_url, "data:text/plain;base64,cGluZw==");
    }

    /// The load path never sees a name, so this is the belt to
    /// [`sanitise`]'s braces: anything shaped like a path is refused before it
    /// can be joined onto the directory.
    #[tokio::test]
    async fn nothing_shaped_like_a_path_is_accepted_as_an_id() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = UploadStore::new(dir.path());

        for attempt in [
            "../../etc/passwd",
            "..",
            "a/b",
            r"a\b",
            ".hidden",
            "",
            "with space",
        ] {
            assert!(
                matches!(
                    store.load(attempt).await,
                    Err(UploadError::InvalidId { .. })
                ),
                "`{attempt}` was not refused"
            );
        }
    }

    /// An id that *could* be one but names nothing. A `404`, never a silent
    /// empty file: a call that quietly dropped an attachment would send a body
    /// missing a file and say nothing about it.
    #[tokio::test]
    async fn an_id_that_matches_nothing_is_an_error_rather_than_an_empty_file() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = UploadStore::new(dir.path());
        store.store("a.txt", None, b"one".to_vec()).await.unwrap();

        let error = store.load("aaaaaaaaaaaa").await.unwrap_err();

        assert!(matches!(error, UploadError::UnknownUpload { .. }));
    }

    /// Binary is not text, and saying so is the point: `text` is what a template
    /// tests to decide between inlining a file and base64-ing it.
    #[tokio::test]
    async fn a_file_that_is_not_utf8_has_no_text() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = UploadStore::new(dir.path());
        // A PNG header, which is both not UTF-8 and the realistic case.
        let stored = store
            .store("shot.png", None, vec![0x89, b'P', b'N', b'G', 0xff, 0xfe])
            .await
            .unwrap();

        let loaded = store.load(&stored.id).await.unwrap();

        assert!(loaded.text.is_none());
        assert_eq!(loaded.content_type.as_deref(), Some("image/png"));
        assert!(loaded.data_url.starts_with("data:image/png;base64,"));
    }

    /// The type is read off the extension, because what the browser claimed was
    /// never written down. An extension that says nothing says nothing.
    #[test]
    fn the_media_type_is_guessed_from_the_extension() {
        assert_eq!(content_type_for("a.PNG").as_deref(), Some("image/png"));
        assert_eq!(content_type_for("a.jpeg").as_deref(), Some("image/jpeg"));
        assert_eq!(
            content_type_for("payload.json").as_deref(),
            Some("application/json")
        );
        assert_eq!(content_type_for("archive.tar.gz"), None);
        assert_eq!(content_type_for("noextension"), None);
    }

    /// Falls back rather than emitting `data:;base64,…`, which is not a data URL
    /// and which an endpoint would reject with something unhelpful.
    #[tokio::test]
    async fn an_unknown_type_still_produces_a_usable_data_url() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = UploadStore::new(dir.path());
        let stored = store.store("blob", None, b"ping".to_vec()).await.unwrap();

        let loaded = store.load(&stored.id).await.unwrap();

        assert!(loaded.content_type.is_none());
        assert_eq!(
            loaded.data_url,
            "data:application/octet-stream;base64,cGluZw=="
        );
    }

    #[tokio::test]
    async fn past_the_cap_nothing_is_written() {
        let dir = tempfile::TempDir::new().unwrap();
        let store = UploadStore::new(dir.path());

        let error = store
            .store("big.bin", None, vec![0; MAX_BYTES + 1])
            .await
            .unwrap_err();

        assert!(matches!(error, UploadError::TooLarge { .. }));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }
}
