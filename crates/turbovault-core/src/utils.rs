//! Shared utilities for operations across turbovault crates.
//!
//! Provides DRY helpers for:
//! - Serialization with consistent error handling
//! - Result/report builders
//! - Path validation
//! - Transaction tracking

use crate::{Error, Result};
use std::path::{Path, PathBuf};

/// Render `path` as a `/`-separated string, the spelling Obsidian vault paths
/// (and this server's MCP surface) always use.
///
/// Only touches [`std::path::MAIN_SEPARATOR`], and only when it is not
/// already `/`. On Unix that separator IS `/`, so this is a no-op, which
/// matters because a backslash is a legal filename character there; an
/// unconditional `.replace('\\', "/")` would corrupt a component that
/// legitimately contains one. On Windows it rewrites the `\` the platform
/// APIs return so a path looks identical regardless of which OS produced it.
///
/// A hand-rolled helper rather than the `path-slash` crate: the conversion
/// is exactly this one conditional replace, small enough that a dependency
/// (plus its `Path`/`PathBuf` extension-trait surface we would not use) buys
/// nothing a doc comment doesn't already cover.
pub fn path_to_slash(path: &Path) -> String {
    let rendered = path.to_string_lossy();
    if std::path::MAIN_SEPARATOR == '/' {
        rendered.into_owned()
    } else {
        rendered.replace(std::path::MAIN_SEPARATOR, "/")
    }
}

/// Whether `path` is a *note*: what the note cache, the link graph and the
/// search index model. Markdown, by extension, in any case: `Note.MD` is as
/// much a note as `Note.md`.
///
/// Every consumer has to use this one test. The freshness sweep compares what
/// the cache holds against what a scan finds, and the parser decides whether
/// to read a file's links, so two tests that disagree about a path leave it
/// cached but unparsed, or reported changed on every pass.
///
/// ```
/// use std::path::Path;
/// use turbovault_core::is_note;
///
/// assert!(is_note(Path::new("wiki/Note.MD")));
/// assert!(!is_note(Path::new("board.canvas")));
/// ```
pub fn is_note(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
}

/// The content hash TurboVault hands out for a note: lowercase hex SHA-256 of
/// its Unicode NFC form. It is the `expected_hash` a Direct-backed write
/// checks, the hash `read_note` reports, the before/after hash on an audit
/// entry, and `FileMetadata::checksum`, so any two of them can be compared.
///
/// Normalizing first means the same text typed on two platforms (macOS input
/// often produces decomposed accents) hashes the same. Text that is already
/// NFC, which is nearly all of it, is hashed without being copied.
pub fn compute_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    use unicode_normalization::{IsNormalized, UnicodeNormalization, is_nfc_quick};

    let digest = if is_nfc_quick(content.chars()) == IsNormalized::Yes {
        Sha256::digest(content.as_bytes())
    } else {
        let normalized: String = content.nfc().collect();
        Sha256::digest(normalized.as_bytes())
    };
    bytes_to_lower_hex(digest)
}

/// [`compute_hash`] for bytes that may not be text: the same hash when they
/// are UTF-8, and a plain SHA-256 of the bytes when they are not (an
/// attachment).
pub fn compute_hash_bytes(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};

    match std::str::from_utf8(bytes) {
        Ok(text) => compute_hash(text),
        Err(_) => bytes_to_lower_hex(Sha256::digest(bytes)),
    }
}

/// Encode bytes as lowercase hexadecimal.
pub fn bytes_to_lower_hex(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let bytes = bytes.as_ref();
    let mut out = String::with_capacity(bytes.len() * 2);

    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }

    out
}

/// Generic JSON serialization with consistent error handling
/// Works with any type that implements Serialize (including slices)
pub fn to_json_string<T: serde::Serialize + ?Sized>(data: &T, context: &str) -> Result<String> {
    serde_json::to_string_pretty(data)
        .map_err(|e| Error::other(format!("Failed to serialize {} as JSON: {}", context, e)))
}

/// A path inside a vault, checked against where it lands on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPath {
    /// The path as addressed: the vault root joined with the request, with
    /// `.` and `..` resolved. This is the path to open and to key caches by.
    pub path: PathBuf,
    /// Where that path really is, relative to the vault root's real location,
    /// once every symlink along it has been followed. Empty for the root.
    pub real_relative: PathBuf,
}

/// Path validation helpers
pub struct PathValidator;

