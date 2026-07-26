//! Per-file optimistic-concurrency precondition (GWS.4) — the multi-file CAS /
//! "reconsideration domino".
//!
//! `commit_changeset` reads each target path and remembers the **blob oid** it
//! saw (the version token: `blob_oid_of(bytes)` for working-tree bytes the
//! agent read). Before committing, [`VaultRepo::check_preconditions`]
//! re-resolves each path against the base tree it is building on and confirms
//! the precondition still holds. If **any** path fails, the whole plan aborts
//! with a `ConcurrencyError` (via [`Error::concurrency`]) and nothing is
//! applied — so the agent re-reads the affected paths and re-decides rather
//! than silently overwriting a concurrent change.
//!
//! This is the WS-B.2 OCC validate phase re-expressed against git blob oids:
//! content-addressing makes the comparison exact and cheap.
//!
//! write-substrate-layering M2 / design §6.2: `Precondition` is
//! `turbovault_core::Precondition` — the git-owned type (which could only
//! express `Option<Oid>`, i.e. `ExpectBlob`/`ExpectAbsent`) is deleted;
//! `check_preconditions` now handles all four variants and owns the hex→`Oid`
//! parse. A malformed `ExpectBlob` token surfaces as a loud
//! `ConcurrencyError`, matching the abort-nothing-applied behavior of a
//! genuinely stale token — never a raw [`git2::Error`].

use crate::error::{Error, Result};
use crate::repo::VaultRepo;
use git2::{ObjectType, Oid};
use tracing::instrument;
use turbovault_core::Precondition;

impl VaultRepo {
    /// The blob oid of `content` **without writing it** to the object DB — the
    /// version token for bytes an agent read from the working tree. Equals the
    /// blob oid that [`Self::build_tree`] would store for the same bytes, so a
    /// token computed at read time can be compared directly against a base
    /// tree's entry at commit time.
    pub fn blob_oid_of(content: &[u8]) -> Result<Oid> {
        Ok(Oid::hash_object(ObjectType::Blob, content)?)
    }

