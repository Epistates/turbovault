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
use tracing::instrument;

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

    // -------- Semantic ops --------
    // These compose the raw primitives (`upsert`/`remove`/preconditions) with the
    // safe-by-default precondition policy. Use them for the standard ops; reach
    // for the raw builders only when you explicitly want a blind write.

    /// Create a new file. Precondition: `path` must currently be absent.
    /// Aborts the transaction if the path exists.
    pub fn create(mut self, path: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        let path = path.into();
        self.changes.push(TreeChange::Upsert {
            path: path.clone(),
            content: content.into(),
        });
        self.preconditions.push(Precondition::expect_absent(path));
        self
    }

    /// Update an existing file. Precondition: `path` must currently hold
    /// `expected` (the version token the caller read). Protects against lost
    /// updates — a concurrent change to `path` aborts the transaction.
    pub fn update(
        mut self,
        path: impl Into<String>,
        content: impl Into<Vec<u8>>,
        expected: Oid,
    ) -> Self {
        let path = path.into();
        self.changes.push(TreeChange::Upsert {
            path: path.clone(),
            content: content.into(),
        });
        self.preconditions
            .push(Precondition::expect_blob(path, expected));
        self
    }

    /// Delete an existing file. Precondition: it must currently hold `expected`.
    /// Aborts if the file changed or is absent since the caller read it.
    pub fn delete(mut self, path: impl Into<String>, expected: Oid) -> Self {
        let path = path.into();
        self.changes.push(TreeChange::Remove { path: path.clone() });
        self.preconditions
            .push(Precondition::expect_blob(path, expected));
        self
    }

    /// Atomic rename: move `from` (which must currently hold `expected_from`)
    /// to `to` (which must currently be absent), as **one commit**. Both
    /// endpoints get preconditions. `content` is the bytes to write at `to` —
    /// usually the caller passes the source's bytes unchanged for a pure
    /// rename; passing different bytes is a rename-and-modify. Chain
    /// `.update()`/`.upsert()` after this for link-target updates in the same
    /// commit (move + link-updates is atomic).
    pub fn rename(
        mut self,
        from: impl Into<String>,
        to: impl Into<String>,
        content: impl Into<Vec<u8>>,
        expected_from: Oid,
    ) -> Self {
        let from = from.into();
        let to = to.into();
        self.changes.push(TreeChange::Remove { path: from.clone() });
        self.changes.push(TreeChange::Upsert {
            path: to.clone(),
            content: content.into(),
        });
        self.preconditions
            .push(Precondition::expect_blob(from, expected_from));
        self.preconditions.push(Precondition::expect_absent(to));
        self
    }

    /// The distinct paths this transaction mutates (the materialization set).
    fn changed_paths(&self) -> Vec<String> {
        self.changes.iter().map(|c| c.path().to_string()).collect()
    }

    /// turbovault-lri: enumerate the paths this transaction mutates so
    /// higher layers can apply policies like the `include_ignored`
    /// gitignore-refusal check before submission. Same content as the
    /// private `changed_paths` helper; exposed for consumers in
    /// `turbovault-tools`.
    pub fn touched_paths(&self) -> Vec<String> {
        self.changed_paths()
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
    #[instrument(
        skip(self, txn),
        fields(
            message = %txn.message,
            n_changes = txn.changes.len(),
            n_preconditions = txn.preconditions.len(),
        ),
        name = "git_apply_transaction"
    )]
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
            // `parent_at_apply` is captured INSIDE `commit_with_retry`'s
            // success closure so the post-commit hook reports the correct
            // first parent even after a CAS-rebuild loop (the parent we
            // committed against, NOT the parent at function entry, which
            // may be stale).
            let mut parent_at_apply: Option<Oid> = None;
            let commit = self.commit_with_retry(&refname, |tip| {
                parent_at_apply = tip;
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

            // Fire the GWS.14 reindex hook inside the commit lock so the
            // queue observes commits in commit order (matches the order
            // a future drainer must replay them).
            if let Some(hook) = &self.commit_hook {
                hook(parent_at_apply, commit);
            }

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

    // -------- Semantic constructors (GWS.8) --------

    #[test]
    fn create_on_absent_succeeds_create_on_existing_fails() {
        let (_tmp, vr) = open_unborn();
        // create succeeds when path is absent.
        vr.apply_transaction(&Transaction::new("c").create("a.md", "alpha"))
            .unwrap();
        assert_eq!(read_wt(&vr, "a.md"), "alpha");

        // create on an existing path fails (expect_absent precondition).
        let res = vr.apply_transaction(&Transaction::new("c2").create("a.md", "again"));
        assert!(matches!(res, Err(Error::PreconditionFailed { path, .. }) if path == "a.md"));
        assert_eq!(
            read_wt(&vr, "a.md"),
            "alpha",
            "no overwrite on create-existing"
        );
    }

    #[test]
    fn update_requires_correct_expected_blob() {
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(&Transaction::new("seed").create("a.md", "v1"))
            .unwrap();

        let v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        vr.apply_transaction(&Transaction::new("u").update("a.md", "v2", v1))
            .unwrap();
        assert_eq!(read_wt(&vr, "a.md"), "v2");

        // Update with stale expected (still v1, but file is now v2) aborts.
        let res = vr.apply_transaction(&Transaction::new("u-stale").update("a.md", "v3", v1));
        assert!(matches!(res, Err(Error::PreconditionFailed { path, .. }) if path == "a.md"));
        assert_eq!(read_wt(&vr, "a.md"), "v2", "stale update did not apply");
    }

    #[test]
    fn delete_requires_correct_expected_blob() {
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(&Transaction::new("seed").create("a.md", "v1"))
            .unwrap();

        // Stale expected -> abort, file still there.
        let stale = VaultRepo::blob_oid_of(b"OLD").unwrap();
        let res = vr.apply_transaction(&Transaction::new("d-stale").delete("a.md", stale));
        assert!(matches!(res, Err(Error::PreconditionFailed { path, .. }) if path == "a.md"));
        assert!(workfile(&vr, "a.md").exists(), "stale delete did not apply");

        // Correct expected -> file gone.
        let v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        vr.apply_transaction(&Transaction::new("d").delete("a.md", v1))
            .unwrap();
        assert!(!workfile(&vr, "a.md").exists());
    }

    #[test]
    fn rename_atomically_with_endpoint_preconditions() {
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(&Transaction::new("seed").create("old.md", "body"))
            .unwrap();

        let from_blob = VaultRepo::blob_oid_of(b"body").unwrap();
        let res = vr
            .apply_transaction(
                &Transaction::new("rn").rename("old.md", "new.md", "body", from_blob),
            )
            .unwrap();

        assert!(!workfile(&vr, "old.md").exists(), "source removed");
        assert_eq!(read_wt(&vr, "new.md"), "body", "destination written");
        let tree = vr.git().find_commit(res.commit).unwrap().tree_id();
        assert!(vr.blob_oid_at(tree, "old.md").unwrap().is_none());
        assert!(vr.blob_oid_at(tree, "new.md").unwrap().is_some());
    }

    #[test]
    fn rename_aborts_on_stale_source() {
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(&Transaction::new("seed").create("old.md", "body"))
            .unwrap();

        let stale = VaultRepo::blob_oid_of(b"different").unwrap();
        let res =
            vr.apply_transaction(&Transaction::new("rn").rename("old.md", "new.md", "body", stale));
        assert!(matches!(res, Err(Error::PreconditionFailed { path, .. }) if path == "old.md"));
        assert!(workfile(&vr, "old.md").exists(), "source kept on abort");
        assert!(
            !workfile(&vr, "new.md").exists(),
            "destination not written on abort"
        );
    }

    #[test]
    fn rename_aborts_when_destination_exists() {
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(
            &Transaction::new("seed")
                .create("old.md", "body")
                .create("new.md", "occupied"),
        )
        .unwrap();

        let from_blob = VaultRepo::blob_oid_of(b"body").unwrap();
        let res = vr.apply_transaction(
            &Transaction::new("rn").rename("old.md", "new.md", "body", from_blob),
        );
        assert!(matches!(res, Err(Error::PreconditionFailed { path, .. }) if path == "new.md"));
        assert!(workfile(&vr, "old.md").exists());
        assert_eq!(read_wt(&vr, "new.md"), "occupied", "destination untouched");
    }

    #[test]
    fn rename_chained_with_link_updates_is_one_commit() {
        // Move + update-links: rename old->new AND fix link targets in two other
        // files, all in one atomic commit (the case the legacy batch couldn't).
        let (_tmp, vr) = open_unborn();
        vr.apply_transaction(
            &Transaction::new("seed")
                .create("old.md", "body")
                .create("link1.md", "see [[old]]")
                .create("link2.md", "ref [[old]] here"),
        )
        .unwrap();

        let body_blob = VaultRepo::blob_oid_of(b"body").unwrap();
        let l1_blob = VaultRepo::blob_oid_of(b"see [[old]]").unwrap();
        let l2_blob = VaultRepo::blob_oid_of(b"ref [[old]] here").unwrap();
        let res = vr
            .apply_transaction(
                &Transaction::new("mv+links")
                    .rename("old.md", "new.md", "body", body_blob)
                    .update("link1.md", "see [[new]]", l1_blob)
                    .update("link2.md", "ref [[new]] here", l2_blob),
            )
            .unwrap();

        // All four file changes landed in ONE commit.
        let tree = vr.git().find_commit(res.commit).unwrap().tree_id();
        assert!(vr.blob_oid_at(tree, "old.md").unwrap().is_none());
        assert!(vr.blob_oid_at(tree, "new.md").unwrap().is_some());
        assert_eq!(read_wt(&vr, "link1.md"), "see [[new]]");
        assert_eq!(read_wt(&vr, "link2.md"), "ref [[new]] here");
    }

    // -------- GWS.14: commit hook --------

    type HookCalls = std::sync::Arc<std::sync::Mutex<Vec<(Option<Oid>, Oid)>>>;
    type CommitOnlyCalls = std::sync::Arc<std::sync::Mutex<Vec<Oid>>>;

    fn open_unborn_with_hook(hook: crate::CommitHook) -> (TempDir, crate::VaultRepo) {
        let tmp = TempDir::new().unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(tmp.path(), &opts).unwrap();
        let vr = crate::VaultRepo::open_with_locks_and_hook(
            tmp.path(),
            std::sync::Arc::new(crate::CommitLocks::new()),
            hook,
        )
        .unwrap();
        (tmp, vr)
    }

    #[test]
    fn commit_hook_fires_on_initial_commit_with_no_parent() {
        let calls: HookCalls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_clone = std::sync::Arc::clone(&calls);
        let hook: crate::CommitHook = std::sync::Arc::new(move |p, c| {
            calls_clone.lock().unwrap().push((p, c));
        });
        let (_tmp, vr) = open_unborn_with_hook(hook);

        let res = vr
            .apply_transaction(&Transaction::new("c").create("a.md", "alpha"))
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, None, "initial commit has no parent");
        assert_eq!(calls[0].1, res.commit);
    }

    #[test]
    fn commit_hook_reports_parent_on_followup_commit() {
        let calls: HookCalls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_clone = std::sync::Arc::clone(&calls);
        let hook: crate::CommitHook = std::sync::Arc::new(move |p, c| {
            calls_clone.lock().unwrap().push((p, c));
        });
        let (_tmp, vr) = open_unborn_with_hook(hook);

        let r1 = vr
            .apply_transaction(&Transaction::new("c1").create("a.md", "v1"))
            .unwrap();
        let v1 = crate::VaultRepo::blob_oid_of(b"v1").unwrap();
        let r2 = vr
            .apply_transaction(&Transaction::new("c2").update("a.md", "v2", v1))
            .unwrap();

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], (None, r1.commit));
        assert_eq!(
            calls[1],
            (Some(r1.commit), r2.commit),
            "second commit's parent is the first commit"
        );
    }

    #[test]
    fn commit_hook_does_not_fire_on_precondition_abort() {
        let calls: CommitOnlyCalls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_clone = std::sync::Arc::clone(&calls);
        let hook: crate::CommitHook = std::sync::Arc::new(move |_p, c| {
            calls_clone.lock().unwrap().push(c);
        });
        let (_tmp, vr) = open_unborn_with_hook(hook);

        vr.apply_transaction(&Transaction::new("c").create("a.md", "v1"))
            .unwrap();

        // Stale precondition -> reconsideration domino -> no commit.
        let stale = crate::VaultRepo::blob_oid_of(b"OLD").unwrap();
        assert!(
            vr.apply_transaction(&Transaction::new("u").update("a.md", "v2", stale))
                .is_err()
        );

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "hook only fires for the successful commit");
    }

    #[test]
    fn vault_repo_without_hook_is_silent() {
        // Sanity: open_with_locks (no hook) still applies cleanly.
        let (_tmp, vr) = open_unborn();
        let r = vr.apply_transaction(&Transaction::new("c").create("a.md", "x"));
        assert!(r.is_ok());
    }
}
