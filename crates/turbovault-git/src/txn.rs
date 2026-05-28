//! Transaction = one commit (GWS.7).
//!
//! A [`Transaction`] is a set of tree changes plus per-file preconditions and a
//! commit message. [`VaultRepo::apply_transaction`] runs the full pipeline as
//! **one commit**, under the worktree commit lock:
//!
//! 1. acquire the per-worktree commit lock (GWS.6);
//! 2. `commit_with_retry` on the current branch ref (GWS.3): each attempt
//!    resolves the tip to its base tree, **checks preconditions** against that
//!    base (GWS.4) — a mismatch aborts the whole transaction (the
//!    reconsideration domino), no retry — then builds the new tree in an
//!    isolated index (GWS.2) and `commit-tree`s on the tip. On a ref CAS loss
//!    the tip is re-read and the attempt retried, which re-checks preconditions
//!    on the new base, so a concurrent change to one of the transaction's own
//!    paths surfaces as an abort, not a silent overwrite;
//! 3. **materialize** the committed paths into the working tree (GWS.5);
//! 4. release the lock.
//!
//! Single-file writes are a degenerate one-change transaction; batches and
//! move+link-updates are multi-change transactions — all atomic, one commit.

use crate::error::{Error, Result};
use crate::occ::Precondition;
use crate::plumbing::TreeChange;
use crate::repo::VaultRepo;
use git2::Oid;
use std::collections::BTreeSet;

/// A unit of change applied as a single commit.
#[derive(Debug, Clone, Default)]
pub struct Transaction {
    message: String,
    changes: Vec<TreeChange>,
    preconditions: Vec<Precondition>,
}

impl Transaction {
    /// Start a transaction with a commit message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Default::default()
        }
    }

    /// Add or overwrite a file.
    pub fn upsert(mut self, path: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        self.changes.push(TreeChange::Upsert {
            path: path.into(),
            content: content.into(),
        });
        self
    }

    /// Remove a file.
    pub fn remove(mut self, path: impl Into<String>) -> Self {
        self.changes.push(TreeChange::Remove { path: path.into() });
        self
    }

    /// Add an arbitrary [`TreeChange`].
    pub fn with_change(mut self, change: TreeChange) -> Self {
        self.changes.push(change);
        self
    }

    /// Require `path` to currently hold `blob` (an update of known content).
    pub fn expect_blob(mut self, path: impl Into<String>, blob: Oid) -> Self {
        self.preconditions
            .push(Precondition::expect_blob(path, blob));
        self
    }

    /// Require `path` to currently be absent (a create).
    pub fn expect_absent(mut self, path: impl Into<String>) -> Self {
        self.preconditions.push(Precondition::expect_absent(path));
        self
    }

    /// Add an arbitrary [`Precondition`] (e.g. over a page read but not written,
    /// extending the multi-file CAS to the transaction's read set).
    pub fn with_precondition(mut self, precondition: Precondition) -> Self {
        self.preconditions.push(precondition);
        self
    }

    /// The distinct paths this transaction mutates (the materialization set).
    fn changed_paths(&self) -> Vec<String> {
        self.changes.iter().map(|c| c.path().to_string()).collect()
    }
}

/// Outcome of a committed transaction.
#[derive(Debug, Clone)]
pub struct TransactionResult {
    /// The new commit the branch now points at.
    pub commit: Oid,
    /// The paths materialized into the working tree.
    pub paths: Vec<String>,
}

