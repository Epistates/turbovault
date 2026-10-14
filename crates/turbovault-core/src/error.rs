//! Error types for the Obsidian system.
//!
//! All errors in the system are represented by the [`Error`] enum.
//! This ensures composable error handling across crates.

use std::io;
use std::path::{Path, PathBuf};
use thiserror::Error as ThisError;

/// The core error type for all Obsidian operations.
#[derive(ThisError, Debug)]
#[non_exhaustive]
pub enum Error {
    /// File system error with no path to name.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// File system error on a known path. [`Error::io_at`] makes one for
    /// every kind but a missing file, which is [`Error::FileNotFound`].
    #[error("I/O error on {path}: {source}")]
    IoAt {
        /// The path the operation was on.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },

    /// File not found
    #[error("File not found: {path}")]
    FileNotFound {
        /// The path that doesn't exist.
        path: PathBuf,
    },

    /// Invalid file path (outside vault, too long, etc.)
    #[error("Invalid file path: {reason}")]
    InvalidPath {
        /// Why the path is refused.
        reason: String,
    },

    /// Path traversal attempt detected
    #[error("Path traversal detected: {path}")]
    PathTraversalAttempt {
        /// The path that would leave the vault.
        path: PathBuf,
    },

    /// Path lies inside the vault but in a directory the note APIs may not
    /// touch (TurboVault's own state, `.git`, `.obsidian`, ...).
    #[error(
        "Protected path: {path} lies under '{component}', which the note APIs may not read or write"
    )]
    ProtectedPath {
        /// The path asked for.
        path: PathBuf,
        /// The protected directory it lies under.
        component: String,
    },

    /// File too large for processing
    #[error("File too large ({size} bytes, max {max} bytes): {path}")]
    FileTooLarge {
        /// The file.
        path: PathBuf,
        /// Its size in bytes.
        size: u64,
        /// The limit in bytes.
        max: u64,
    },

    /// Content that doesn't parse, such as malformed frontmatter.
    #[error("Parse error: {reason}")]
    ParseError {
        /// What failed to parse, and why.
        reason: String,
    },

    /// The server or a vault is not set up to do what was asked.
    #[error("Configuration error: {reason}")]
    ConfigError {
        /// What is misconfigured.
        reason: String,
    },

    /// A request whose arguments are invalid.
    #[error("Validation error: {reason}")]
    ValidationError {
        /// What is wrong with them.
        reason: String,
    },

    /// The target changed since the caller last saw it.
    #[error("Concurrent access conflict: {reason}")]
    ConcurrencyError {
        /// What changed.
        reason: String,
    },

    /// Not found in graph
    #[error("Not found in graph: {key}")]
    NotFound {
        /// What was looked up.
        key: String,
    },

    /// A change the vault's `allowed_operations` setting doesn't permit.
    #[error("{reason}")]
    OperationNotAllowed {
        /// Which operation, on which vault.
        reason: String,
    },

    /// A failure inside the server that fits no other variant.
    #[error("Error: {0}")]
    Other(String),
}

/// Convenient Result type alias
pub type Result<T> = std::result::Result<T, Error>;

/// What kind of failure an [`Error`] is, from the caller's side: whether
/// asking again differently can succeed, and how. Each protocol TurboVault
/// speaks (MCP tool results, the plugin API) maps these to its own codes, so
/// a missing note reads as "not found" everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ErrorCategory {
    /// The note, file or graph entry asked for does not exist.
    NotFound,
    /// The request itself is malformed: a bad path, an invalid value, input
    /// that does not parse.
    InvalidInput,
    /// The request is well formed but policy refuses it, such as a path in a
    /// protected directory.
    PermissionDenied,
    /// The target changed since the caller last saw it.
    Conflict,
    /// The server is not set up to answer it, such as no active vault.
    Unavailable,
    /// Something failed inside the server.
    Internal,
}

impl Error {
    /// The [`ErrorCategory`] this error belongs to.
    pub fn category(&self) -> ErrorCategory {
        match self {
            Error::FileNotFound { .. } | Error::NotFound { .. } => ErrorCategory::NotFound,
            Error::Io(io_error) if io_error.kind() == io::ErrorKind::NotFound => {
                ErrorCategory::NotFound
            }
            // A protected path is a policy refusal, not a malformed request:
            // the path is well formed and may well exist.
            Error::ProtectedPath { .. } | Error::OperationNotAllowed { .. } => {
                ErrorCategory::PermissionDenied
            }
            Error::InvalidPath { .. }
            | Error::PathTraversalAttempt { .. }
            | Error::FileTooLarge { .. }
            | Error::ParseError { .. }
            | Error::ValidationError { .. } => ErrorCategory::InvalidInput,
            Error::ConcurrencyError { .. } => ErrorCategory::Conflict,
            Error::ConfigError { .. } => ErrorCategory::Unavailable,
            Error::Io(_) | Error::IoAt { .. } | Error::Other(_) => ErrorCategory::Internal,
        }
    }

