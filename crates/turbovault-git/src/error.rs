//! Error type for the git write substrate.
//!
//! The crate owns git2 / io / invariant-violation errors, plus a bridge from
//! `turbovault-core`'s error type (`Error::Core`, write-substrate-layering M1)
//! — the boundary through which `commit_changeset` (M2) surfaces failures from
//! parsing/validating a core `ChangePlan`/`Precondition`, without this crate
//! re-declaring core's variants. `git2` itself still never leaks into a
//! consumer crate; conversion to the consumer's error type happens at the
//! tool-layer boundary (added when the substrate is wired into the MCP
//! server, GWS.12).

use std::path::PathBuf;

/// Errors from the git write substrate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The vault root is not a git repository (feature is git-gated).
    #[error("not a git repository: {0}")]
    NotARepo(PathBuf),

    /// A compare-and-swap on a ref failed: the ref moved since we read it.
    /// The changeset applied nothing; the caller should re-read and retry or
    /// surface a conflict (the reconsideration domino).
    #[error("ref CAS conflict on {refname}: expected {expected:?}, found {found:?}")]
    CasConflict {
        refname: String,
        expected: Option<git2::Oid>,
        found: Option<git2::Oid>,
    },

    /// A per-file precondition failed: the blob at `path` in the base tree is not
    /// what the caller read (it changed underneath the changeset). The whole
    /// changeset aborts with **nothing applied** — the multi-file CAS / the
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

    /// Bridged error from `turbovault-core` (write-substrate-layering M1/ij6):
    /// the boundary through which `commit_changeset` (M2) surfaces failures
    /// from parsing/validating a core `ChangePlan`/`Precondition`, without
    /// this crate re-declaring core's error variants.
    #[error("core error: {0}")]
    Core(#[from] turbovault_core::Error),
}

/// Result alias for the git write substrate.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn core_error_bridges_via_from() {
        let core_err = turbovault_core::Error::not_found("a.md");
        let git_err: Error = core_err.into();
        assert!(matches!(git_err, Error::Core(_)));
        assert!(git_err.to_string().contains("Not found in graph"));
    }
}
