use thiserror::Error;

/// Errors raised by the vector search engine.
#[derive(Debug, Error)]
pub enum VectorError {
    /// The embedding backend failed to load or to embed text.
    #[error("embedding error: {0}")]
    Embedding(String),
    /// The dense (HNSW) or lexical (BM25) index failed a read or write.
    #[error("index error: {0}")]
    Index(String),
    /// A persisted snapshot could not be decoded, or was internally inconsistent.
    #[error("snapshot error: {0}")]
    Snapshot(String),
    /// A configuration value was missing or malformed.
    #[error("config error: {0}")]
    Config(String),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, VectorError>;
