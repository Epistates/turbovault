//! Worktree-as-transaction / fan-out mode (GWS.9).
//!
//! `begin_fanout` opens a **scratch git worktree** on a `wip/<id>` branch
//! forked from main's current tip. All transactions applied through that
//! worktree's [`VaultRepo`] commit to the wip branch — they share the parent's
//! object DB but use a separate working tree + index. Obsidian, pointed at
//! main's working tree, stays stable for the whole fan-out.
//!
//! When the fan-out is done, `commit_fanout` merges the wip branch back into
//! main (configurable strategy) and cleans up the scratch worktree + branch.
//! `abandon_fanout` discards the fan-out (no commits land on main).
//!
//! The fan-out's worktree gets a separate per-worktree commit mutex (it's a
//! different worktree key), so transactions inside the fan-out never contend
//! with main's commit mutex.

use crate::error::{Error, Result};
use crate::repo::VaultRepo;
use git2::Oid;
use std::path::{Path, PathBuf};
use tracing::instrument;

/// How a fan-out merges back into main.
#[derive(Debug, Clone, Copy)]
pub enum MergeStrategy {
    /// `git merge --no-ff` — a merge commit on main with main's tip and the
    /// wip tip as parents. Preserves the wip branch's per-transaction commits
    /// (no squash). The default.
    MergeCommit,
    /// Advance main's ref directly to the wip tip — fails if main has moved
    /// since the fan-out began (would create a fork; use `MergeCommit` then).
    FastForward,
}

/// Result of a successful merge-back.
#[derive(Debug, Clone)]
pub struct MergeBackResult {
    /// Main's tip after the merge-back.
    pub tip_after: Oid,
    /// Main's tip before the merge-back (the CAS expected-old).
    pub tip_before: Oid,
    /// `Some(oid)` of the merge commit (`MergeCommit` strategy); `None` for a
    /// pure fast-forward (no new commit object, just a ref advance).
    pub merge_commit: Option<Oid>,
}

/// An open fan-out scratch worktree. Hold txns through `worktree_repo()`;
/// finalize via `commit_fanout` (merge back) or `abandon_fanout` (discard).
pub struct FanoutTransaction<'a> {
    main: &'a VaultRepo,
    worktree_repo: VaultRepo,
    main_branch: String,
    wip_branch: String,
    worktree_name: String,
    worktree_path: PathBuf,
    parent_tip: Oid,
}

impl<'a> FanoutTransaction<'a> {
    /// The substrate handle for transactions inside the fan-out.
    pub fn worktree_repo(&self) -> &VaultRepo {
        &self.worktree_repo
    }

    /// The wip branch the fan-out commits to (`wip/<id>`).
    pub fn wip_branch(&self) -> &str {
        &self.wip_branch
    }

    /// Main's tip when the fan-out began.
    pub fn parent_tip(&self) -> Oid {
        self.parent_tip
    }