impl VaultRepo {
    /// Apply `txn` as a single commit (see the module docs for the pipeline).
    ///
    /// Aborts with [`Error::PreconditionFailed`] if any precondition is stale
    /// (nothing committed, working tree untouched) and with [`Error::Other`] for
    /// an empty transaction or duplicate change paths.
    pub fn apply_transaction(&self, txn: &Transaction) -> Result<TransactionResult> {
        if txn.changes.is_empty() {
            return Err(Error::Other("empty transaction (no changes)".to_string()));
        }
        // A path mutated twice in one transaction is ambiguous — reject it.
        let mut seen = BTreeSet::new();
        for c in &txn.changes {
            if !seen.insert(c.path()) {
                return Err(Error::Other(format!(
                    "duplicate change for path {} in one transaction",
                    c.path()
                )));
            }
        }

        let refname = self.head_ref()?; // errors if HEAD is detached
        let changed = txn.changed_paths();

        self.with_commit_lock(|| {
            let commit = self.commit_with_retry(&refname, |tip| {
                let base_tree = match tip {
                    Some(c) => Some(self.git().find_commit(c)?.tree_id()),
                    None => None,
                };
                // Abort the whole transaction if any precondition is stale.
                self.check_preconditions(base_tree, &txn.preconditions)?;
                let tree = self.build_tree(base_tree, &txn.changes)?;
                let parents: Vec<Oid> = tip.into_iter().collect();
                self.commit_tree(tree, &parents, &txn.message)
            })?;

            // Reveal the commit to the working tree (still under the lock).
            self.materialize(commit, &changed)?;

            Ok(TransactionResult {
                commit,
                paths: changed,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    fn workfile(vr: &VaultRepo, rel: &str) -> std::path::PathBuf {
        vr.git().workdir().unwrap().join(rel)
    }

    fn read_wt(vr: &VaultRepo, rel: &str) -> String {
        std::fs::read_to_string(workfile(vr, rel)).unwrap()
    }

    #[test]
    fn create_on_unborn_makes_initial_commit() {
        let (_tmp, vr) = open_unborn();
        let txn = Transaction::new("create a")
            .upsert("a.md", "alpha")
            .expect_absent("a.md");
        let res = vr.apply_transaction(&txn).unwrap();

        assert_eq!(
            vr.head_oid(),
            Some(res.commit),
            "branch advanced to the commit"
        );
        assert_eq!(
            read_wt(&vr, "a.md"),
            "alpha",
            "materialized to working tree"
        );
    }

    #[test]
    fn update_with_correct_precondition_succeeds() {
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(&Transaction::new("c").upsert("a.md", "v1"))
            .unwrap();

        let v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        let txn = Transaction::new("update a")
            .upsert("a.md", "v2")
            .expect_blob("a.md", v1);
        vr.apply_transaction(&txn).unwrap();
        assert_eq!(read_wt(&vr, "a.md"), "v2");
    }

    #[test]
    fn stale_precondition_aborts_nothing_applied() {
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(&Transaction::new("c").upsert("a.md", "v1"))
            .unwrap();
        let head_before = vr.head_oid();

        // Caller thinks a.md still holds "stale" content.
        let stale = VaultRepo::blob_oid_of(b"stale").unwrap();
        let txn = Transaction::new("bad update")
            .upsert("a.md", "v2")
            .expect_blob("a.md", stale);
        assert!(matches!(
            vr.apply_transaction(&txn),
            Err(Error::PreconditionFailed { .. })
        ));

        assert_eq!(vr.head_oid(), head_before, "no commit on abort");
        assert_eq!(
            read_wt(&vr, "a.md"),
            "v1",
            "working tree untouched on abort"
        );
    }

    #[test]
    fn multi_file_batch_is_one_atomic_commit() {
        let (_tmp, vr) = open_unborn();
        let txn = Transaction::new("batch")
            .upsert("a.md", "A")
            .upsert("dir/b.md", "B")
            .remove("ghost.md"); // remove of absent path is a no-op in the tree
        let res = vr.apply_transaction(&txn).unwrap();

        // Exactly one commit; both writes present.
        let commit = vr.git().find_commit(res.commit).unwrap();
        assert_eq!(
            commit.parent_count(),
            0,
            "single initial commit for the batch"
        );
        assert_eq!(read_wt(&vr, "a.md"), "A");
        assert_eq!(read_wt(&vr, "dir/b.md"), "B");
    }

    #[test]
    fn read_set_precondition_aborts_batch() {
        // Multi-file CAS over the read set: the txn writes a.md but also asserts
        // b.md is unchanged. If b.md moved, the whole batch aborts even though we
        // never write b.md.
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(&Transaction::new("seed").upsert("b.md", "B1"))
            .unwrap();
        let head_before = vr.head_oid();

        let stale_b = VaultRepo::blob_oid_of(b"B-OLD").unwrap();
        let txn = Transaction::new("write a, guard b")
            .upsert("a.md", "A")
            .expect_blob("b.md", stale_b);
        assert!(matches!(
            vr.apply_transaction(&txn),
            Err(Error::PreconditionFailed { path, .. }) if path == "b.md"
        ));
        assert_eq!(vr.head_oid(), head_before, "nothing committed");
        assert!(!workfile(&vr, "a.md").exists(), "a.md never materialized");
    }

    #[test]
    fn empty_transaction_rejected() {
        let (_tmp, vr) = open_unborn();
        assert!(matches!(
            vr.apply_transaction(&Transaction::new("empty")),
            Err(Error::Other(_))
        ));
    }

    #[test]
    fn duplicate_change_path_rejected() {
        let (_tmp, vr) = open_unborn();
        let txn = Transaction::new("dup")
            .upsert("a.md", "x")
            .upsert("a.md", "y");
        assert!(matches!(vr.apply_transaction(&txn), Err(Error::Other(_))));
    }

    #[test]
    fn move_as_remove_plus_upsert_one_commit() {
        // The move+links shape (GWS.8 will build these): remove old + add new in
        // one atomic commit.
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(&Transaction::new("seed").upsert("old.md", "body"))
            .unwrap();

        let txn = Transaction::new("move old->new")
            .remove("old.md")
            .upsert("new.md", "body");
        let res = vr.apply_transaction(&txn).unwrap();

        assert!(!workfile(&vr, "old.md").exists(), "old path removed");
        assert_eq!(read_wt(&vr, "new.md"), "body", "new path written");
        // One commit carried both the removal and the add.
        let tree = vr.git().find_commit(res.commit).unwrap().tree_id();
        assert!(vr.blob_oid_at(tree, "old.md").unwrap().is_none());
        assert!(vr.blob_oid_at(tree, "new.md").unwrap().is_some());
    }
}
