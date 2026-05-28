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
use crate::locks::{CommitLocks, lock_recover};
use git2::{Oid, Repository};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A handle to the git repository backing a vault.
pub struct VaultRepo {
    repo: Repository,
    /// Shared per-worktree commit-lock registry (GWS.6). All handles to the same
    /// worktree must share one registry to serialize the commit critical section.
    commit_locks: Arc<CommitLocks>,
}

impl VaultRepo {
    /// Open the git repository whose working tree root is `vault_root`, with a
    /// **private** commit-lock registry. Use [`Self::open_with_locks`] when
    /// multiple handles (e.g. a server managing several worktrees) must share
    /// one registry.
    ///
    /// Strict: `vault_root` must be the repository root (we do not walk parent
    /// directories). Returns [`Error::NotARepo`] if there is no repo there.
    pub fn open(vault_root: &Path) -> Result<Self> {
        Self::open_with_locks(vault_root, Arc::new(CommitLocks::new()))
    }

    /// Open at `vault_root` sharing the given commit-lock registry, so handles to
    /// the same worktree serialize their commit critical sections.
    pub fn open_with_locks(vault_root: &Path, commit_locks: Arc<CommitLocks>) -> Result<Self> {
        match Repository::open(vault_root) {
            Ok(repo) => Ok(Self { repo, commit_locks }),
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                Err(Error::NotARepo(vault_root.to_path_buf()))
            }
            Err(e) => Err(Error::Git(e)),
        }
    }

    /// Clone the shared commit-lock registry — handed to a scratch worktree's
    /// `VaultRepo` (GWS.9) so all handles to the same repo's worktrees keep
    /// using one registry.
    pub fn commit_locks(&self) -> Arc<CommitLocks> {
        Arc::clone(&self.commit_locks)
    }

    /// Run `f` while holding this worktree's commit lock — the serialization
    /// boundary for the commit + checkout critical section (GWS.6/GWS.7). The
    /// key is the worktree's workdir (or the git dir for a bare repo).
    pub fn with_commit_lock<R>(&self, f: impl FnOnce() -> R) -> R {
        let key = self.worktree_key();
        let mutex = self.commit_locks.mutex_for(&key);
        let _guard = lock_recover(&mutex);
        f()
    }

    /// Identity of this worktree for commit-lock keying: the working directory,
    /// falling back to the git directory for a bare repo.
    fn worktree_key(&self) -> PathBuf {
        self.repo
            .workdir()
            .unwrap_or_else(|| self.repo.path())
            .to_path_buf()
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

    /// Borrow the underlying repository (for the plumbing layers).
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

    #[test]
    fn shared_registry_same_worktree_shares_one_mutex() {
        let tmp = TempDir::new().unwrap();
        init_unborn(tmp.path());
        let locks = Arc::new(CommitLocks::new());
        let r1 = VaultRepo::open_with_locks(tmp.path(), Arc::clone(&locks)).unwrap();
        let r2 = VaultRepo::open_with_locks(tmp.path(), Arc::clone(&locks)).unwrap();
        let m1 = r1.commit_locks.mutex_for(&r1.worktree_key());
        let m2 = r2.commit_locks.mutex_for(&r2.worktree_key());
        assert!(
            Arc::ptr_eq(&m1, &m2),
            "shared registry + same worktree -> one commit mutex"
        );
    }

    #[test]
    fn with_commit_lock_runs_closure() {
        let tmp = TempDir::new().unwrap();
        init_unborn(tmp.path());
        let vr = VaultRepo::open(tmp.path()).unwrap();
        assert_eq!(vr.with_commit_lock(|| 42), 42);
    }
}