    /// Merge the fan-out back into main and clean up the scratch worktree.
    /// On any error the fan-out artifacts may be left behind; call
    /// `abandon_fanout` to clean up explicitly.
    #[instrument(
        skip(self),
        fields(
            wip_branch = %self.wip_branch,
            main_branch = %self.main_branch,
            strategy = ?strategy,
        ),
        name = "git_commit_fanout"
    )]
    pub fn commit_fanout(self, strategy: MergeStrategy) -> Result<MergeBackResult> {
        // Serialize the merge-back on main's commit lock (same critical section
        // every other writer on main uses).
        let main = self.main;
        let result = main.with_commit_lock(|| self.merge_back(strategy));
        // Always attempt cleanup (worktree + wip branch), even on merge error,
        // so we don't leak the scratch state.
        let _ = self.cleanup_after_failure_attempt();
        result
    }

    /// Discard the fan-out: nothing lands on main; scratch worktree + wip
    /// branch removed.
    pub fn abandon_fanout(self) -> Result<()> {
        self.cleanup()
    }

    // --- internals ---

    /// `commit_fanout` consumed `self`, so cleanup runs on the borrowed copy of
    /// the fields before drop. This is a separate helper because the merge
    /// path borrows `self` and we can't call `cleanup(self)` afterward — so we
    /// inline a non-consuming variant for the failure-tolerant final step.
    fn cleanup_after_failure_attempt(&self) -> Result<()> {
        cleanup_inner(
            self.main,
            &self.wip_branch,
            &self.worktree_name,
            &self.worktree_path,
        )
    }

    fn cleanup(self) -> Result<()> {
        cleanup_inner(
            self.main,
            &self.wip_branch,
            &self.worktree_name,
            &self.worktree_path,
        )
    }

    fn merge_back(&self, strategy: MergeStrategy) -> Result<MergeBackResult> {
        let main = self.main;
        let repo = main.git();
        let wip_ref = format!("refs/heads/{}", self.wip_branch);

        let wip_tip = repo
            .refname_to_id(&wip_ref)
            .map_err(|e| Error::Other(format!("wip branch {} missing: {e}", self.wip_branch)))?;
        let main_tip_before = repo
            .refname_to_id(&self.main_branch)
            .map_err(|e| Error::Other(format!("main branch {} missing: {e}", self.main_branch)))?;

        // If the fan-out made no commits, the wip branch still points at the
        // parent tip — there is nothing to merge back. Treat as a no-op success.
        if wip_tip == self.parent_tip {
            return Ok(MergeBackResult {
                tip_after: main_tip_before,
                tip_before: main_tip_before,
                merge_commit: None,
            });
        }

        match strategy {
            MergeStrategy::FastForward => {
                // FF only works when main has not advanced since the fan-out began.
                if main_tip_before != self.parent_tip {
                    return Err(Error::Other(format!(
                        "fast-forward merge-back failed: main advanced ({} -> {}) during the \
                         fan-out; use MergeCommit instead",
                        self.parent_tip, main_tip_before
                    )));
                }
                main.cas_ref(&self.main_branch, Some(main_tip_before), wip_tip)?;
                let changed = main.paths_changed_between(main_tip_before, wip_tip)?;
                main.materialize(wip_tip, &changed)?;
                Ok(MergeBackResult {
                    tip_after: wip_tip,
                    tip_before: main_tip_before,
                    merge_commit: None,
                })
            }
            MergeStrategy::MergeCommit => {
                // Build the merged tree (3-way merge: base=parent_tip,
                // ours=main_tip_before, theirs=wip_tip). If concurrent main
                // writes don't conflict with the fan-out's changes, the merge
                // succeeds cleanly; otherwise we surface the conflict.
                let base_tree = repo.find_commit(self.parent_tip)?.tree()?;
                let ours_tree = repo.find_commit(main_tip_before)?.tree()?;
                let theirs_tree = repo.find_commit(wip_tip)?.tree()?;
                let mut idx = repo.merge_trees(&base_tree, &ours_tree, &theirs_tree, None)?;
                if idx.has_conflicts() {
                    return Err(Error::Other(format!(
                        "merge-back conflict between main ({}) and wip {} ({}); \
                         resolve manually",
                        main_tip_before, self.wip_branch, wip_tip
                    )));
                }
                let merged_tree_oid = idx.write_tree_to(repo)?;

                // Merge commit with two parents preserves wip's history.
                let message = format!(
                    "merge fan-out {} into {}",
                    self.wip_branch, self.main_branch
                );
                let merge_commit_oid =
                    main.commit_tree(merged_tree_oid, &[main_tip_before, wip_tip], &message)?;
                main.cas_ref(&self.main_branch, Some(main_tip_before), merge_commit_oid)?;

                let changed = main.paths_changed_between(main_tip_before, merge_commit_oid)?;
                main.materialize(merge_commit_oid, &changed)?;
                Ok(MergeBackResult {
                    tip_after: merge_commit_oid,
                    tip_before: main_tip_before,
                    merge_commit: Some(merge_commit_oid),
                })
            }
        }
    }
}

/// Prune the scratch worktree + delete the wip branch. Tolerant: try every
/// step; if one fails we still attempt the rest, then return the first error.
fn cleanup_inner(
    main: &VaultRepo,
    wip_branch: &str,
    worktree_name: &str,
    worktree_path: &Path,
) -> Result<()> {
    let repo = main.git();
    let mut first_err: Option<Error> = None;

    // (a) Remove the worktree's working-tree directory. Ignore NotFound (the
    // dir may already be gone, e.g. if the caller cleaned it up).
    match std::fs::remove_dir_all(worktree_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            first_err.get_or_insert(Error::Io(e));
        }
    }
    // (b) Prune the .git/worktrees/<name>/ metadata.
    match repo.find_worktree(worktree_name) {
        Ok(wt) => {
            let mut opts = git2::WorktreePruneOptions::new();
            opts.valid(true).working_tree(true).locked(true);
            if let Err(e) = wt.prune(Some(&mut opts)) {
                first_err.get_or_insert(Error::Git(e));
            }
        }
        Err(e) if e.code() == git2::ErrorCode::NotFound => {} // already pruned
        Err(e) => {
            first_err.get_or_insert(Error::Git(e));
        }
    }
    // (c) Delete the wip branch.
    match repo.find_branch(wip_branch, git2::BranchType::Local) {
        Ok(mut b) => {
            if let Err(e) = b.delete() {
                first_err.get_or_insert(Error::Git(e));
            }
        }
        Err(e) if e.code() == git2::ErrorCode::NotFound => {}
        Err(e) => {
            first_err.get_or_insert(Error::Git(e));
        }
    }
    match first_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

