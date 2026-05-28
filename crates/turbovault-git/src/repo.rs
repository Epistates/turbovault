//! Repository handle + detection (GWS.1).
//!
//! [`VaultRepo`] wraps a `git2::Repository` opened at the vault root. The git
//! write substrate is **opt-in per vault and git-gated**: a vault that is not a
//! git repo is detected here and the substrate is a no-op for it (the caller
//! falls back / refuses with a clear error).
//!
//! Resolves the current branch and HEAD across the three states the substrate
//! must handle: a normal born branch, an **unborn** branch (fresh repo, no
//! commits — the initial-commit case), and a **detached** HEAD.

use crate::error::{Error, Result};
use git2::{Oid, Repository};
use std::path::Path;

/// A handle to the git repository backing a vault.
pub struct VaultRepo {
    repo: Repository,
}

impl VaultRepo {
    /// Open the git repository whose working tree root is `vault_root`.
    ///
    /// Strict: `vault_root` must be the repository root (we do not walk parent
    /// directories). Returns [`Error::NotARepo`] if there is no repo there.
    pub fn open(vault_root: &Path) -> Result<Self> {
        match Repository::open(vault_root) {
            Ok(repo) => Ok(Self { repo }),
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                Err(Error::NotARepo(vault_root.to_path_buf()))
            }
            Err(e) => Err(Error::Git(e)),
        }
    }

    /// Whether `vault_root` is the root of a git repository.
    pub fn is_git_repo(vault_root: &Path) -> bool {
        Repository::open(vault_root).is_ok()
    }

    /// The current branch's short name (e.g. `main`).
    ///
    /// Returns `None` when HEAD is **detached** (points directly at a commit, no
    /// branch). Works for an **unborn** branch too — the name exists before the
    /// first commit.
    pub fn current_branch(&self) -> Option<String> {
        if self.repo.head_detached().unwrap_or(false) {
            return None;
        }
        let head = self.repo.find_reference("HEAD").ok()?;
        let target = head.symbolic_target()?; // e.g. "refs/heads/main"
        target.strip_prefix("refs/heads/").map(str::to_string)
    }

    /// The full ref name HEAD points at (e.g. `refs/heads/main`), even when the
    /// branch is **unborn**. Errors if HEAD is detached (no branch ref).
    pub fn head_ref(&self) -> Result<String> {
        let head = self.repo.find_reference("HEAD")?;
        head.symbolic_target()
            .map(str::to_string)
            .ok_or_else(|| Error::Other("HEAD is detached; no branch ref".to_string()))
    }

    /// The HEAD commit oid, or `None` when the branch is **unborn** (no commits).
    pub fn head_oid(&self) -> Option<Oid> {
        self.repo.head().ok()?.target()
    }

    /// Whether the current branch is unborn (a fresh repo with no commits).
    pub fn is_unborn(&self) -> bool {
        matches!(
            self.repo.head(),
            Err(ref e) if e.code() == git2::ErrorCode::UnbornBranch
        )
    }

    /// Borrow the underlying repository (for the plumbing layers, GWS.2+).
    #[allow(dead_code)] // consumed by the plumbing/CAS layers landing in GWS.2+
    pub(crate) fn git(&self) -> &Repository {
        &self.repo
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use git2::{Repository, Signature};
    use tempfile::TempDir;

    /// Init a repo with its default branch named `main` (deterministic across
    /// host git config) and no commits yet.
    fn init_unborn(dir: &Path) -> Repository {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        Repository::init_opts(dir, &opts).unwrap()
    }

    fn commit_one(repo: &Repository) -> Oid {
        let sig = Signature::now("TurboVault", "tv@localhost").unwrap();
        let tree_oid = {
            let mut idx = git2::Index::new().unwrap();
            let blob = repo.blob(b"hello").unwrap();
            idx.add(&git2::IndexEntry {
                ctime: git2::IndexTime::new(0, 0),
                mtime: git2::IndexTime::new(0, 0),
                dev: 0,
                ino: 0,
                mode: 0o100_644,
                uid: 0,
                gid: 0,
                file_size: 5,
                id: blob,
                flags: 0,
                flags_extended: 0,
                path: b"a.md".to_vec(),
            })
            .unwrap();
            idx.write_tree_to(repo).unwrap()
        };
        let tree = repo.find_tree(tree_oid).unwrap();
        repo.commit(Some("refs/heads/main"), &sig, &sig, "init", &tree, &[])
            .unwrap()
    }

    #[test]
    fn open_non_git_dir_errors() {
        let tmp = TempDir::new().unwrap();
        assert!(!VaultRepo::is_git_repo(tmp.path()));
        match VaultRepo::open(tmp.path()) {
            Err(Error::NotARepo(p)) => assert_eq!(p, tmp.path()),
            Err(e) => panic!("expected NotARepo, got error {e:?}"),
            Ok(_) => panic!("expected NotARepo, got Ok"),
        }
    }

    #[test]
    fn open_detects_repo() {
        let tmp = TempDir::new().unwrap();
        init_unborn(tmp.path());
        assert!(VaultRepo::is_git_repo(tmp.path()));
        assert!(VaultRepo::open(tmp.path()).is_ok());
    }

    #[test]
    fn unborn_branch_resolution() {
        let tmp = TempDir::new().unwrap();
        init_unborn(tmp.path());
        let vr = VaultRepo::open(tmp.path()).unwrap();

        assert!(vr.is_unborn(), "fresh repo has an unborn branch");
        assert_eq!(vr.head_oid(), None, "no commit yet -> no HEAD oid");
        assert_eq!(
            vr.current_branch().as_deref(),
            Some("main"),
            "branch name exists before the first commit"
        );
        assert_eq!(vr.head_ref().unwrap(), "refs/heads/main");
    }

    #[test]
    fn born_branch_resolution() {
        let tmp = TempDir::new().unwrap();
        let repo = init_unborn(tmp.path());
        let c1 = commit_one(&repo);
        let vr = VaultRepo::open(tmp.path()).unwrap();

        assert!(!vr.is_unborn());
        assert_eq!(vr.head_oid(), Some(c1));
        assert_eq!(vr.current_branch().as_deref(), Some("main"));
        assert_eq!(vr.head_ref().unwrap(), "refs/heads/main");
    }

    #[test]
    fn detached_head_has_no_branch() {
        let tmp = TempDir::new().unwrap();
        let repo = init_unborn(tmp.path());
        let c1 = commit_one(&repo);
        repo.set_head_detached(c1).unwrap();

        let vr = VaultRepo::open(tmp.path()).unwrap();
        assert_eq!(
            vr.head_oid(),
            Some(c1),
            "detached HEAD still resolves a commit"
        );
        assert_eq!(vr.current_branch(), None, "detached HEAD has no branch");
        assert!(vr.head_ref().is_err(), "no branch ref while detached");
    }
}
