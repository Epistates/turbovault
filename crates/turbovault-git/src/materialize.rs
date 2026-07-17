//! Working-tree materialization (GWS.5): make the working tree match a commit.
//!
//! After the ref advances (GWS.3), the working tree is stale — the substrate's
//! truth is the commit graph, and the working tree is a materialized *view* of
//! HEAD. This step writes a commit's bytes for the touched paths into the
//! working tree (atomic **temp + rename** per file; removals deleted) and syncs
//! the index to the commit's tree so `git status` stays clean.
//!
//! The operation is **idempotent**: re-running it re-writes HEAD's content, so
//! it doubles as the crash/partial-failure **resync** ("advance ref, then write
//! file" is two steps; if the second is interrupted, re-materialize). The
//! per-worktree commit mutex (GWS.6) serializes concurrent materializations,
//! which contend on the shared index.

use crate::error::{Error, Result};
use crate::repo::VaultRepo;
use git2::Oid;
use std::path::Path;
use tracing::instrument;
use uuid::Uuid;

impl VaultRepo {
    /// Refuse a commit when any touched working-tree path does not currently
    /// match the commit it is based on. This protects untracked files and
    /// unsaved/manual edits from being overwritten during materialization.
    /// Call while holding the commit lock.
    pub(crate) fn ensure_worktree_matches_commit(
        &self,
        base: Option<Oid>,
        paths: &[String],
    ) -> Result<()> {
        let repo = self.git();
        let staged_mask = git2::Status::INDEX_NEW
            | git2::Status::INDEX_MODIFIED
            | git2::Status::INDEX_DELETED
            | git2::Status::INDEX_RENAMED
            | git2::Status::INDEX_TYPECHANGE;
        if repo
            .statuses(None)?
            .iter()
            .any(|entry| entry.status().intersects(staged_mask))
        {
            return Err(Error::Other(
                "Git index contains staged changes; commit or unstage them before a TurboVault write"
                    .to_string(),
            ));
        }
        let workdir = repo
            .workdir()
            .ok_or_else(|| Error::Other("bare repository has no working tree".to_string()))?;
        let tree = match base {
            Some(oid) => Some(repo.find_commit(oid)?.tree()?),
            None => None,
        };

        for rel in paths {
            let expected = match tree.as_ref() {
                Some(tree) => match tree.get_path(Path::new(rel)) {
                    Ok(entry) => Some(repo.find_blob(entry.id())?.content().to_vec()),
                    Err(error) if error.code() == git2::ErrorCode::NotFound => None,
                    Err(error) => return Err(Error::Git(error)),
                },
                None => None,
            };
            let target = workdir.join(rel);
            let actual = match std::fs::symlink_metadata(&target) {
                Ok(metadata) if metadata.file_type().is_file() => Some(std::fs::read(&target)?),
                Ok(_) => {
                    return Err(Error::Other(format!(
                        "working-tree path '{rel}' is not a regular file; refusing to overwrite it"
                    )));
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => return Err(Error::Io(error)),
            };
            if actual != expected {
                return Err(Error::Other(format!(
                    "working-tree path '{rel}' differs from HEAD; commit, restore, or move the local change before retrying"
                )));
            }
        }
        Ok(())
    }

    /// Materialize `paths` from `commit`'s tree into the working tree, and sync
    /// the index to that tree. For each path: present in the tree → write its
    /// blob atomically (temp + rename, parent dirs created); absent → remove the
    /// working-tree file if present. Idempotent (safe to re-run as a resync).
    #[instrument(
        skip(self, paths),
        fields(commit = %commit, n_paths = paths.len()),
        name = "git_materialize"
    )]
    pub fn materialize(&self, commit: Oid, paths: &[String]) -> Result<()> {
        let repo = self.git();
        let workdir = repo
            .workdir()
            .ok_or_else(|| Error::Other("bare repository has no working tree".to_string()))?
            .to_path_buf();
        let tree = repo.find_commit(commit)?.tree()?;

        for rel in paths {
            let target = workdir.join(rel);
            match tree.get_path(Path::new(rel)) {
                Ok(entry) => {
                    let blob = repo.find_blob(entry.id())?;
                    if let Some(parent) = target.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    // Atomic per-file write: temp (unique suffix) + rename.
                    let tmp = target.with_extension(format!("tmp.{}", Uuid::new_v4()));
                    if let Err(e) = std::fs::write(&tmp, blob.content()) {
                        let _ = std::fs::remove_file(&tmp);
                        return Err(Error::Io(e));
                    }
                    if let Err(e) = std::fs::rename(&tmp, &target) {
                        let _ = std::fs::remove_file(&tmp);
                        return Err(Error::Io(e));
                    }
                }
                Err(e) if e.code() == git2::ErrorCode::NotFound => {
                    // Removed in this commit: delete the working-tree file if present.
                    if target.exists() {
                        std::fs::remove_file(&target)?;
                    }
                }
                Err(e) => return Err(Error::Git(e)),
            }
        }

        // Sync the real index to the commit's tree so working tree == index ==
        // HEAD and `git status` is clean for the touched paths.
        let mut index = repo.index()?;
        index.read_tree(&tree)?;
        index.write()?;
        Ok(())
    }

    /// Re-materialize `paths` from the current HEAD commit (the resync entry
    /// point after a crash/partial materialization). No-op if the branch is
    /// unborn (nothing committed yet).
    pub fn resync_to_head(&self, paths: &[String]) -> Result<()> {
        match self.head_oid() {
            Some(head) => self.materialize(head, paths),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plumbing::TreeChange;
    use git2::Repository;
    use tempfile::TempDir;

    const MAIN: &str = "refs/heads/main";

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

    /// Commit `changes` on top of current HEAD and advance main; returns the new tip.
    fn commit(vr: &VaultRepo, changes: &[TreeChange]) -> Oid {
        let tip = vr.head_oid();
        let base = tip.map(|c| vr.git().find_commit(c).unwrap().tree_id());
        let tree = vr.build_tree(base, changes).unwrap();
        let parents: Vec<Oid> = tip.into_iter().collect();
        let c = vr.commit_tree(tree, &parents, "c").unwrap();
        vr.cas_ref(MAIN, tip, c).unwrap();
        c
    }

    fn workfile(vr: &VaultRepo, rel: &str) -> std::path::PathBuf {
        vr.git().workdir().unwrap().join(rel)
    }

    /// Never recursively delete an unexpected working-tree directory.
    #[test]
    fn materialize_refuses_to_replace_directory_with_file() {
        let (_tmp, vr) = open_unborn();
        let c = commit(&vr, &[upsert("x", "i am a file")]);
        // Simulate a prior working-tree state where `x` was a non-empty dir.
        let target = workfile(&vr, "x");
        std::fs::create_dir_all(target.join("child")).unwrap();
        std::fs::write(target.join("child/leaf"), "stale").unwrap();

        assert!(vr.materialize(c, &["x".into()]).is_err());
        assert!(target.join("child/leaf").exists());
    }

    #[test]
    fn materialize_writes_upserts() {
        let (_tmp, vr) = open_unborn();
        let c = commit(&vr, &[upsert("a.md", "alpha"), upsert("dir/b.md", "beta")]);
        vr.materialize(c, &["a.md".into(), "dir/b.md".into()])
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(workfile(&vr, "a.md")).unwrap(),
            "alpha"
        );
        assert_eq!(
            std::fs::read_to_string(workfile(&vr, "dir/b.md")).unwrap(),
            "beta",
            "nested parent dirs created"
        );
    }

    #[test]
    fn materialize_removes_deletes() {
        let (_tmp, vr) = open_unborn();
        let c1 = commit(&vr, &[upsert("a.md", "alpha")]);
        vr.materialize(c1, &["a.md".into()]).unwrap();
        assert!(workfile(&vr, "a.md").exists());

        // Second commit removes a.md.
        let c2 = commit(
            &vr,
            &[TreeChange::Remove {
                path: "a.md".to_string(),
            }],
        );
        vr.materialize(c2, &["a.md".into()]).unwrap();
        assert!(
            !workfile(&vr, "a.md").exists(),
            "delete removed from working tree"
        );
    }

    #[test]
    fn materialize_syncs_index_clean_status() {
        let (_tmp, vr) = open_unborn();
        let c = commit(&vr, &[upsert("a.md", "alpha")]);
        vr.materialize(c, &["a.md".into()]).unwrap();
        // Index + working tree both match HEAD -> the path is CURRENT (clean).
        let status = vr.git().status_file(Path::new("a.md")).unwrap();
        assert_eq!(
            status,
            git2::Status::CURRENT,
            "no pending changes after materialize"
        );
    }

    #[test]
    fn materialize_is_idempotent() {
        let (_tmp, vr) = open_unborn();
        let c = commit(&vr, &[upsert("a.md", "alpha")]);
        vr.materialize(c, &["a.md".into()]).unwrap();
        vr.materialize(c, &["a.md".into()]).unwrap(); // again
        assert_eq!(
            std::fs::read_to_string(workfile(&vr, "a.md")).unwrap(),
            "alpha"
        );
    }

    #[test]
    fn resync_restores_clobbered_working_tree() {
        let (_tmp, vr) = open_unborn();
        let _c = commit(&vr, &[upsert("a.md", "alpha")]);
        vr.resync_to_head(&["a.md".into()]).unwrap();
        assert_eq!(
            std::fs::read_to_string(workfile(&vr, "a.md")).unwrap(),
            "alpha"
        );

        // Simulate an interrupted materialization / external clobber, then resync.
        std::fs::write(workfile(&vr, "a.md"), "CORRUPT").unwrap();
        vr.resync_to_head(&["a.md".into()]).unwrap();
        assert_eq!(
            std::fs::read_to_string(workfile(&vr, "a.md")).unwrap(),
            "alpha",
            "resync restores HEAD content"
        );
    }

    #[test]
    fn resync_unborn_is_noop() {
        let (_tmp, vr) = open_unborn();
        vr.resync_to_head(&["whatever.md".into()])
            .expect("no-op on unborn branch");
    }
}
