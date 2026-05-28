//! Error type for the git write substrate.
//!
//! The crate owns its error domain (git2 / io / invariant violations) rather
//! than leaking `git2` into `turbovault-core`. Conversion to
//! `turbovault_core::Error` happens at the tool-layer boundary (added when the
//! substrate is wired into the MCP server, GWS.12).

use std::path::PathBuf;

/// Errors from the git write substrate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The vault root is not a git repository (feature is git-gated).
    #[error("not a git repository: {0}")]
    NotARepo(PathBuf),

    /// A compare-and-swap on a ref failed: the ref moved since we read it.
    /// The transaction applied nothing; the caller should re-read and retry or
    /// surface a conflict (the reconsideration domino).
    #[error("ref CAS conflict on {refname}: expected {expected:?}, found {found:?}")]
    CasConflict {
        refname: String,
        expected: Option<git2::Oid>,
        found: Option<git2::Oid>,
    },

    /// A per-file precondition failed: the blob at `path` in the base tree is not
    /// what the caller read (it changed underneath the transaction). The whole
    /// transaction aborts with **nothing applied** — the multi-file CAS / the
    /// reconsideration domino. The caller re-reads the affected paths and
    /// re-decides.
    #[error("precondition failed for {path}: expected {expected:?}, found {found:?}")]
    PreconditionFailed {
        path: String,
        expected: Option<git2::Oid>,
        found: Option<git2::Oid>,
    },

    /// Underlying libgit2 error.
    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    /// Filesystem error (working-tree materialization, etc.).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Invariant violation or unsupported state with a description.
    #[error("{0}")]
    Other(String),
}

/// Result alias for the git write substrate.
pub type Result<T> = std::result::Result<T, Error>;
