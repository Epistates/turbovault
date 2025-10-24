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

impl Error {
    /// Create an IO error
    pub fn io(err: io::Error) -> Self {
        Error::Io(err)
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
}
