//! Restore from git history (GWS.10).
//!
//! Git history *is* the rollback log — every prior version of every path is in
//! the object DB, content-addressed. The substrate doesn't compensate after a
//! partial apply (atomic-commit: the ref either advances or it doesn't; orphan
//! blobs GC away), so "rollback" in this model is **forward**: build a new
//! changeset that restores the affected paths to their state at some target
//! commit, and apply it as a normal commit.
//!
//! Primitives:
//! - [`VaultRepo::read_at`] — preview a path's content at a historical commit.
//! - [`VaultRepo::paths_changed_between`] — the path set the rollback tool
//!   needs (diff the commit-to-undo against its parent).
//! - [`VaultRepo::build_restore_changeset`] — assemble a [`Changeset`]
//!   that brings each given path back to its target-commit state, with the
//!   right precondition (the path's CURRENT blob at HEAD) so a concurrent
//!   change since the rollback was requested aborts loudly. Caller applies it
//!   via [`VaultRepo::commit_changeset`].
//!
//! The tool layer's `rollback_note(operation_id)` composes these: locate the
//! commit for `operation_id`, take its parent as the target, list the paths
//! it touched, and apply the restore changeset.

use crate::changeset::Changeset;
use crate::error::{Error, Result};
use crate::repo::VaultRepo;
use git2::Oid;
use std::path::Path;
use tracing::instrument;

impl VaultRepo {
    /// Read a path's bytes at a specific commit. `None` if the path is absent
    /// in that commit's tree. The bytes-level preview for the rollback UI.
    pub fn read_at(&self, commit: Oid, path: &str) -> Result<Option<Vec<u8>>> {
        let tree = self.git().find_commit(commit)?.tree()?;
        match tree.get_path(Path::new(path)) {
            Ok(entry) => Ok(Some(self.read_blob(entry.id())?)),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(Error::Git(e)),
        }
    }

    /// The set of paths whose content differs between commits `a` and `b`.
    /// For the rollback flow, pass the commit-to-undo as `b` and its parent as
    /// `a` to get exactly the paths to restore.
    pub fn paths_changed_between(&self, a: Oid, b: Oid) -> Result<Vec<String>> {
        let r = self.git();
        let a_tree = r.find_commit(a)?.tree()?;
        let b_tree = r.find_commit(b)?.tree()?;
        let diff = r.diff_tree_to_tree(Some(&a_tree), Some(&b_tree), None)?;
        let mut paths = Vec::new();
        diff.foreach(
            &mut |delta, _| {
                if let Some(p) = delta.new_file().path().or_else(|| delta.old_file().path()) {
                    paths.push(p.to_string_lossy().to_string());
                }
                true
            },
            None,
            None,
            None,
        )?;
        Ok(paths)
    }

    /// Per-path change status between two commits, or between the empty tree
    /// and `b` when `a` is `None` (the initial-commit case).
    ///
    /// Each entry is `(path, present_in_b)`:
    /// - `true`  → path was added or modified in `b` (re-index it).
    /// - `false` → path was deleted in `b` (drop it from derived indexes).
    ///
    /// Used by the GWS.14 reindex apply step, which needs to distinguish
    /// "added/modified → parse + add to graph" from "deleted → remove from
    /// graph". `paths_changed_between` collapses both into one bag, which
    /// loses the information.
    pub fn diff_path_statuses(&self, a: Option<Oid>, b: Oid) -> Result<Vec<(String, bool)>> {
        let r = self.git();
        let b_tree = r.find_commit(b)?.tree()?;
        let a_tree = match a {
            Some(oid) => Some(r.find_commit(oid)?.tree()?),
            None => None,
        };
        let diff = r.diff_tree_to_tree(a_tree.as_ref(), Some(&b_tree), None)?;

        let mut out = Vec::new();
        diff.foreach(
            &mut |delta, _| {
                let status = delta.status();
                // Pick the path that actually exists on the relevant side.
                let path = match status {
                    git2::Delta::Deleted => delta.old_file().path(),
                    _ => delta.new_file().path().or_else(|| delta.old_file().path()),
                };
                if let Some(p) = path {
                    let present_in_b = !matches!(status, git2::Delta::Deleted);
                    out.push((p.to_string_lossy().to_string(), present_in_b));
                }
                true
            },
            None,
            None,
            None,
        )?;
        Ok(out)
    }

