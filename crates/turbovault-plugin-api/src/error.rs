use serde::{Deserialize, Serialize};

/// Stable, machine-readable categories returned across the plugin boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginErrorCode {
    /// A request was malformed or violated a contract precondition.
    InvalidInput,
    /// The requested vault, note, or tool does not exist.
    NotFound,
    /// An optimistic-concurrency precondition failed.
    Conflict,
    /// A required host capability is temporarily unavailable.
    Unavailable,
    /// The host or plugin failed unexpectedly.
    Internal,
}

/// Error type shared by the host and plugins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {message}")]
pub struct PluginError {
    /// Stable error category.
    pub code: PluginErrorCode,
    /// Human-readable detail safe to return to a plugin caller.
    pub message: String,
}

impl PluginError {
    /// Construct an error with a stable code.
    pub fn new(code: PluginErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Construct an invalid-input error.
    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(PluginErrorCode::InvalidInput, message)
    }

    /// Construct a not-found error.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(PluginErrorCode::NotFound, message)
    }

    /// Construct a concurrency-conflict error.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(PluginErrorCode::Conflict, message)
    }

    /// Construct an unavailable error.
    pub fn unavailable(message: impl Into<String>) -> Self {
        Self::new(PluginErrorCode::Unavailable, message)
    }

    /// Construct an internal error.
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(PluginErrorCode::Internal, message)
    }
}

/// Result type used by plugin contracts.
pub type PluginResult<T> = Result<T, PluginError>;
