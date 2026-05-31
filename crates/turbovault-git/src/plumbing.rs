//! Object-DB plumbing (GWS.2): build trees in an **isolated index** and create
//! commit objects, with **no working-tree interaction**.
//!
//! The substrate stages from the batch's own bytes (not the working tree) into
//! an ephemeral `git2::Index` that is never bound to `.git/index`, seeds it from
//! a parent tree, applies the transaction's changes, and writes the tree +
//! commit to the object DB. Advancing a ref (CAS) and materializing the working
//! tree are separate, later steps (GWS.3, GWS.5).

use crate::error::{Error, Result};
use crate::repo::VaultRepo;
use git2::{Commit, Index, IndexEntry, IndexTime, Oid, Signature};
use std::path::Path;
use tracing::instrument;

/// A single change to apply to a tree. Moves are modeled at the op-mapping layer
/// (GWS.8) as `Remove(old)` + `Upsert(new)`.
#[derive(Debug, Clone)]
pub enum TreeChange {
    /// Add a new file or overwrite an existing one with `content`.
    Upsert { path: String, content: Vec<u8> },
    /// Remove a file from the tree.
    Remove { path: String },
}

impl TreeChange {
    /// The vault-relative path this change targets.
    pub fn path(&self) -> &str {
        match self {
            TreeChange::Upsert { path, .. } | TreeChange::Remove { path } => path,
        }
    }
}

