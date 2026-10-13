//! Error types for the Obsidian system.
//!
//! All errors in the system are represented by the [`Error`] enum.
//! This ensures composable error handling across crates.

use std::io;
use std::path::PathBuf;
use thiserror::Error as ThisError;

/// The core error type for all Obsidian operations.
#[derive(ThisError, Debug)]
pub enum Error {
    /// File system error
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// File not found
    #[error("File not found: {path}")]
    FileNotFound { path: PathBuf },

    /// Invalid file path (outside vault, too long, etc.)
    #[error("Invalid file path: {reason}")]
    InvalidPath { reason: String },

    /// Path traversal attempt detected
    #[error("Path traversal detected: {path}")]
    PathTraversalAttempt { path: PathBuf },

    /// Path lies inside the vault but in a directory the note APIs may not
    /// touch (TurboVault's own state, `.git`, `.obsidian`, ...).
    #[error(
        "Protected path: {path} lies under '{component}', which the note APIs may not read or write"
    )]
    ProtectedPath { path: PathBuf, component: String },

    /// File too large for processing
    #[error("File too large ({size} bytes, max {max} bytes): {path}")]
    FileTooLarge { path: PathBuf, size: u64, max: u64 },

    /// Parse error
    #[error("Parse error: {reason}")]
    ParseError { reason: String },

    /// Invalid configuration
    #[error("Configuration error: {reason}")]
    ConfigError { reason: String },

    /// Validation error
    #[error("Validation error: {reason}")]
    ValidationError { reason: String },

    /// Concurrent access conflict
    #[error("Concurrent access conflict: {reason}")]
    ConcurrencyError { reason: String },

    /// Not found in graph
    #[error("Not found in graph: {key}")]
    NotFound { key: String },

    /// Generic unclassified error
    #[error("Error: {0}")]
    Other(String),

    /// Wrapped error from other crates
    #[error("Wrapped error: {0}")]
    Wrapped(Box<dyn std::error::Error + Send + Sync>),
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
            Error::ProtectedPath { .. } => ErrorCategory::PermissionDenied,
            Error::InvalidPath { .. }
            | Error::PathTraversalAttempt { .. }
            | Error::FileTooLarge { .. }
            | Error::ParseError { .. }
            | Error::ValidationError { .. } => ErrorCategory::InvalidInput,
            Error::ConcurrencyError { .. } => ErrorCategory::Conflict,
            Error::ConfigError { .. } => ErrorCategory::Unavailable,
            Error::Wrapped(inner) if inner.is::<OperationNotAllowed>() => {
                ErrorCategory::PermissionDenied
            }
            Error::Io(_) | Error::Other(_) | Error::Wrapped(_) => ErrorCategory::Internal,
        }
    }

    /// The refusal this error carries if a vault's `allowed_operations`
    /// refused the change, as [`Error::operation_not_allowed`] made it.
    pub fn as_operation_not_allowed(&self) -> Option<&OperationNotAllowed> {
        match self {
            Error::Wrapped(inner) => inner.downcast_ref::<OperationNotAllowed>(),
            _ => None,
        }
    }

    /// A change the vault's `allowed_operations` doesn't permit. Its
    /// [`category`](Self::category) is [`ErrorCategory::PermissionDenied`].
    pub fn operation_not_allowed(reason: impl Into<String>) -> Self {
        Error::Wrapped(Box::new(OperationNotAllowed(reason.into())))
    }
}

/// A change refused by the vault's `allowed_operations` setting. It travels
/// in [`Error::Wrapped`], which keeps the [`Error`] enum unchanged; use
/// [`Error::operation_not_allowed`] to make one and
/// [`Error::as_operation_not_allowed`] to recognize it.
#[derive(Debug, ThisError)]
#[error("{0}")]
pub struct OperationNotAllowed(pub String);

impl Error {
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
    /// That's fine for a log, not for for a client that has to recognize "missing
    /// file" from the text. Every other `ErrorKind` still wraps as
    /// [`Error::io`]. `path` should already be vault-relative and
    /// `/`-separated (see [`crate::path_to_slash`]) so the message matches
    /// every other path this server returns.
    pub fn io_at(path: impl Into<PathBuf>, err: io::Error) -> Self {
        if err.kind() == io::ErrorKind::NotFound {
            Error::file_not_found(path)
        } else {
            Error::Io(err)
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

    /// Create a configuration error
    pub fn config_error(reason: impl Into<String>) -> Self {
        Error::ConfigError {
            reason: reason.into(),
        }
    }

    /// Create a validation error
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
    fn io_at_leaves_other_error_kinds_as_io() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "denied");
        let err = Error::io_at("notes/locked.md", io_err);

        assert!(matches!(err, Error::Io(_)));
        assert!(err.to_string().contains("denied"));
    }

    #[test]
    fn a_refused_operation_is_permission_denied() {
        let err = Error::operation_not_allowed("vault 'notes' does not allow write");

        assert_eq!(err.category(), ErrorCategory::PermissionDenied);
        assert_eq!(
            err.as_operation_not_allowed().map(ToString::to_string),
            Some("vault 'notes' does not allow write".to_string())
        );
        // Any other wrapped error is still a fault in the server.
        let other = Error::Wrapped(Box::new(io::Error::other("boom")));
        assert_eq!(other.category(), ErrorCategory::Internal);
        assert!(other.as_operation_not_allowed().is_none());
    }
}