    /// Build a changeset that restores `paths` to their state at
    /// `target_commit`, with a precondition on each path's current blob at
    /// HEAD (so a concurrent write since the restore was requested aborts the
    /// whole thing loudly).
    ///
    /// For each path:
    /// - target has it + current has a different version → `update`.
    /// - target has it + current absent → `create`.
    /// - target lacks it + current has it → `delete`.
    /// - target == current → skipped (no-op).
    ///
    /// Returns `Ok(None)` when there is nothing to do (every path is already
    /// at the target state). Errors if the branch is unborn.
    #[instrument(
        skip(self, paths, message),
        fields(target_commit = %target_commit, n_paths = paths.len()),
        name = "git_build_restore_changeset"
    )]
    pub fn build_restore_changeset(
        &self,
        target_commit: Oid,
        paths: &[String],
        message: impl Into<String>,
    ) -> Result<Option<Changeset>> {
        let head_oid = self
            .head_oid()
            .ok_or_else(|| Error::Other("cannot restore: branch is unborn".to_string()))?;
        let head_tree = self.git().find_commit(head_oid)?.tree_id();
        let target_tree = self.git().find_commit(target_commit)?.tree_id();

        let mut txn = Changeset::new(message);
        let mut any = false;
        for path in paths {
            let current = self.blob_oid_at(head_tree, path)?;
            let target = self.blob_oid_at(target_tree, path)?;
            if current == target {
                continue; // already at target state
            }
            match (current, target) {
                (Some(current_oid), Some(target_oid)) => {
                    let content = self.read_blob(target_oid)?;
                    txn = txn.update(path, content, current_oid);
                }
                (Some(current_oid), None) => {
                    txn = txn.delete(path, current_oid);
                }
                (None, Some(target_oid)) => {
                    let content = self.read_blob(target_oid)?;
                    txn = txn.create(path, content);
                }
                (None, None) => unreachable!("filtered by current == target above"),
            }
            any = true;
        }
        Ok(if any { Some(txn) } else { None })
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

    /// Commit `txn` and return the new HEAD commit oid.
    fn commit(vr: &VaultRepo, txn: Changeset) -> Oid {
        vr.commit_changeset(&txn).unwrap().commit
    }

    #[test]
    fn read_at_returns_content_or_none() {
        let (_t, vr) = open_unborn();
        let c1 = commit(&vr, Changeset::new("c").create("a.md", "v1"));
        assert_eq!(
            vr.read_at(c1, "a.md").unwrap().as_deref(),
            Some(b"v1".as_slice())
        );
        assert_eq!(vr.read_at(c1, "missing.md").unwrap(), None);
    }

    #[test]
    fn paths_changed_between_diff_two_commits() {
        let (_t, vr) = open_unborn();
        let c1 = commit(&vr, Changeset::new("c").create("a.md", "alpha"));
        let blob_a = VaultRepo::blob_oid_of(b"alpha").unwrap();
        let c2 = commit(
            &vr,
            Changeset::new("c2")
                .update("a.md", "ALPHA", blob_a)
                .create("b.md", "beta"),
        );
        let mut paths = vr.paths_changed_between(c1, c2).unwrap();
        paths.sort();
        assert_eq!(paths, vec!["a.md".to_string(), "b.md".to_string()]);
    }

    #[test]
    fn restore_updates_a_changed_path_back() {
        // Restore an updated file to its earlier content.
        let (_t, vr) = open_unborn();
        let c1 = commit(&vr, Changeset::new("c").create("a.md", "v1"));
        let blob_v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        let _c2 = commit(&vr, Changeset::new("u").update("a.md", "v2", blob_v1));

        let restore_txn = vr
            .build_restore_changeset(c1, &["a.md".to_string()], "rollback to c1")
            .unwrap()
            .expect("there IS something to restore");
        vr.commit_changeset(&restore_txn).unwrap();
        assert_eq!(read_wt(&vr, "a.md"), "v1", "restored to c1's content");
    }

    #[test]
    fn restore_recreates_a_deleted_path() {
        // The deleted-then-restored case: target has it, current does not -> create.
        let (_t, vr) = open_unborn();
        let c1 = commit(&vr, Changeset::new("c").create("a.md", "v1"));
        let blob_v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        let _c2 = commit(&vr, Changeset::new("d").delete("a.md", blob_v1));
        assert!(!workfile(&vr, "a.md").exists());

        let restore_txn = vr
            .build_restore_changeset(c1, &["a.md".to_string()], "undo delete")
            .unwrap()
            .unwrap();
        vr.commit_changeset(&restore_txn).unwrap();
        assert_eq!(read_wt(&vr, "a.md"), "v1");
    }

    #[test]
    fn restore_deletes_a_created_path() {
        // The created-then-restored case: target lacks it, current has it -> delete.
        let (_t, vr) = open_unborn();
        // Make a non-empty initial commit so we have a target commit BEFORE a.md existed.
        let c1 = commit(&vr, Changeset::new("seed").create("seed.md", "S"));
        let _c2 = commit(&vr, Changeset::new("c").create("a.md", "alpha"));
        assert!(workfile(&vr, "a.md").exists());

        let restore_txn = vr
            .build_restore_changeset(c1, &["a.md".to_string()], "undo create")
            .unwrap()
            .unwrap();
        vr.commit_changeset(&restore_txn).unwrap();
        assert!(
            !workfile(&vr, "a.md").exists(),
            "a.md absent in target, removed"
        );
    }

    #[test]
    fn restore_no_op_when_current_matches_target() {
        let (_t, vr) = open_unborn();
        let c1 = commit(&vr, Changeset::new("c").create("a.md", "v1"));
        // Path already matches target -> Ok(None).
        let result = vr
            .build_restore_changeset(c1, &["a.md".to_string()], "nothing to do")
            .unwrap();
        assert!(result.is_none(), "no-op restore returns None");
    }

    #[test]
    fn restore_full_commit_undoes_its_changes() {
        // The rollback_note flow: undo a commit by restoring every path it
        // touched to its state at the commit's parent.
        let (_t, vr) = open_unborn();
        let c1 = commit(
            &vr,
            Changeset::new("seed")
                .create("a.md", "A1")
                .create("b.md", "B1"),
        );
        let blob_a1 = VaultRepo::blob_oid_of(b"A1").unwrap();
        let blob_b1 = VaultRepo::blob_oid_of(b"B1").unwrap();
        let c2 = commit(
            &vr,
            Changeset::new("multi")
                .update("a.md", "A2", blob_a1)
                .update("b.md", "B2", blob_b1),
        );

        // To undo c2: restore the paths it touched to their state at its parent (c1).
        let paths = vr.paths_changed_between(c1, c2).unwrap();
        let restore_txn = vr
            .build_restore_changeset(c1, &paths, "rollback c2")
            .unwrap()
            .unwrap();
        vr.commit_changeset(&restore_txn).unwrap();
        assert_eq!(read_wt(&vr, "a.md"), "A1");
        assert_eq!(read_wt(&vr, "b.md"), "B1");
    }

    #[test]
    fn restore_aborts_loudly_if_path_changed_since_request() {
        // The reconsideration domino on the rollback path: if `a.md` is mutated
        // between when the rollback was prepared and when it applies, the
        // precondition (current blob) fails and the restore aborts.
        let (_t, vr) = open_unborn();
        let c1 = commit(&vr, Changeset::new("c").create("a.md", "v1"));
        let blob_v1 = VaultRepo::blob_oid_of(b"v1").unwrap();
        let _c2 = commit(&vr, Changeset::new("u").update("a.md", "v2", blob_v1));

        // Prepare the restore txn (preconditioned against current state == v2).
        let restore_txn = vr
            .build_restore_changeset(c1, &["a.md".to_string()], "rollback to c1")
            .unwrap()
            .unwrap();
        // Concurrent third write moves a.md to v3 before the restore applies.
        let blob_v2 = VaultRepo::blob_oid_of(b"v2").unwrap();
        commit(&vr, Changeset::new("u2").update("a.md", "v3", blob_v2));
        // Now applying the prepared restore must abort — precondition expects v2.
        let res = vr.commit_changeset(&restore_txn);
        assert!(matches!(res, Err(Error::PreconditionFailed { path, .. }) if path == "a.md"));
        assert_eq!(read_wt(&vr, "a.md"), "v3", "concurrent change preserved");
    }

    // -------- GWS.14: diff_path_statuses --------

    #[test]
    fn diff_path_statuses_initial_commit_treats_everything_as_added() {
        let (_t, vr) = open_unborn();
        let c = commit(
            &vr,
            Changeset::new("init")
                .create("a.md", "A")
                .create("dir/b.md", "B"),
        );
        let mut out = vr.diff_path_statuses(None, c).unwrap();
        out.sort();
        assert_eq!(
            out,
            vec![("a.md".to_string(), true), ("dir/b.md".to_string(), true)],
        );
    }

    #[test]
    fn diff_path_statuses_distinguishes_added_modified_deleted() {
        let (_t, vr) = open_unborn();
        let c1 = commit(
            &vr,
            Changeset::new("seed")
                .create("keep.md", "K")
                .create("gone.md", "G")
                .create("mod.md", "M1"),
        );
        let m1 = VaultRepo::blob_oid_of(b"M1").unwrap();
        let g = VaultRepo::blob_oid_of(b"G").unwrap();
        let c2 = commit(
            &vr,
            Changeset::new("mix")
                .create("new.md", "N")
                .update("mod.md", "M2", m1)
                .delete("gone.md", g),
        );

        let mut out = vr.diff_path_statuses(Some(c1), c2).unwrap();
        out.sort();
        assert_eq!(
            out,
            vec![
                ("gone.md".to_string(), false), // deleted
                ("mod.md".to_string(), true),   // modified
                ("new.md".to_string(), true),   // added
            ],
            "keep.md (unchanged) is NOT in the diff"
        );
    }

    #[test]
    fn diff_path_statuses_empty_for_identical_commits() {
        let (_t, vr) = open_unborn();
        let c = commit(&vr, Changeset::new("c").create("a.md", "x"));
        let out = vr.diff_path_statuses(Some(c), c).unwrap();
        assert!(out.is_empty());
    }
}
