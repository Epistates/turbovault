//! One commit per applied plan (GWS.7).
//!
//! [`VaultRepo::commit_changeset`] runs a [`turbovault_core::ChangePlan`] — an
//! ordered set of changes plus per-path preconditions plus a commit message —
//! as **one commit**, under the worktree commit lock:
//!
//! 1. acquire the per-worktree commit lock (GWS.6);
//! 2. `commit_with_retry` on the current branch ref (GWS.3): each attempt
//!    resolves the tip to its base tree, **checks preconditions** against that
//!    base (GWS.4) — a mismatch aborts the whole plan (the reconsideration
//!    domino), no retry — folds any `Rename` changes by reading `from`'s
//!    bytes off the base tree (write-substrate-layering M2, design §6.2),
//!    then builds the new tree in an isolated index (GWS.2) and
//!    `commit-tree`s on the tip. On a ref CAS loss the tip is re-read and the
//!    attempt retried, which re-checks preconditions on the new base, so a
//!    concurrent change to one of the plan's own paths surfaces as an abort,
//!    not a silent overwrite;
//! 3. **materialize** the committed paths into the working tree (GWS.5);
//! 4. release the lock.
//!
//! Single-file writes are a degenerate one-change plan; batches and
//! move+link-updates are multi-change plans — all atomic, one commit.
//!
//! write-substrate-layering M2 / design §6.1/§6.2/§11.9: `ChangePlan` (from
//! `turbovault-core`) is the SOLE public mutation-plan type — the git-owned
//! `Changeset` builder is deleted; `commit_changeset` consumes
//! `&turbovault_core::ChangePlan` directly. This crate owns hex→`Oid`
//! parsing (at the precondition check, [`crate::occ`]) and the `Rename` fold
//! — `ChangePlan` itself stays git2-free.

use crate::error::{Error, Result};
use crate::plumbing::TreeChange;
use crate::repo::VaultRepo;
use git2::Oid;
use std::collections::BTreeSet;
use tracing::instrument;
use turbovault_core::{Change, ChangePlan};

/// Outcome of a committed changeset.
#[derive(Debug, Clone)]
pub struct ChangesetResult {
    /// The commit the branch points at. For a no-op changeset
    /// (`no_op == true`) this is the *unchanged* HEAD — nothing was committed.
    pub commit: Oid,
    /// The paths materialized into the working tree. Empty when `no_op`.
    pub paths: Vec<String>,
    /// turbovault-4nc: `true` when the changeset's changes produced a tree
    /// identical to the parent's (an idempotent / no-effect write). The
    /// substrate skipped the commit, ref CAS, materialize, and reindex hook —
    /// the working tree already matched HEAD. Preconditions are still checked
    /// first, so a stale read aborts even when the result would be identical.
    pub no_op: bool,
}