impl VaultRepo {
    /// Open a fan-out scratch worktree (GWS.9). Creates a wip branch
    /// `wip/<id>` at this repo's current HEAD commit, then creates a git
    /// worktree at `worktree_path` (must be OUTSIDE main's working tree —
    /// git refuses nested worktrees). Returns a [`FanoutTransaction`] whose
    /// `worktree_repo()` is the substrate handle for all txns inside the
    /// fan-out.
    ///
    /// Errors if this branch is unborn (no commit to fork from) or detached.
    #[instrument(
        skip(self),
        fields(id = %id, worktree_path = ?worktree_path),
        name = "git_begin_fanout"
    )]
    pub fn begin_fanout(&self, id: &str, worktree_path: &Path) -> Result<FanoutTransaction<'_>> {
        let main_branch = self.head_ref()?; // errors if detached
        let parent_tip = self
            .head_oid()
            .ok_or_else(|| Error::Other("cannot fan-out from an unborn branch".to_string()))?;

        let wip_branch = format!("wip/{id}");
        let worktree_name = format!("wip-{id}");

        // Create the wip branch off main's tip.
        let parent_commit = self.git().find_commit(parent_tip)?;
        let wip_branch_obj = self.git().branch(&wip_branch, &parent_commit, false)?;
        let wip_ref = wip_branch_obj.into_reference();

        // Create the git worktree on the wip branch.
        let mut opts = git2::WorktreeAddOptions::new();
        opts.reference(Some(&wip_ref));
        self.git()
            .worktree(&worktree_name, worktree_path, Some(&opts))?;

        // Open the worktree as a VaultRepo, sharing the commit-lock registry.
        let worktree_repo = VaultRepo::open_with_locks(worktree_path, self.commit_locks())?;

        Ok(FanoutTransaction {
            main: self,
            worktree_repo,
            main_branch,
            wip_branch,
            worktree_name,
            worktree_path: worktree_path.to_path_buf(),
            parent_tip,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Transaction;
    use git2::Repository;
    use tempfile::TempDir;

    /// Init main repo + apply one seed commit so main is BORN (begin_fanout
    /// requires a tip to fork from).
    fn open_born() -> (TempDir, TempDir, VaultRepo) {
        let main_dir = TempDir::new().unwrap();
        // Worktree path must live OUTSIDE main's workdir. Hold its TempDir in
        // its parent so it survives until both are dropped.
        let scratch_parent = TempDir::new().unwrap();
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(main_dir.path(), &opts).unwrap();
        let vr = VaultRepo::open(main_dir.path()).unwrap();
        vr.apply_transaction(&Transaction::new("seed").create("seed.md", "S"))
            .unwrap();
        (main_dir, scratch_parent, vr)
    }

    fn scratch_path(parent: &TempDir, id: &str) -> PathBuf {
        parent.path().join(format!("worktree-{id}"))
    }

    fn wt_read(repo: &VaultRepo, rel: &str) -> String {
        std::fs::read_to_string(repo.git().workdir().unwrap().join(rel)).unwrap()
    }

    #[test]
    fn begin_isolates_worktree_main_untouched() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "1");
        let fanout = vr.begin_fanout("1", &wt_path).unwrap();

        // The wip branch was created off main's tip.
        let main_tip = vr.head_oid().unwrap();
        assert_eq!(fanout.parent_tip(), main_tip);
        assert_eq!(fanout.wip_branch(), "wip/1");

        // Apply a txn in the fan-out — main's tip is UNCHANGED.
        fanout
            .worktree_repo()
            .apply_transaction(&Transaction::new("c").create("a.md", "alpha"))
            .unwrap();
        assert_eq!(
            vr.head_oid(),
            Some(main_tip),
            "main unchanged during fan-out"
        );
        // The worktree's working tree has the new file; main's does not.
        assert_eq!(wt_read(fanout.worktree_repo(), "a.md"), "alpha");
        assert!(!vr.git().workdir().unwrap().join("a.md").exists());

        fanout.abandon_fanout().unwrap();
    }

    #[test]
    fn commit_fanout_merge_commit_lands_on_main_with_two_parents() {
        let (_m, scratch, vr) = open_born();
        let main_tip_before = vr.head_oid().unwrap();
        let wt_path = scratch_path(&scratch, "2");
        let fanout = vr.begin_fanout("2", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .apply_transaction(&Transaction::new("c").create("a.md", "alpha"))
            .unwrap();

        let res = fanout.commit_fanout(MergeStrategy::MergeCommit).unwrap();

        let merge_oid = res.merge_commit.expect("merge commit expected");
        assert_eq!(vr.head_oid(), Some(merge_oid));
        let merge_commit = vr.git().find_commit(merge_oid).unwrap();
        assert_eq!(
            merge_commit.parent_count(),
            2,
            "merge commit has two parents"
        );
        assert_eq!(merge_commit.parent_id(0).unwrap(), main_tip_before);
        // Main's working tree now contains the fan-out's file.
        assert_eq!(wt_read(&vr, "a.md"), "alpha");
        // Scratch worktree + wip branch cleaned up.
        assert!(!wt_path.exists());
        assert!(
            vr.git()
                .find_branch("wip/2", git2::BranchType::Local)
                .is_err()
        );
    }

    #[test]
    fn commit_fanout_fast_forward_when_main_unchanged() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "3");
        let fanout = vr.begin_fanout("3", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .apply_transaction(&Transaction::new("c").create("a.md", "alpha"))
            .unwrap();

        let res = fanout.commit_fanout(MergeStrategy::FastForward).unwrap();
        assert!(res.merge_commit.is_none(), "FF makes no new commit object");
        assert_eq!(vr.head_oid(), Some(res.tip_after));
        assert_eq!(wt_read(&vr, "a.md"), "alpha");
    }

    #[test]
    fn fast_forward_fails_when_main_advanced_concurrently() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "4");
        let fanout = vr.begin_fanout("4", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .apply_transaction(&Transaction::new("c").create("a.md", "alpha"))
            .unwrap();

        // Concurrent writer on main (e.g. cross-process / Workflow B) advances
        // main while the fan-out was working.
        vr.apply_transaction(&Transaction::new("concurrent").create("c.md", "concurrent"))
            .unwrap();

        let res = fanout.commit_fanout(MergeStrategy::FastForward);
        assert!(
            matches!(res, Err(Error::Other(_))),
            "FF must refuse when main advanced"
        );
    }

    #[test]
    fn merge_commit_handles_concurrent_main_advance_disjoint() {
        let (_m, scratch, vr) = open_born();
        let wt_path = scratch_path(&scratch, "5");
        let fanout = vr.begin_fanout("5", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .apply_transaction(&Transaction::new("c").create("a.md", "alpha"))
            .unwrap();

        // Concurrent main writer touches a DISJOINT path.
        vr.apply_transaction(&Transaction::new("concurrent").create("c.md", "concurrent"))
            .unwrap();

        let res = fanout.commit_fanout(MergeStrategy::MergeCommit).unwrap();
        let merge_oid = res.merge_commit.unwrap();
        let tree = vr.git().find_commit(merge_oid).unwrap().tree_id();
        // Both changes present in the merged tree.
        assert!(vr.blob_oid_at(tree, "a.md").unwrap().is_some());
        assert!(vr.blob_oid_at(tree, "c.md").unwrap().is_some());
        // Working tree matches.
        assert_eq!(wt_read(&vr, "a.md"), "alpha");
        assert_eq!(wt_read(&vr, "c.md"), "concurrent");
    }

    #[test]
    fn abandon_leaves_main_untouched_and_cleans_up() {
        let (_m, scratch, vr) = open_born();
        let main_tip = vr.head_oid().unwrap();
        let wt_path = scratch_path(&scratch, "6");
        let fanout = vr.begin_fanout("6", &wt_path).unwrap();
        fanout
            .worktree_repo()
            .apply_transaction(&Transaction::new("c").create("a.md", "alpha"))
            .unwrap();

        fanout.abandon_fanout().unwrap();

        assert_eq!(vr.head_oid(), Some(main_tip), "main unchanged on abandon");
        assert!(!wt_path.exists(), "worktree dir removed");
        assert!(
            vr.git()
                .find_branch("wip/6", git2::BranchType::Local)
                .is_err()
        );
    }

    #[test]
    fn empty_fanout_commit_is_a_noop() {
        let (_m, scratch, vr) = open_born();
        let main_tip = vr.head_oid().unwrap();
        let wt_path = scratch_path(&scratch, "7");
        let fanout = vr.begin_fanout("7", &wt_path).unwrap();
        // No txns applied — wip tip == parent_tip.
        let res = fanout.commit_fanout(MergeStrategy::MergeCommit).unwrap();
        assert!(res.merge_commit.is_none());
        assert_eq!(res.tip_after, main_tip);
    }
}