    /// The path this error is about, if it names one.
    pub fn path(&self) -> Option<&Path> {
        match self {
            Error::FileNotFound { path }
            | Error::PathTraversalAttempt { path }
            | Error::ProtectedPath { path, .. }
            | Error::FileTooLarge { path, .. }
            | Error::IoAt { path, .. } => Some(path),
            _ => None,
        }
    }

    /// Create an IO error
    pub fn io(err: io::Error) -> Self {
        Error::Io(err)
    }

    /// Create an IO error for an operation on `path`, translating a missing
    /// file into [`Error::file_not_found`] instead of wrapping the raw I/O
    /// error.
    ///
    /// `io::Error`'s `Display` text for `ErrorKind::NotFound` is the OS's own
    /// message ("No such file or directory (os error 2)" on Unix, "The
    /// system cannot find the file specified. (os error 2)" on Windows).
    /// That's fine for a log, not for a client that has to recognize "missing
    /// file" from the text. Every other `ErrorKind` becomes [`Error::IoAt`],
    /// which keeps the path. `path` should already be vault-relative and
    /// `/`-separated (see [`crate::path_to_slash`]) so the message matches
    /// every other path this server returns.
    pub fn io_at(path: impl Into<PathBuf>, err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::NotFound {
            Error::file_not_found(path)
        } else {
            Error::IoAt {
                path: path.into(),
                source: err,
            }
        }
    }

    /// Create a file not found error
    pub fn file_not_found(path: impl Into<PathBuf>) -> Self {
        Error::FileNotFound { path: path.into() }
    }

    /// Create an invalid path error
    pub fn invalid_path(reason: impl Into<String>) -> Self {
        Error::InvalidPath {
            reason: reason.into(),
        }
    }

    /// Create a path traversal error
    pub fn path_traversal(path: impl Into<PathBuf>) -> Self {
        Error::PathTraversalAttempt { path: path.into() }
    }

    /// Create a protected-path error
    pub fn protected_path(path: impl Into<PathBuf>, component: impl Into<String>) -> Self {
        Error::ProtectedPath {
            path: path.into(),
            component: component.into(),
        }
    }

    /// Create a file too large error
    pub fn file_too_large(path: impl Into<PathBuf>, size: u64, max: u64) -> Self {
        Error::FileTooLarge {
            path: path.into(),
            size,
            max,
        }
    }

    /// Create a parse error
    pub fn parse_error(reason: impl Into<String>) -> Self {
        Error::ParseError {
            reason: reason.into(),
        }
    }

    /// Create a configuration error: the server or a vault is not set up to
    /// do what was asked. For bad request arguments use
    /// [`Error::validation_error`]; for failures inside the server,
    /// [`Error::other`].
    pub fn config_error(reason: impl Into<String>) -> Self {
        Error::ConfigError {
            reason: reason.into(),
        }
    }

    /// Create a validation error: the request's arguments are invalid.
    pub fn validation_error(reason: impl Into<String>) -> Self {
        Error::ValidationError {
            reason: reason.into(),
        }
    }

    /// Create a concurrency error
    pub fn concurrency_error(reason: impl Into<String>) -> Self {
        Error::ConcurrencyError {
            reason: reason.into(),
        }
    }

    /// Create a not found error
    pub fn not_found(key: impl Into<String>) -> Self {
        Error::NotFound { key: key.into() }
    }

    /// A change the vault's `allowed_operations` doesn't permit. Its
    /// [`category`](Self::category) is [`ErrorCategory::PermissionDenied`].
    pub fn operation_not_allowed(reason: impl Into<String>) -> Self {
        Error::OperationNotAllowed {
            reason: reason.into(),
        }
    }

    /// Create a generic error
    pub fn other(msg: impl Into<String>) -> Self {
        Error::Other(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let err = Error::file_not_found("/path/to/file");
        assert!(err.to_string().contains("File not found"));

        let err = Error::invalid_path("contains .. traversal");
        assert!(err.to_string().contains("Invalid file path"));
    }

    #[test]
    fn io_at_translates_not_found_into_file_not_found() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "os-specific text");
        let err = Error::io_at("notes/missing.md", io_err);

        assert!(matches!(err, Error::FileNotFound { .. }));
        let message = err.to_string();
        assert!(message.contains("not found"));
        assert!(message.contains("notes/missing.md"));
        assert!(!message.contains("os-specific text"));
    }

    #[test]
    fn io_at_keeps_the_path_for_other_error_kinds() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        let err = Error::io_at("notes/locked.md", io_err);

        assert!(matches!(err, Error::IoAt { .. }));
        assert_eq!(err.path(), Some(Path::new("notes/locked.md")));
        let message = err.to_string();
        assert!(message.contains("denied") && message.contains("notes/locked.md"));
        assert_eq!(err.category(), ErrorCategory::Internal);
    }

    #[test]
    fn a_refused_operation_is_permission_denied() {
        let err = Error::operation_not_allowed("vault 'notes' does not allow write");

        assert_eq!(err.category(), ErrorCategory::PermissionDenied);
        assert!(matches!(err, Error::OperationNotAllowed { .. }));
        assert_eq!(err.to_string(), "vault 'notes' does not allow write");
    }
}
