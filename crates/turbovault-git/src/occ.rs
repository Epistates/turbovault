//! Per-file optimistic-concurrency precondition (GWS.4) — the multi-file CAS /
//! "reconsideration domino".
//!
//! A transaction reads each target path and remembers the **blob oid** it saw
//! (the version token: `blob_oid_of(bytes)` for working-tree bytes the agent
//! read). Before committing, [`VaultRepo::check_preconditions`] re-resolves each
//! path against the base tree it is building on and confirms the blob oid still
//! matches. If **any** path changed underneath the transaction, the whole batch
//! aborts with [`Error::PreconditionFailed`] and nothing is applied — so the
//! agent re-reads the affected paths and re-decides rather than silently
//! overwriting a concurrent change.
//!
//! This is the WS-B.2 OCC validate phase re-expressed against git blob oids:
//! content-addressing makes the comparison exact and cheap.

use crate::error::{Error, Result};
use crate::repo::VaultRepo;
use git2::{ObjectType, Oid};

/// A precondition on one path: the blob oid the caller expects to find in the
/// base tree. `expected == None` asserts the path is **absent** (a create).
#[derive(Debug, Clone)]
pub struct Precondition {
    pub path: String,
    pub expected: Option<Oid>,
}

impl Precondition {
    /// The path must currently hold exactly this blob (an update of known content).
    pub fn expect_blob(path: impl Into<String>, blob: Oid) -> Self {
        Self {
            path: path.into(),
            expected: Some(blob),
        }
    }

    /// The path must currently be absent (a create).
    pub fn expect_absent(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            expected: None,
        }
    }
}

impl VaultRepo {
    /// The blob oid of `content` **without writing it** to the object DB — the
    /// version token for bytes an agent read from the working tree. Equals the
    /// blob oid that [`Self::build_tree`] would store for the same bytes, so a
    /// token computed at read time can be compared directly against a base
    /// tree's entry at commit time.
    pub fn blob_oid_of(content: &[u8]) -> Result<Oid> {
        Ok(Oid::hash_object(ObjectType::Blob, content)?)
    }

    /// Validate every precondition against `base_tree` (the tree the transaction
    /// is building on; `None` = an empty/unborn base where nothing exists).
    /// Returns `Ok(())` only if **all** match; the first mismatch aborts with
    /// [`Error::PreconditionFailed`] (the whole transaction fails, nothing
    /// applied).
    pub fn check_preconditions(
        &self,
        base_tree: Option<Oid>,
        preconditions: &[Precondition],
    ) -> Result<()> {
        for pc in preconditions {
            let found = match base_tree {
                Some(tree) => self.blob_oid_at(tree, &pc.path)?,
                None => None, // empty base: every path is absent
            };
            if found != pc.expected {
                return Err(Error::PreconditionFailed {
                    path: pc.path.clone(),
                    expected: pc.expected,
                    found,
                });
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
                Precondition::expect_blob("a.md", a),
                Precondition::expect_blob("b.md", b),
                Precondition::expect_absent("c.md"),
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
        match vr.check_preconditions(Some(t), &[Precondition::expect_blob("a.md", stale)]) {
            Err(Error::PreconditionFailed { path, .. }) => assert_eq!(path, "a.md"),
            other => panic!("expected PreconditionFailed, got {other:?}"),
        }
    }

    #[test]
    fn expect_absent_but_present_fails() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        assert!(matches!(
            vr.check_preconditions(Some(t), &[Precondition::expect_absent("a.md")]),
            Err(Error::PreconditionFailed { .. })
        ));
    }

    #[test]
    fn expect_blob_but_absent_fails() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let phantom = VaultRepo::blob_oid_of(b"x").unwrap();
        assert!(matches!(
            vr.check_preconditions(Some(t), &[Precondition::expect_blob("missing.md", phantom)]),
            Err(Error::PreconditionFailed { .. })
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
                Precondition::expect_blob("a.md", a),
                Precondition::expect_blob("b.md", b_stale),
            ],
        ) {
            Err(Error::PreconditionFailed { path, .. }) => assert_eq!(path, "b.md"),
            other => panic!("expected PreconditionFailed on b.md, got {other:?}"),
        }
    }

    #[test]
    fn empty_base_treats_all_as_absent() {
        let (_tmp, vr) = open_unborn();
        // Against an unborn/empty base, expect_absent passes and expect_blob fails.
        vr.check_preconditions(None, &[Precondition::expect_absent("a.md")])
            .expect("absent on empty base");
        let phantom = VaultRepo::blob_oid_of(b"x").unwrap();
        assert!(matches!(
            vr.check_preconditions(None, &[Precondition::expect_blob("a.md", phantom)]),
            Err(Error::PreconditionFailed { .. })
        ));
    }
}
