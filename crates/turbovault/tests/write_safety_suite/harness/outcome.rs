//! The outcome axis + assertion (design doc §6).
//!
//! A backend/adapter reduces whatever its layer returns (a `Result`, a
//! `CallToolResult`, an `EditResult`) into a layer-agnostic [`Observed`]; the
//! [`Outcome`] then asserts against it. `DIRTY_ERR` and `CAS_FAIL` are the same
//! [`Outcome::ConcurrencyError`] — one unified "changed underneath us" error.

/// The desired outcome of a matrix cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Write/edit succeeds and materializes to the working tree.
    Ok,
    /// Refused with **no disk change** — the unified concurrency error the
    /// matrix labels `DIRTY_ERR` *and* `CAS_FAIL`.
    ConcurrencyError,
    /// In-place op on an absent path: `FileNotFound`, nothing created.
    NoFile,
    /// An op-specific refusal that is neither a concurrency conflict nor a
    /// missing file, with **no disk change** — e.g. `edit_note`'s SEARCH text
    /// matching nothing. Surfaced by `edit_note`'s one-off; kept general so
    /// other ops can reuse it.
    OpError,
}

/// How an operation failed, classified across layers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservedError {
    /// The unified "changed underneath us" refusal (precondition / dirty tree).
    Concurrency,
    /// Target does not exist.
    NotFound,
    /// Anything else (a bug from the harness's point of view — should not occur
    /// in a passing cell).
    Other,
}

/// A backend-normalized observation of what an operation did.
///
/// `PartialEq` so the harness's own plumbing probe can assert that every World
/// produced the IDENTICAL observation for a cell (`probe.rs`) — a divergence the
/// per-world matrix assertions cannot see, since each only checks its own world
/// against the coarse expected [`Outcome`].
#[derive(Clone, Debug, PartialEq)]
pub struct Observed {
    /// The op reported success.
    pub succeeded: bool,
    /// Classified error kind when it failed.
    pub error: Option<ObservedError>,
    /// Working-tree content of the target *after* the op (`None` == absent).
    pub after_content: Option<String>,
}

impl Observed {
    pub fn ok(after_content: Option<String>) -> Self {
        Observed {
            succeeded: true,
            error: None,
            after_content,
        }
    }

    pub fn failed(error: ObservedError, after_content: Option<String>) -> Self {
        Observed {
            succeeded: false,
            error: Some(error),
            after_content,
        }
    }
}

impl Outcome {
    /// Check `observed` against this expected outcome, returning `Err(reason)` on
    /// mismatch so the runner can collect every cell's result (rather than
    /// panicking on the first). `before` is the target's content immediately
    /// before the op — a refusal must leave it byte-for-byte intact (the
    /// no-clobber invariant these tests exist to protect).
    ///
    /// The specific *effect* of an `Ok` (what bytes/deletion resulted) is the
    /// adapter's concern; here `Ok` checks only that the op succeeded, so this
    /// stays operation-agnostic.
    pub fn check(self, observed: &Observed, before: Option<&str>) -> Result<(), String> {
        match self {
            Outcome::Ok => {
                if !observed.succeeded {
                    return Err(format!("expected OK, got failure {:?}", observed.error));
                }
            }
            Outcome::ConcurrencyError => {
                if observed.succeeded {
                    return Err(
                        "expected ConcurrencyError, but the op SUCCEEDED (a clobber/defect)".into(),
                    );
                }
                if observed.error != Some(ObservedError::Concurrency) {
                    return Err(format!(
                        "expected a concurrency refusal, got {:?}",
                        observed.error
                    ));
                }
                if observed.after_content.as_deref() != before {
                    return Err(
                        "ConcurrencyError must leave the working tree unchanged (no clobber)"
                            .into(),
                    );
                }
            }
            Outcome::NoFile => {
                if observed.succeeded {
                    return Err("expected NoFile, but the op SUCCEEDED".into());
                }
                if observed.error != Some(ObservedError::NotFound) {
                    return Err(format!(
                        "expected a not-found refusal, got {:?}",
                        observed.error
                    ));
                }
                if observed.after_content.as_deref() != before {
                    return Err("NoFile must not create the target".into());
                }
            }
            Outcome::OpError => {
                if observed.succeeded {
                    return Err("expected OpError, but the op SUCCEEDED".into());
                }
                if observed.error != Some(ObservedError::Other) {
                    return Err(format!(
                        "expected an op-specific error, got {:?}",
                        observed.error
                    ));
                }
                if observed.after_content.as_deref() != before {
                    return Err("OpError must leave the working tree unchanged".into());
                }
            }
        }
        Ok(())
    }

    /// Panicking form of [`Self::check`] for direct unit assertions.
    pub fn assert(self, observed: &Observed, before: Option<&str>) {
        if let Err(msg) = self.check(observed, before) {
            panic!("{msg}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_requires_success() {
        Outcome::Ok.assert(&Observed::ok(Some("new".into())), Some("old"));
    }

    #[test]
    #[should_panic(expected = "clobber/defect")]
    fn concurrency_error_rejects_a_silent_success() {
        // A "success" where the matrix demanded a refusal is exactly the clobber
        // defect — the assertion must catch it.
        Outcome::ConcurrencyError.assert(&Observed::ok(Some("clobbered".into())), Some("dirty"));
    }

    #[test]
    #[should_panic(expected = "unchanged")]
    fn concurrency_error_requires_no_disk_change() {
        // Correct error kind, but the content changed => still a defect.
        Outcome::ConcurrencyError.assert(
            &Observed::failed(ObservedError::Concurrency, Some("changed".into())),
            Some("dirty"),
        );
    }

    #[test]
    fn concurrency_error_accepts_refusal_with_content_intact() {
        Outcome::ConcurrencyError.assert(
            &Observed::failed(ObservedError::Concurrency, Some("dirty".into())),
            Some("dirty"),
        );
    }

    #[test]
    fn nofile_requires_notfound_and_nothing_created() {
        Outcome::NoFile.assert(&Observed::failed(ObservedError::NotFound, None), None);
    }
}