impl PathValidator {
    /// Resolve `path` against `vault_root`, refusing it unless it stays inside
    /// the vault both as written and as the filesystem will resolve it.
    ///
    /// Checking the text alone is not enough, because the text is not what
    /// gets opened. A symlink inside the vault can point anywhere, and a note
    /// that does not exist yet cannot be canonicalized the ordinary way, which
    /// is exactly when the check used to fall back to the text and let a new
    /// note be written through a link to anywhere on the machine. The real
    /// location is found by following every link along the path as far as it
    /// exists, including a dangling one at the end, which writing through
    /// would create.
    ///
    /// The resolved path is still the one as addressed, not the real one, so a
    /// vault registered through a symlinked directory keeps its own paths.
    pub fn resolve_in_vault(vault_root: &Path, path: &Path) -> Result<ResolvedPath> {
        Self::resolve_in_vault_at(vault_root, &Self::real_root(vault_root)?, path)
    }

    /// Where `vault_root` really is, every symlink to it followed: what
    /// [`Self::resolve_in_vault_at`] takes as `real_root`.
    pub fn real_root(vault_root: &Path) -> Result<PathBuf> {
        Ok(soft_canonicalize::soft_canonicalize(lexically_normalize(
            vault_root,
        ))?)
    }

    /// [`Self::resolve_in_vault`] for a caller that has already resolved where
    /// the vault root really is (`real_root`, every symlink to it followed),
    /// which costs a walk of the root's own path on each call.
    pub fn resolve_in_vault_at(
        vault_root: &Path,
        real_root: &Path,
        path: &Path,
    ) -> Result<ResolvedPath> {
        let root = lexically_normalize(vault_root);
        let addressed = lexically_normalize(&root.join(path));
        if !addressed.starts_with(&root) {
            return Err(Error::path_traversal(addressed));
        }

        // A path the filesystem cannot resolve (a symlink loop, a directory
        // that cannot be read) cannot be shown to stay inside, so it does not.
        let real = soft_canonicalize::soft_canonicalize(&addressed)
            .map_err(|_| Error::path_traversal(&addressed))?;
        let Ok(real_relative) = real.strip_prefix(real_root) else {
            return Err(Error::path_traversal(addressed));
        };

        Ok(ResolvedPath {
            real_relative: real_relative.to_path_buf(),
            path: addressed,
        })
    }

    /// Ensure a path is within a vault root (prevents directory traversal).
    ///
    /// See [`Self::resolve_in_vault`], which this is the path half of.
    pub fn validate_path_in_vault(vault_root: &Path, path: &Path) -> Result<PathBuf> {
        Ok(Self::resolve_in_vault(vault_root, path)?.path)
    }
}

/// Resolve `.` and `..` in a path by its text alone, without touching the
/// filesystem. `..` at the root stays at the root.
fn lexically_normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    None | Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.pop();
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct TestData {
        name: String,
        value: i32,
    }

    #[test]
    fn test_to_json_string() {
        let data = TestData {
            name: "test".to_string(),
            value: 42,
        };
        let json = to_json_string(&data, "test_data").unwrap();
        assert!(json.contains("test"));
        assert!(json.contains("42"));
    }

    #[test]
    fn test_bytes_to_lower_hex() {
        assert_eq!(bytes_to_lower_hex([0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
    }

    #[test]
    fn path_to_slash_leaves_an_already_slashed_path_untouched() {
        assert_eq!(path_to_slash(Path::new("a/b")), "a/b");
    }

    #[cfg(not(windows))]
    #[test]
    fn path_to_slash_preserves_a_literal_backslash_in_a_unix_filename() {
        // A backslash is a legal filename character on Unix. MAIN_SEPARATOR
        // there is '/', so the conditional replace must be a no-op and leave
        // this real filename intact rather than corrupt it.
        assert_eq!(path_to_slash(Path::new(r"weird\name.md")), r"weird\name.md");
    }

    #[cfg(windows)]
    #[test]
    fn path_to_slash_converts_backslashes_on_windows() {
        assert_eq!(path_to_slash(Path::new(r"islands\a.md")), "islands/a.md");
        assert_eq!(
            path_to_slash(Path::new(r"C:\vault\guides\authentication.md")),
            "C:/vault/guides/authentication.md"
        );
    }

    #[cfg(windows)]
    #[test]
    fn path_to_slash_leaves_an_already_slashed_windows_path_untouched() {
        assert_eq!(path_to_slash(Path::new("a/b")), "a/b");
    }

    #[test]
    fn test_path_validator_valid() {
        let vault_root = PathBuf::from("/vault");
        let path = Path::new("notes/file.md");
        let result = PathValidator::validate_path_in_vault(&vault_root, path);
        assert!(result.is_ok());
    }

    #[test]
    fn test_path_validator_traversal() {
        let vault_root = PathBuf::from("/vault");
        let path = Path::new("../../../etc/passwd");
        let result = PathValidator::validate_path_in_vault(&vault_root, path);
        assert!(result.is_err());
    }
}