impl VaultRepo {
    /// Fold a plan's [`Change`]s into [`TreeChange`]s the object-DB plumbing
    /// understands: `Upsert`/`Remove` map straight through; `Rename` reads
    /// `from`'s bytes off `base_tree` (the CAS-resolved base, under the
    /// commit lock) and folds to `Remove { from }` + `Upsert { to, <from's
    /// bytes> }` — the git substrate is the only side that builds an
    /// in-memory tree, so it is the only side that can perform this fold
    /// (design §6.2; `core::Change::Rename` deliberately carries no content).
    fn resolve_tree_changes(
        &self,
        base_tree: Option<Oid>,
        changes: &[Change],
    ) -> Result<Vec<TreeChange>> {
        let mut out = Vec::with_capacity(changes.len());
        for c in changes {
            match c {
                Change::Upsert { path, content } => out.push(TreeChange::Upsert {
                    path: path.clone(),
                    content: content.clone(),
                }),
                Change::Remove { path } => out.push(TreeChange::Remove { path: path.clone() }),
                Change::Rename { from, to } => {
                    let found = match base_tree {
                        Some(tree) => self.blob_oid_at(tree, from)?,
                        None => None,
                    };
                    let oid = found.ok_or_else(|| {
                        Error::other(format!("rename source {from} not found in base tree"))
                    })?;
                    let content = self.read_blob(oid)?;
                    out.push(TreeChange::Remove { path: from.clone() });
                    out.push(TreeChange::Upsert {
                        path: to.clone(),
                        content,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Apply `plan` as a single commit (see the module docs for the pipeline).
    ///
    /// Aborts with a `ConcurrencyError` (via [`Error::concurrency`]) if any
    /// precondition is stale (nothing committed, working tree untouched) and
    /// with [`Error::other`] for an empty plan or duplicate change paths.
    #[instrument(
        skip(self, plan),
        fields(
            message = %plan.message,
            n_changes = plan.changes.len(),
            n_preconditions = plan.preconditions.len(),
        ),
        name = "git_commit_changeset"
    )]
    pub fn commit_changeset(&self, plan: &ChangePlan) -> Result<ChangesetResult> {
        if plan.changes.is_empty() {
            return Err(Error::other("empty plan (no changes)"));
        }
        // A path mutated twice in one plan is ambiguous — reject it. Covers
        // both endpoints of a Rename, so a Rename landing on a path another
        // change also targets is caught too.
        let mut seen = BTreeSet::new();
        for path in plan.touched_paths() {
            if !seen.insert(path.clone()) {
                return Err(Error::other(format!(
                    "duplicate change for path {path} in one plan"
                )));
            }
        }

        let refname = self.head_ref()?; // errors if HEAD is detached
        let changed = plan.touched_paths();

        self.with_commit_lock(|| {
            // `parent_at_apply` is captured INSIDE `commit_with_retry`'s
            // success closure so the post-commit hook reports the correct
            // first parent even after a CAS-rebuild loop (the parent we
            // committed against, NOT the parent at function entry, which
            // may be stale).
            let mut parent_at_apply: Option<Oid> = None;
            let committed = self.commit_with_retry(&refname, |tip| {
                parent_at_apply = tip;
                self.ensure_worktree_matches_commit(tip, &changed)?;
                let base_tree = match tip {
                    Some(c) => Some(self.git().find_commit(c)?.tree_id()),
                    None => None,
                };
                // Abort the whole plan if any precondition is stale. This
                // MUST run before the identity-tree short-circuit below: a
                // stale read aborts loudly (the reconsideration domino) even
                // when the resulting tree would be identical, because the
                // read was against a now-changed base.
                self.check_preconditions(base_tree, &plan.preconditions)?;
                let tree_changes = self.resolve_tree_changes(base_tree, &plan.changes)?;
                let tree = self.build_tree(base_tree, &tree_changes)?;
                // turbovault-4nc: identity-tree short-circuit. If the changes
                // produce a tree byte-identical to the base (an idempotent
                // rewrite, a remove of an already-absent path, ...), there is
                // nothing to commit — return `None` so `commit_with_retry`
                // skips the ref CAS. The working tree already matches HEAD, so
                // materialize + the reindex hook are skipped below too.
                if Some(tree) == base_tree {
                    return Ok(None);
                }
                let parents: Vec<Oid> = tip.into_iter().collect();
                Ok(Some(self.commit_tree(tree, &parents, &plan.message)?))
            })?;

            match committed {
                Some(commit) => {
                    // Reveal the commit to the working tree (still under the lock).
                    self.materialize(commit, &changed)?;

                    // Fire the GWS.14 reindex hook inside the commit lock so the
                    // queue observes commits in commit order (matches the order
                    // a future drainer must replay them).
                    if let Some(hook) = &self.commit_hook {
                        hook(parent_at_apply, commit);
                    }

                    Ok(ChangesetResult {
                        commit,
                        paths: changed,
                        no_op: false,
                    })
                }
                None => {
                    // Identity tree -> no commit, no CAS, no materialize, no
                    // hook. `parent_at_apply` is the tip we evaluated (whose
                    // preconditions passed); an identity tree implies a
                    // non-unborn base, so it is always `Some` here.
                    let commit = parent_at_apply.ok_or_else(|| {
                        Error::other("identity-tree no-op on an unborn branch is impossible")
                    })?;
                    Ok(ChangesetResult {
                        commit,
                        paths: Vec::new(),
                        no_op: true,
                    })
                }
            }
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
        let txn = ChangePlan::new("create a")
            .upsert("a.md", "alpha")
            .expect_absent("a.md");
        let res = vr.commit_changeset(&txn).unwrap();

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
    fn create_refuses_to_clobber_untracked_worktree_file() {
        let (_tmp, vr) = open_unborn();
        std::fs::write(workfile(&vr, "draft.md"), "local draft").unwrap();

        let result = vr.commit_changeset(
            &ChangePlan::new("create draft").create("draft.md", "generated content"),
        );

        assert!(matches!(
            result,
            Err(Error::Core(turbovault_core::Error::Other(ref message))) if message.contains("differs from HEAD")
        ));
        assert_eq!(vr.head_oid(), None, "ref did not advance");
        assert_eq!(read_wt(&vr, "draft.md"), "local draft");
    }

    #[test]
    fn update_refuses_to_clobber_dirty_worktree_file() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").create("note.md", "v1"))
            .unwrap();
        let head_before = vr.head_oid();
        let v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        std::fs::write(workfile(&vr, "note.md"), "manual edit").unwrap();

        let result =
            vr.commit_changeset(&ChangePlan::new("update").update("note.md", "v2", v1.to_string()));

        assert!(matches!(
            result,
            Err(Error::Core(turbovault_core::Error::Other(ref message))) if message.contains("differs from HEAD")
        ));
        assert_eq!(vr.head_oid(), head_before, "ref did not advance");
        assert_eq!(read_wt(&vr, "note.md"), "manual edit");
    }

    #[test]
    fn write_refuses_to_discard_unrelated_staged_change() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(
            &ChangePlan::new("seed")
                .create("a.md", "a")
                .create("b.md", "b"),
        )
        .unwrap();
        let head_before = vr.head_oid();
        std::fs::write(workfile(&vr, "b.md"), "staged b").unwrap();
        let mut index = vr.git().index().unwrap();
        index.add_path(std::path::Path::new("b.md")).unwrap();
        index.write().unwrap();

        let result = vr.commit_changeset(&ChangePlan::new("update a").upsert("a.md", "a2"));

        assert!(matches!(
            result,
            Err(Error::Core(turbovault_core::Error::Other(ref message))) if message.contains("staged changes")
        ));
        assert_eq!(vr.head_oid(), head_before);
        assert!(
            vr.git()
                .status_file(std::path::Path::new("b.md"))
                .unwrap()
                .contains(git2::Status::INDEX_MODIFIED),
            "the caller's staged change remains staged"
        );
    }

    #[test]
    fn update_with_correct_precondition_succeeds() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("c").upsert("a.md", "v1"))
            .unwrap();

        let v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        let txn = ChangePlan::new("update a")
            .upsert("a.md", "v2")
            .expect_blob("a.md", v1.to_string());
        vr.commit_changeset(&txn).unwrap();
        assert_eq!(read_wt(&vr, "a.md"), "v2");
    }

    #[test]
    fn stale_precondition_aborts_nothing_applied() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("c").upsert("a.md", "v1"))
            .unwrap();
        let head_before = vr.head_oid();

        // Caller thinks a.md still holds "stale" content.
        let stale = VaultRepo::blob_oid_of(b"stale").unwrap();
        let txn = ChangePlan::new("bad update")
            .upsert("a.md", "v2")
            .expect_blob("a.md", stale.to_string());
        assert!(matches!(
            vr.commit_changeset(&txn),
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { .. }))
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
        let txn = ChangePlan::new("batch")
            .upsert("a.md", "A")
            .upsert("dir/b.md", "B")
            .remove("ghost.md"); // remove of absent path is a no-op in the tree
        let res = vr.commit_changeset(&txn).unwrap();

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
        vr.commit_changeset(&ChangePlan::new("seed").upsert("b.md", "B1"))
            .unwrap();
        let head_before = vr.head_oid();

        let stale_b = VaultRepo::blob_oid_of(b"B-OLD").unwrap();
        let txn = ChangePlan::new("write a, guard b")
            .upsert("a.md", "A")
            .expect_blob("b.md", stale_b.to_string());
        assert!(matches!(
            vr.commit_changeset(&txn),
            Err(Error::Core(turbovault_core::Error::ConcurrencyError { ref reason })) if reason.contains("b.md")
        ));
        assert_eq!(vr.head_oid(), head_before, "nothing committed");
        assert!(!workfile(&vr, "a.md").exists(), "a.md never materialized");
    }

    #[test]
    fn empty_changeset_rejected() {
        let (_tmp, vr) = open_unborn();
        assert!(matches!(
            vr.commit_changeset(&ChangePlan::new("empty")),
            Err(Error::Core(turbovault_core::Error::Other(_)))
        ));
    }

    #[test]
    fn duplicate_change_path_rejected() {
        let (_tmp, vr) = open_unborn();
        let txn = ChangePlan::new("dup")
            .upsert("a.md", "x")
            .upsert("a.md", "y");
        assert!(matches!(
            vr.commit_changeset(&txn),
            Err(Error::Core(turbovault_core::Error::Other(_)))
        ));
    }

    #[test]
    fn move_as_remove_plus_upsert_one_commit() {
        // The move+links shape (GWS.8 will build these): remove old + add new in
        // one atomic commit.
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").upsert("old.md", "body"))
            .unwrap();

        let txn = ChangePlan::new("move old->new")
            .remove("old.md")
            .upsert("new.md", "body");
        let res = vr.commit_changeset(&txn).unwrap();

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
        vr.commit_changeset(&ChangePlan::new("c").create("a.md", "alpha"))
            .unwrap();
        assert_eq!(read_wt(&vr, "a.md"), "alpha");

        // create on an existing path fails (expect_absent precondition).
        let res = vr.commit_changeset(&ChangePlan::new("c2").create("a.md", "again"));
        assert!(
            matches!(res, Err(Error::Core(turbovault_core::Error::ConcurrencyError { reason })) if reason.contains("a.md"))
        );
        assert_eq!(
            read_wt(&vr, "a.md"),
            "alpha",
            "no overwrite on create-existing"
        );
    }

    #[test]
    fn update_requires_correct_expected_blob() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").create("a.md", "v1"))
            .unwrap();

        let v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        vr.commit_changeset(&ChangePlan::new("u").update("a.md", "v2", v1.to_string()))
            .unwrap();
        assert_eq!(read_wt(&vr, "a.md"), "v2");

        // Update with stale expected (still v1, but file is now v2) aborts.
        let res =
            vr.commit_changeset(&ChangePlan::new("u-stale").update("a.md", "v3", v1.to_string()));
        assert!(
            matches!(res, Err(Error::Core(turbovault_core::Error::ConcurrencyError { reason })) if reason.contains("a.md"))
        );
        assert_eq!(read_wt(&vr, "a.md"), "v2", "stale update did not apply");
    }