impl VaultRepo {
    /// Build a tree from `base` (a parent commit's tree oid, or `None` for an
    /// empty base) applying `changes` in an **isolated in-memory index**. Blobs
    /// and the resulting tree are written to the object DB. The shared
    /// `.git/index` is never touched. Returns the new tree oid.
    #[instrument(
        skip(self, changes),
        fields(base = ?base, n_changes = changes.len()),
        name = "git_build_tree"
    )]
    pub fn build_tree(&self, base: Option<Oid>, changes: &[TreeChange]) -> Result<Oid> {
        let repo = self.git();
        let mut index = Index::new()?;
        if let Some(base_oid) = base {
            let tree = repo.find_tree(base_oid)?;
            index.read_tree(&tree)?;
        }
        for change in changes {
            match change {
                TreeChange::Upsert { path, content } => {
                    let blob = repo.blob(content)?;
                    index.add(&IndexEntry {
                        ctime: IndexTime::new(0, 0),
                        mtime: IndexTime::new(0, 0),
                        dev: 0,
                        ino: 0,
                        mode: 0o100_644,
                        uid: 0,
                        gid: 0,
                        file_size: content.len() as u32,
                        id: blob,
                        flags: 0,
                        flags_extended: 0,
                        path: path.as_bytes().to_vec(),
                    })?;
                }
                TreeChange::Remove { path } => {
                    index.remove_path(Path::new(path))?;
                }
            }
        }
        Ok(index.write_tree_to(repo)?)
    }

    /// Create a commit object from `tree` and `parents` **without moving any
    /// ref** (this is `commit-tree`, not `commit`). The ref advance is a separate
    /// CAS step (GWS.3). Returns the new commit oid.
    #[instrument(
        skip(self),
        fields(tree = %tree, n_parents = parents.len(), message = %message),
        name = "git_commit_tree"
    )]
    pub fn commit_tree(&self, tree: Oid, parents: &[Oid], message: &str) -> Result<Oid> {
        let repo = self.git();
        let sig = self.author_signature()?;
        let tree = repo.find_tree(tree)?;
        let parent_commits: Vec<Commit> = parents
            .iter()
            .map(|oid| repo.find_commit(*oid))
            .collect::<std::result::Result<_, _>>()?;
        let parent_refs: Vec<&Commit> = parent_commits.iter().collect();
        Ok(repo.commit(None, &sig, &sig, message, &tree, &parent_refs)?)
    }

    /// The blob oid at `path` in `tree`, or `None` if absent. This is the value
    /// a transaction reads as its CAS pre-image (GWS.4) and what materialization
    /// resolves to working-tree bytes (GWS.5).
    pub fn blob_oid_at(&self, tree: Oid, path: &str) -> Result<Option<Oid>> {
        let tree = self.git().find_tree(tree)?;
        match tree.get_path(Path::new(path)) {
            Ok(entry) => Ok(Some(entry.id())),
            Err(e) if e.code() == git2::ErrorCode::NotFound => Ok(None),
            Err(e) => Err(Error::Git(e)),
        }
    }

    /// Read a blob's bytes by oid.
    pub fn read_blob(&self, oid: Oid) -> Result<Vec<u8>> {
        Ok(self.git().find_blob(oid)?.content().to_vec())
    }

    /// Author/committer signature.
    ///
    /// turbovault-ov7 / TV-004: defaults to the built-in
    /// `TurboVault <turbovault@localhost>` identity so machine-authored
    /// commits are visibly distinguishable from human commits in
    /// `git log` / `git blame`. The previous behavior pulled the
    /// operator's global `user.name` / `user.email` first, muddying
    /// the audit trail and blocking "act only on bot commits"
    /// automation.
    ///
    /// Per-vault override via `VaultGitConfig::author` is the
    /// documented upgrade path (architecture §13.5); plumbing that
    /// override into the substrate is a follow-up — until then this
    /// is the single default.
    fn author_signature(&self) -> Result<Signature<'static>> {
        Ok(Signature::now("TurboVault", "turbovault@localhost")?)
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

    fn upsert(path: &str, content: &str) -> TreeChange {
        TreeChange::Upsert {
            path: path.to_string(),
            content: content.as_bytes().to_vec(),
        }
    }

    #[test]
    fn build_tree_from_empty_base() {
        let (_tmp, vr) = open_unborn();
        let t = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let oid = vr.blob_oid_at(t, "a.md").unwrap().expect("a.md present");
        assert_eq!(vr.read_blob(oid).unwrap(), b"alpha");
        assert!(vr.blob_oid_at(t, "missing.md").unwrap().is_none());
    }

    #[test]
    fn build_tree_seeds_from_base_and_adds() {
        let (_tmp, vr) = open_unborn();
        let t1 = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let t2 = vr.build_tree(Some(t1), &[upsert("b.md", "beta")]).unwrap();
        // a.md preserved, b.md added.
        assert!(vr.blob_oid_at(t2, "a.md").unwrap().is_some());
        let b = vr.blob_oid_at(t2, "b.md").unwrap().unwrap();
        assert_eq!(vr.read_blob(b).unwrap(), b"beta");
    }

    #[test]
    fn upsert_overwrites_existing() {
        let (_tmp, vr) = open_unborn();
        let t1 = vr.build_tree(None, &[upsert("a.md", "v1")]).unwrap();
        let t2 = vr.build_tree(Some(t1), &[upsert("a.md", "v2")]).unwrap();
        let oid = vr.blob_oid_at(t2, "a.md").unwrap().unwrap();
        assert_eq!(vr.read_blob(oid).unwrap(), b"v2");
    }

    #[test]
    fn remove_drops_path() {
        let (_tmp, vr) = open_unborn();
        let t1 = vr
            .build_tree(None, &[upsert("a.md", "alpha"), upsert("b.md", "beta")])
            .unwrap();
        let t2 = vr
            .build_tree(
                Some(t1),
                &[TreeChange::Remove {
                    path: "a.md".to_string(),
                }],
            )
            .unwrap();
        assert!(
            vr.blob_oid_at(t2, "a.md").unwrap().is_none(),
            "a.md removed"
        );
        assert!(vr.blob_oid_at(t2, "b.md").unwrap().is_some(), "b.md kept");
    }

    #[test]
    fn commit_tree_creates_object_without_moving_ref() {
        let (_tmp, vr) = open_unborn();
        let t1 = vr.build_tree(None, &[upsert("a.md", "alpha")]).unwrap();
        let c1 = vr.commit_tree(t1, &[], "init").unwrap();

        // The branch is still unborn: commit_tree built an object but moved no ref.
        assert!(vr.is_unborn(), "commit_tree must NOT advance any ref");
        assert_eq!(vr.head_oid(), None);

        // Parent linkage + tree content round-trip.
        let t2 = vr.build_tree(Some(t1), &[upsert("b.md", "beta")]).unwrap();
        let c2 = vr.commit_tree(t2, &[c1], "add b").unwrap();
        let commit2 = vr.git().find_commit(c2).unwrap();
        assert_eq!(commit2.parent_count(), 1);
        assert_eq!(commit2.parent_id(0).unwrap(), c1);
        let b = vr.blob_oid_at(commit2.tree_id(), "b.md").unwrap().unwrap();
        assert_eq!(vr.read_blob(b).unwrap(), b"beta");
    }
}
