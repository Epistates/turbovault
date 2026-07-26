//! Error type for the git write substrate.
//!
//! The crate owns only the genuinely git2-specific error shapes (`NotARepo`,
//! `Git`) plus a bridge from `turbovault-core`'s error type (`Error::Core`,
//! write-substrate-layering M1/ij6). Everything that isn't git2-specific —
//! "changed underneath us" conflicts (ref CAS / stale precondition), I/O, and
//! free-form invariant errors — routes through `core::Error` instead of a
//! locally-declared duplicate, via the [`Error::concurrency`] / [`Error::other`]
//! constructors and the `From<std::io::Error>` impl below. `git2` itself still
//! never leaks into a consumer crate; conversion to the consumer's error type
//! happens at the tool-layer boundary (added when the substrate is wired into
//! the MCP server, GWS.12).

use std::path::PathBuf;

/// Errors from the git write substrate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The vault root is not a git repository (feature is git-gated).
    #[error("not a git repository: {0}")]
    NotARepo(PathBuf),

    /// Underlying libgit2 error.
    #[error("git error: {0}")]
    Git(#[from] git2::Error),

    /// Bridged error from `turbovault-core` (write-substrate-layering M1/ij6):
    /// the boundary through which `commit_changeset` (M2) surfaces failures
    /// from parsing/validating a core `ChangePlan`/`Precondition`, without
    /// this crate re-declaring core's error variants. Also where this crate's
    /// own "changed underneath us" / io / free-form errors land — see
    /// [`Error::concurrency`] / [`Error::other`] / `From<std::io::Error>`.
    #[error(transparent)]
    Core(#[from] turbovault_core::Error),
}

impl Error {
    /// A "changed underneath us" conflict — a ref CAS race (`cas_ref`) or a
    /// stale per-file precondition (`check_preconditions`). Replaces the
    /// removed `CasConflict`/`PreconditionFailed` variants; callers format
    /// the old structured fields (refname/path, expected, found) into
    /// `reason` so nothing is lost, e.g. "ref CAS conflict on {refname}:
    /// expected {..}, found {..}".
    pub(crate) fn concurrency(reason: impl Into<String>) -> Self {
        Error::Core(turbovault_core::Error::concurrency_error(reason))
    }

    /// A free-form invariant violation or unsupported state. Replaces the
    /// removed `Other` catch-all variant.
    pub(crate) fn other(msg: impl Into<String>) -> Self {
        Error::Core(turbovault_core::Error::other(msg))
    }
}

/// Replaces the removed `Error::Io(#[from] io::Error)` variant: routes
/// through `core::Error::Io` instead of a locally-declared duplicate, so `?`
/// on an `io::Error` still converts for free.
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Core(turbovault_core::Error::Io(e))
    }
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

    #[test]
    fn io_error_bridges_via_from() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let git_err: Error = io_err.into();
        assert!(matches!(
            git_err,
            Error::Core(turbovault_core::Error::Io(_))
        ));
    }

    #[test]
    fn concurrency_and_other_route_through_core() {
        assert!(matches!(
            Error::concurrency("stale"),
            Error::Core(turbovault_core::Error::ConcurrencyError { .. })
        ));
        assert!(matches!(
            Error::other("nope"),
            Error::Core(turbovault_core::Error::Other(_))
        ));
    }
}