    #[test]
    fn delete_requires_correct_expected_blob() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").create("a.md", "v1"))
            .unwrap();

        // Stale expected -> abort, file still there.
        let stale = VaultRepo::blob_oid_of(b"OLD").unwrap();
        let res =
            vr.commit_changeset(&ChangePlan::new("d-stale").delete("a.md", stale.to_string()));
        assert!(
            matches!(res, Err(Error::Core(turbovault_core::Error::ConcurrencyError { reason })) if reason.contains("a.md"))
        );
        assert!(workfile(&vr, "a.md").exists(), "stale delete did not apply");

        // Correct expected -> file gone.
        let v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        vr.commit_changeset(&ChangePlan::new("d").delete("a.md", v1.to_string()))
            .unwrap();
        assert!(!workfile(&vr, "a.md").exists());
    }

    // -------- Rename (design §6.2 fold; deliverable D / 9n6 primitive) --------

    #[test]
    fn rename_atomically_moves_content_in_one_commit() {
        // The valid-rename half of the 9n6 primitive: to's ExpectAbsent
        // precondition passes, so commit_changeset folds Rename into
        // remove(from)+upsert(to) and lands both in one commit — the fold
        // reads from's bytes off the base tree itself (no caller-supplied
        // content).
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").create("old.md", "body"))
            .unwrap();

        let from_blob = VaultRepo::blob_oid_of(b"body").unwrap();
        let res = vr
            .commit_changeset(&ChangePlan::new("rn").rename(
                "old.md",
                "new.md",
                from_blob.to_string(),
            ))
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
        vr.commit_changeset(&ChangePlan::new("seed").create("old.md", "body"))
            .unwrap();

        let stale = VaultRepo::blob_oid_of(b"different").unwrap();
        let res = vr.commit_changeset(&ChangePlan::new("rn").rename(
            "old.md",
            "new.md",
            stale.to_string(),
        ));
        assert!(
            matches!(res, Err(Error::Core(turbovault_core::Error::ConcurrencyError { reason })) if reason.contains("old.md"))
        );
        assert!(workfile(&vr, "old.md").exists(), "source kept on abort");
        assert!(
            !workfile(&vr, "new.md").exists(),
            "destination not written on abort"
        );
    }

    #[test]
    fn rename_aborts_when_destination_clobber_guard_trips() {
        // The 9n6 primitive: ChangePlan::rename already emits (to,
        // ExpectAbsent) at the builder level (M1) — this proves
        // commit_changeset ENFORCES it. from is kept, to is untouched,
        // nothing applied.
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(
            &ChangePlan::new("seed")
                .create("old.md", "body")
                .create("new.md", "occupied"),
        )
        .unwrap();

        let from_blob = VaultRepo::blob_oid_of(b"body").unwrap();
        let res = vr.commit_changeset(&ChangePlan::new("rn").rename(
            "old.md",
            "new.md",
            from_blob.to_string(),
        ));
        assert!(
            matches!(res, Err(Error::Core(turbovault_core::Error::ConcurrencyError { reason })) if reason.contains("new.md"))
        );
        assert!(workfile(&vr, "old.md").exists());
        assert_eq!(read_wt(&vr, "new.md"), "occupied", "destination untouched");
    }

    #[test]
    fn rename_chained_with_link_updates_is_one_commit() {
        // Move + update-links: rename old->new AND fix link targets in two other
        // files, all in one atomic commit (the case the direct batch couldn't).
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(
            &ChangePlan::new("seed")
                .create("old.md", "body")
                .create("link1.md", "see [[old]]")
                .create("link2.md", "ref [[old]] here"),
        )
        .unwrap();

        let body_blob = VaultRepo::blob_oid_of(b"body").unwrap();
        let l1_blob = VaultRepo::blob_oid_of(b"see [[old]]").unwrap();
        let l2_blob = VaultRepo::blob_oid_of(b"ref [[old]] here").unwrap();
        let res = vr
            .commit_changeset(
                &ChangePlan::new("mv+links")
                    .rename("old.md", "new.md", body_blob.to_string())
                    .update("link1.md", "see [[new]]", l1_blob.to_string())
                    .update("link2.md", "ref [[new]] here", l2_blob.to_string()),
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
            .commit_changeset(&ChangePlan::new("c").create("a.md", "alpha"))
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
            .commit_changeset(&ChangePlan::new("c1").create("a.md", "v1"))
            .unwrap();
        let v1 = crate::VaultRepo::blob_oid_of(b"v1").unwrap();
        let r2 = vr
            .commit_changeset(&ChangePlan::new("c2").update("a.md", "v2", v1.to_string()))
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

        vr.commit_changeset(&ChangePlan::new("c").create("a.md", "v1"))
            .unwrap();

        // Stale precondition -> reconsideration domino -> no commit.
        let stale = crate::VaultRepo::blob_oid_of(b"OLD").unwrap();
        assert!(
            vr.commit_changeset(&ChangePlan::new("u").update("a.md", "v2", stale.to_string()))
                .is_err()
        );

        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "hook only fires for the successful commit");
    }

    #[test]
    fn vault_repo_without_hook_is_silent() {
        // Sanity: open_with_locks (no hook) still applies cleanly.
        let (_tmp, vr) = open_unborn();
        let r = vr.commit_changeset(&ChangePlan::new("c").create("a.md", "x"));
        assert!(r.is_ok());
    }

    // -------- turbovault-uag: mutation-testing survivors (cargo-mutants) --------

    /// `commit_changeset`'s materialize call must cover BOTH endpoints of a
    /// `Rename`, not just `from` (write-substrate-layering M2, design §6.2) —
    /// a mutant that dropped `ChangePlan::touched_paths`' Rename arm (tested
    /// directly in `turbovault-core`) would leave `to` un-materialized here.
    #[test]
    fn rename_materializes_both_endpoints() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").create("old.md", "body"))
            .unwrap();
        let from_blob = VaultRepo::blob_oid_of(b"body").unwrap();
        let res = vr
            .commit_changeset(&ChangePlan::new("rn").rename(
                "old.md",
                "new.md",
                from_blob.to_string(),
            ))
            .unwrap();
        assert_eq!(res.paths, vec!["old.md".to_string(), "new.md".to_string()]);
    }

    /// The raw escape hatches `with_change` / `with_precondition` must actually
    /// register the change / precondition (surviving mutants dropped them).
    #[test]
    fn raw_with_change_and_with_precondition_take_effect() {
        let (_tmp, vr) = open_unborn();
        // with_change(Upsert) lands the file.
        let txn = ChangePlan::new("raw").with_change(Change::Upsert {
            path: "x.md".into(),
            content: b"hi".to_vec(),
        });
        assert_eq!(txn.changes.len(), 1, "with_change must register the change");
        vr.commit_changeset(&txn).unwrap();
        assert_eq!(read_wt(&vr, "x.md"), "hi");
        // with_precondition(x.md, expect_absent) on an EXISTING path must
        // abort — and specifically on the PRECONDITION, not because a
        // dropped builder left an empty plan (the plan carries a real
        // upsert, so an empty-plan error would mean with_precondition
        // discarded the chain).
        let blocked = ChangePlan::new("b")
            .upsert("y.md", b"y".to_vec())
            .with_precondition("x.md", turbovault_core::Precondition::ExpectAbsent);
        assert_eq!(
            blocked.changes.len(),
            1,
            "with_precondition must preserve the builder chain"
        );
        let err = vr.commit_changeset(&blocked).unwrap_err().to_string();
        assert!(
            !err.contains("empty"),
            "must abort on the precondition, not because the txn was emptied: {err}"
        );
    }

    /// `git_commit_first_parent` must resolve the real parent chain (a survivor
    /// replaced it with `Ok(None)`); the reindex drainer depends on it.
    #[test]
    fn git_commit_first_parent_resolves_chain() {
        let (_tmp, vr) = open_unborn();
        let r1 = vr
            .commit_changeset(&ChangePlan::new("c1").create("a.md", "1"))
            .unwrap();
        let v1 = crate::VaultRepo::blob_oid_of(b"1").unwrap();
        let r2 = vr
            .commit_changeset(&ChangePlan::new("c2").update("a.md", "2", v1.to_string()))
            .unwrap();
        assert_eq!(
            vr.git_commit_first_parent(r2.commit).unwrap(),
            Some(r1.commit),
            "c2's first parent is c1"
        );
        assert_eq!(
            vr.git_commit_first_parent(r1.commit).unwrap(),
            None,
            "the root commit has no parent"
        );
    }

    /// tlx.5: `first_parent_range` must return EVERY commit a multi-commit jump
    /// introduced (oldest-first), and `None` on a non-fast-forward target so the
    /// ref listener falls back to tip-only.
    #[test]
    fn first_parent_range_walks_the_chain() {
        let (_tmp, vr) = open_unborn();
        let c1 = vr
            .commit_changeset(&ChangePlan::new("c1").create("a.md", "1"))
            .unwrap()
            .commit;
        let v1 = crate::VaultRepo::blob_oid_of(b"1").unwrap();
        let c2 = vr
            .commit_changeset(&ChangePlan::new("c2").update("a.md", "2", v1.to_string()))
            .unwrap()
            .commit;
        let v2 = crate::VaultRepo::blob_oid_of(b"2").unwrap();
        let c3 = vr
            .commit_changeset(&ChangePlan::new("c3").update("a.md", "3", v2.to_string()))
            .unwrap()
            .commit;

        // (c1, c3] = [c2, c3], oldest-first.
        assert_eq!(
            vr.first_parent_range(Some(c1), c3).unwrap(),
            Some(vec![c2, c3])
        );
        // No stop = the whole chain back to root.
        assert_eq!(
            vr.first_parent_range(None, c3).unwrap(),
            Some(vec![c1, c2, c3])
        );
        // stop == tip = empty range (nothing new).
        assert_eq!(vr.first_parent_range(Some(c3), c3).unwrap(), Some(vec![]));
        // Non-ff: a stop that does NOT precede the tip (c1 doesn't descend from
        // c3) has no clean range -> None -> caller falls back to tip-only.
        assert_eq!(vr.first_parent_range(Some(c3), c1).unwrap(), None);
    }

    /// hq8: a `stop` reachable from `tip` ONLY through a merge's SECOND parent
    /// is not on the first-parent chain — `first_parent_range` must return None
    /// (fallback), NOT walk past it to root and re-enqueue all of history (the
    /// graph_descendant_of bug coderabbit caught).
    #[test]
    fn first_parent_range_falls_back_on_merge_second_parent() {
        let (_tmp, vr) = open_unborn();
        let c1 = vr
            .commit_changeset(&ChangePlan::new("c1").create("a.md", "1"))
            .unwrap()
            .commit;
        let v1 = crate::VaultRepo::blob_oid_of(b"1").unwrap();
        let c2 = vr
            .commit_changeset(&ChangePlan::new("c2").update("a.md", "2", v1.to_string()))
            .unwrap()
            .commit;
        // f1: a side-branch commit off c1 (same tree, distinct commit).
        let c1_tree = vr.git().find_commit(c1).unwrap().tree_id();
        let f1 = vr.commit_tree(c1_tree, &[c1], "f1").unwrap();
        // m: a merge whose FIRST parent is c2 and SECOND parent is f1.
        let c2_tree = vr.git().find_commit(c2).unwrap().tree_id();
        let m = vr.commit_tree(c2_tree, &[c2, f1], "m").unwrap();

        // f1 is reachable from m only via the 2nd parent -> not on the
        // first-parent chain -> None (fallback), not the whole history.
        assert_eq!(vr.first_parent_range(Some(f1), m).unwrap(), None);
        // sanity: a stop ON the first-parent chain still yields the range.
        assert_eq!(
            vr.first_parent_range(Some(c1), m).unwrap(),
            Some(vec![c2, m])
        );
    }

    /// `is_path_ignored` must honor `.gitignore` (survivors hard-coded
    /// `Ok(false)` / `Ok(true)`); the substrate's include_ignored gate uses it.
    #[test]
    fn is_path_ignored_honors_gitignore() {
        let (tmp, vr) = open_unborn();
        std::fs::write(tmp.path().join(".gitignore"), "*.tmp\n").unwrap();
        assert!(
            vr.is_path_ignored("scratch.tmp").unwrap(),
            "*.tmp must be ignored"
        );
        assert!(
            !vr.is_path_ignored("note.md").unwrap(),
            "note.md must not be ignored"
        );
    }

    /// turbovault-xw4: a move-shaped multi-file txn (remove old + new blob + a
    /// linker rewrite) must abort ATOMICALLY when ANY participant's precondition
    /// is stale — here the LINKER (not the source). Prior coverage only staled
    /// the source. Zero files change.
    #[test]
    fn multi_file_move_aborts_atomically_when_a_linker_is_stale() {
        let (tmp, vr) = open_unborn();
        vr.commit_changeset(
            &ChangePlan::new("seed")
                .create("old.md", "# Old")
                .create("linker.md", "[[old]]"),
        )
        .unwrap();
        let old_blob = crate::VaultRepo::blob_oid_of(b"# Old").unwrap();
        let stale = crate::VaultRepo::blob_oid_of(b"DIFFERENT").unwrap();
        let head_before = vr.head_oid().unwrap();

        let txn = ChangePlan::new("move")
            .remove("old.md")
            .upsert("new.md", b"# Old".to_vec())
            .expect_blob("old.md", old_blob.to_string())
            .upsert("linker.md", b"[[new]]".to_vec())
            .expect_blob("linker.md", stale.to_string()); // stale linker precondition
        assert!(
            vr.commit_changeset(&txn).is_err(),
            "a stale linker must abort the whole move"
        );
        assert_eq!(vr.head_oid(), Some(head_before), "nothing committed");
        assert_eq!(read_wt(&vr, "old.md"), "# Old", "old.md untouched");
        assert!(
            !tmp.path().join("new.md").exists(),
            "new.md must not have been created"
        );
    }

    /// turbovault-xw4 / PERF-2: identity-tree elision is per-TREE, not
    /// per-change. A txn mixing a no-op sub-change (rewrite a.md with identical
    /// bytes) with a REAL change (create b.md) must still commit — never be
    /// elided as a no-op.
    #[test]
    fn batch_commits_real_change_despite_an_identity_subchange() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").create("a.md", "v1"))
            .unwrap();
        let head_before = vr.head_oid().unwrap();
        let v1 = crate::VaultRepo::blob_oid_of(b"v1").unwrap();

        let res = vr
            .commit_changeset(
                &ChangePlan::new("mixed")
                    .update("a.md", "v1", v1.to_string()) // identity for a.md
                    .create("b.md", "B"), // real change
            )
            .unwrap();
        assert!(!res.no_op, "a txn carrying a real change is not a no-op");
        assert_ne!(vr.head_oid(), Some(head_before), "HEAD advanced");
        assert_eq!(read_wt(&vr, "b.md"), "B");
        assert_eq!(read_wt(&vr, "a.md"), "v1");
    }

    // -------- turbovault-4nc: identity-tree no-op short-circuit --------

    #[test]
    fn identity_tree_write_is_noop() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").create("a.md", "v1"))
            .unwrap();
        let head_before = vr.head_oid();

        // Rewrite a.md with the SAME content + correct precondition: the
        // resulting tree is identical to the base -> no-op.
        let v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        let res = vr
            .commit_changeset(&ChangePlan::new("idempotent").update("a.md", "v1", v1.to_string()))
            .unwrap();

        assert!(res.no_op, "identity rewrite is a no-op");
        assert!(res.paths.is_empty(), "no paths materialized on a no-op");
        assert_eq!(vr.head_oid(), head_before, "HEAD did not advance");
        assert_eq!(
            res.commit,
            head_before.unwrap(),
            "result.commit is the unchanged HEAD"
        );
        assert_eq!(read_wt(&vr, "a.md"), "v1", "working tree unchanged");
    }

    #[test]
    fn noop_skips_commit_hook() {
        let calls: CommitOnlyCalls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls_clone = std::sync::Arc::clone(&calls);
        let hook: crate::CommitHook = std::sync::Arc::new(move |_p, c| {
            calls_clone.lock().unwrap().push(c);
        });
        let (_tmp, vr) = open_unborn_with_hook(hook);

        vr.commit_changeset(&ChangePlan::new("seed").create("a.md", "v1"))
            .unwrap();
        let v1 = crate::VaultRepo::blob_oid_of(b"v1").unwrap();
        let res = vr
            .commit_changeset(&ChangePlan::new("idempotent").update("a.md", "v1", v1.to_string()))
            .unwrap();

        assert!(res.no_op);
        let calls = calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "hook fires for the seed commit only, never for the no-op"
        );
    }

    #[test]
    fn stale_precondition_aborts_before_identity_shortcircuit() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").create("a.md", "v1"))
            .unwrap();
        let head_before = vr.head_oid();

        // Same content (the tree WOULD be identical) but a stale precondition:
        // the abort must win — preconditions are checked before the identity
        // short-circuit, so a stale read never silently passes as a no-op.
        let stale = VaultRepo::blob_oid_of(b"WRONG").unwrap();
        let res = vr.commit_changeset(&ChangePlan::new("idempotent-but-stale").update(
            "a.md",
            "v1",
            stale.to_string(),
        ));
        assert!(
            matches!(res, Err(Error::Core(turbovault_core::Error::ConcurrencyError { ref reason })) if reason.contains("a.md")),
            "stale precondition aborts even when the tree would be identical: {res:?}"
        );
        assert_eq!(vr.head_oid(), head_before, "nothing committed on abort");
    }

    #[test]
    fn remove_absent_path_alone_is_noop() {
        let (_tmp, vr) = open_unborn();
        vr.commit_changeset(&ChangePlan::new("seed").create("a.md", "v1"))
            .unwrap();
        let head_before = vr.head_oid();

        // Removing a path that isn't in the tree leaves the tree unchanged.
        let res = vr
            .commit_changeset(&ChangePlan::new("rm ghost").remove("ghost.md"))
            .unwrap();
        assert!(res.no_op, "removing an absent path is a no-op");
        assert!(res.paths.is_empty());
        assert_eq!(vr.head_oid(), head_before, "HEAD unchanged");
    }
}