    /// Validate every `(path, Precondition)` against `base_tree` (the tree the
    /// changeset is building on; `None` = an empty/unborn base where nothing
    /// exists). Returns `Ok(())` only if **all** match; the first mismatch
    /// aborts with a `ConcurrencyError` (via [`Error::concurrency`]; the whole
    /// changeset fails, nothing applied).
    #[instrument(
        skip(self, preconditions),
        fields(base = ?base_tree, n = preconditions.len()),
        name = "git_check_preconditions"
    )]
    pub fn check_preconditions(
        &self,
        base_tree: Option<Oid>,
        preconditions: &[(String, Precondition)],
    ) -> Result<()> {
        for (path, pc) in preconditions {
            let found = match base_tree {
                Some(tree) => self.blob_oid_at(tree, path)?,
                None => None, // empty base: every path is absent
            };
            match pc {
                Precondition::ExpectBlob(hex) => {
                    let expected = Oid::from_str(hex).map_err(|_| {
                        Error::concurrency(format!(
                            "precondition failed for {path}: malformed expected blob token {hex:?}"
                        ))
                    })?;
                    if found != Some(expected) {
                        return Err(Error::concurrency(format!(
                            "precondition failed for {path}: expected {expected}, found {found:?}"
                        )));
                    }
                }
                Precondition::ExpectAbsent => {
                    if found.is_some() {
                        return Err(Error::concurrency(format!(
                            "precondition failed for {path}: expected absent, found {found:?}"
                        )));
                    }
                }
                Precondition::ExpectExists => {
                    if found.is_none() {
                        return Err(Error::concurrency(format!(
                            "precondition failed for {path}: expected present, found absent"
                        )));
                    }
                }
                Precondition::Blind => {}
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plumbing::TreeChange;
    use git2::Repository;
    use tempfile::TempDir;

    fn open_unborn() -> (TempDir, VaultRepo) {
        let tmp = TempDir::new().unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(tmp.path(), &opts).unwrap();
        let vr = VaultRepo::open(tmp.path()).unwrap();
        (tmp, vr)
    }

    fn upsert(path: &str, content: &str) -> TreeChange {
        TreeChange::Upsert {
            path: path.to_string(),
            content: content.as_bytes().to_vec(),
        }
    }

    #[test]
    fn version_token_matches_stored_blob() {
        // The read-path contract: the oid an agent computes from the bytes it read
        // equals the blob oid stored in the tree for those bytes.
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let stored = vr.blob_oid_at(t, "a.md").unwrap().unwrap();
        let token = VaultRepo::blob_oid_of(b"alpha").unwrap();
        assert_eq!(
            token, stored,
            "version token must equal the stored blob oid"
        );
    }

    #[test]
    fn matching_preconditions_pass() {
        let (_tmp, vr) = open_unborn();
        let t = vr
            .build_tree(None, &[upsert("a.md", "alpha"), upsert("b.md", "beta")])
            .unwrap();
        let a = VaultRepo::blob_oid_of(b"alpha").unwrap();
        let b = VaultRepo::blob_oid_of(b"beta").unwrap();
        vr.check_preconditions(
            Some(t),
            &[
                ("a.md".to_string(), Precondition::ExpectBlob(a.to_string())),
                ("b.md".to_string(), Precondition::ExpectBlob(b.to_string())),
                ("c.md".to_string(), Precondition::ExpectAbsent),
            ],
        )
        .expect("all preconditions match");
    }

    #[test]
    fn changed_blob_fails() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        // Caller thinks a.md holds "stale" but it actually holds "alpha".
        let stale = VaultRepo::blob_oid_of(b"stale").unwrap();
        match vr.check_preconditions(
            Some(t),
            &[(
                "a.md".to_string(),
                Precondition::ExpectBlob(stale.to_string()),
            )],
        ) {
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { reason })) => {
                assert!(reason.contains("a.md"), "reason: {reason}")
            }
            other => panic!("expected ConcurrencyError, got {other:?}"),
        }
    }

    #[test]
    fn expect_absent_but_present_fails() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        assert!(matches!(
            vr.check_preconditions(Some(t), &[("a.md".to_string(), Precondition::ExpectAbsent)]),
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { .. }))
        ));
    }

    #[test]
    fn expect_blob_but_absent_fails() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let phantom = VaultRepo::blob_oid_of(b"x").unwrap();
        assert!(matches!(
            vr.check_preconditions(
                Some(t),
                &[(
                    "missing.md".to_string(),
                    Precondition::ExpectBlob(phantom.to_string())
                )]
            ),
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { .. }))
        ));
    }

    #[test]
    fn one_stale_among_many_aborts_all() {
        // The domino: a single stale path fails the whole multi-file check.
        let (_tmp, vr) = open_unborn();
        let t = vr
            .build_tree(None, &[upsert("a.md", "alpha"), upsert("b.md", "beta")])
            .unwrap();
        let a = VaultRepo::blob_oid_of(b"alpha").unwrap();
        let b_stale = VaultRepo::blob_oid_of(b"OLD-beta").unwrap();
        match vr.check_preconditions(
            Some(t),
            &[
                ("a.md".to_string(), Precondition::ExpectBlob(a.to_string())),
                (
                    "b.md".to_string(),
                    Precondition::ExpectBlob(b_stale.to_string()),
                ),
            ],
        ) {
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { reason })) => {
                assert!(reason.contains("b.md"), "reason: {reason}")
            }
            other => panic!("expected ConcurrencyError on b.md, got {other:?}"),
        }
    }

    #[test]
    fn empty_base_treats_all_as_absent() {
        let (_tmp, vr) = open_unborn();
        // Against an unborn/empty base, expect_absent passes and expect_blob fails.
        vr.check_preconditions(None, &[("a.md".to_string(), Precondition::ExpectAbsent)])
            .expect("absent on empty base");
        let phantom = VaultRepo::blob_oid_of(b"x").unwrap();
        assert!(matches!(
            vr.check_preconditions(
                None,
                &[(
                    "a.md".to_string(),
                    Precondition::ExpectBlob(phantom.to_string())
                )]
            ),
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { .. }))
        ));
    }

    // -------- write-substrate-layering M2: ExpectExists / Blind / malformed hex --------

    #[test]
    fn expect_exists_passes_when_present_fails_when_absent() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        vr.check_preconditions(Some(t), &[("a.md".to_string(), Precondition::ExpectExists)])
            .expect("present path satisfies ExpectExists");
        assert!(matches!(
            vr.check_preconditions(
                Some(t),
                &[("missing.md".to_string(), Precondition::ExpectExists)]
            ),
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { .. }))
        ));
    }

    #[test]
    fn blind_skips_the_check_regardless_of_state() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        // a.md exists but Blind never looks.
        vr.check_preconditions(Some(t), &[("a.md".to_string(), Precondition::Blind)])
            .expect("Blind never checks");
        // missing.md is absent — still fine under Blind.
        vr.check_preconditions(Some(t), &[("missing.md".to_string(), Precondition::Blind)])
            .expect("Blind never checks even when absent");
    }

    #[test]
    fn malformed_expect_blob_token_is_loud_concurrency_error_not_git2() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let err = vr
            .check_preconditions(
                Some(t),
                &[(
                    "a.md".to_string(),
                    Precondition::ExpectBlob("not-a-hex-oid".to_string()),
                )],
            )
            .unwrap_err();
        assert!(
            matches!(
                err,
                Error::Core(turbovault_core::Error::ConcurrencyError { .. })
            ),
            "malformed token must surface as ConcurrencyError, not a raw git2 error: {err:?}"
        );
    }
}
