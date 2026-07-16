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
use fs4::fs_std::FileExt;
use git2::{Oid, Repository};
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Callback fired by [`VaultRepo::commit_changeset`] after a successful
/// commit + materialize, **inside** the commit lock. Arguments are the
/// commit's first-parent oid (or `None` for the initial commit on an
/// unborn branch) and the new commit oid.
///
/// The hook is the substrate's GWS.14 plumbing: downstream consumers
/// (the reindex queue) push the new commit onto a pending-reindex queue;
/// the actual diff + graph/search update runs out of band (lazy GSU,
/// see `git-write-substrate-architecture.md` §8.1 as refined by GWS.14).
///
/// The substrate itself does NOT touch graph or search — that would
/// smear write-substrate logic outward. The hook is the only contract.
pub type CommitHook = Arc<dyn Fn(Option<Oid>, Oid) + Send + Sync>;

/// A handle to the git repository backing a vault.
pub struct VaultRepo {
    repo: Repository,
    /// Shared per-worktree commit-lock registry (GWS.6). All handles to the same
    /// worktree must share one registry to serialize the commit critical section.
    commit_locks: Arc<CommitLocks>,
    /// Optional post-commit hook fired inside the commit lock after the
    /// changeset is materialized. Plumbed for GWS.14 lazy GSU.
    pub(crate) commit_hook: Option<CommitHook>,
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
            Ok(repo) => Ok(Self {
                repo,
                commit_locks,
                commit_hook: None,
            }),
            Err(e) if e.code() == git2::ErrorCode::NotFound => {
                Err(Error::NotARepo(vault_root.to_path_buf()))
            }
            Err(e) => Err(Error::Git(e)),
        }
    }

    /// Open the repo with both a shared commit-lock registry AND a post-commit
    /// hook. The hook fires once per successful `commit_changeset`, inside
    /// the commit lock, after materialization (GWS.14 plumbing).
    ///
    /// Multiple `VaultRepo` handles to the same worktree may install
    /// different hooks; each handle's hook fires only for changesets
    /// applied through THAT handle. The server-side pattern is to install
    /// the same hook on every cached handle for a given vault.
    pub fn open_with_locks_and_hook(
        vault_root: &Path,
        commit_locks: Arc<CommitLocks>,
        commit_hook: CommitHook,
    ) -> Result<Self> {
        let mut vr = Self::open_with_locks(vault_root, commit_locks)?;
        vr.commit_hook = Some(commit_hook);
        Ok(vr)
    }

    /// Clone the shared commit-lock registry — handed to a scratch worktree's
    /// `VaultRepo` (GWS.9) so all handles to the same repo's worktrees keep
    /// using one registry.
    pub fn commit_locks(&self) -> Arc<CommitLocks> {
        Arc::clone(&self.commit_locks)
    }

    /// Run `f` while holding both the in-process mutex and a cross-process
    /// advisory lock for this worktree. The lock spans ref CAS and working-tree
    /// materialization, preventing two TurboVault processes from interleaving
    /// checkout writes after independently successful commits.
    pub fn with_commit_lock<R>(&self, f: impl FnOnce() -> Result<R>) -> Result<R> {
        let key = self.worktree_key();
        let mutex = self.commit_locks.mutex_for(&key);
        let _guard = lock_recover(&mutex);
        let lock_path = self.repo.path().join("turbovault-write.lock");
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock_file.lock_exclusive()?;
        let result = f();
        lock_file.unlock()?;
        result
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
        let target = head.symbolic_target().ok()??; // e.g. "refs/heads/main"
        target.strip_prefix("refs/heads/").map(str::to_string)
    }

    /// The full ref name HEAD points at (e.g. `refs/heads/main`), even when the
    /// branch is **unborn**. Errors if HEAD is detached (no branch ref).
    pub fn head_ref(&self) -> Result<String> {
        let head = self.repo.find_reference("HEAD")?;
        head.symbolic_target()
            .map_err(Error::Git)?
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

    /// First-parent oid of `commit`, or `None` for a root commit (the
    /// initial commit on an unborn branch). Thin libgit2 wrapper exposed
    /// so downstream consumers (the GWS.14 reindex drainer) can resolve a
    /// commit's parent without taking a direct `git2` dep.
    pub fn git_commit_first_parent(&self, commit: Oid) -> Result<Option<Oid>> {
        let c = self.repo.find_commit(commit)?;
        Ok(c.parent_ids().next())
    }

    /// First-parent commits in `(stop_exclusive, tip]`, oldest-first.
    ///
    /// tlx.5: the out-of-band ref listener uses this to enqueue EVERY commit a
    /// multi-commit jump introduced (e.g. a `git pull` of N commits) instead of
    /// only the new tip — otherwise the drainer diffs the tip against its first
    /// parent and silently skips the intermediate commits' changes.
    ///
    /// Returns `Ok(None)` when `stop_exclusive` is set but is NOT on `tip`'s
    /// FIRST-PARENT chain — a non-ff move (force-push, branch switch) OR a stop
    /// reachable only through a merge's second parent has no clean range, so the
    /// caller falls back to best-effort (full coherence needs a restart; the
    /// §8.4 limitation). With `stop_exclusive == None`, walks the whole
    /// first-parent chain from `tip` to root.
    pub fn first_parent_range(
        &self,
        stop_exclusive: Option<Oid>,
        tip: Oid,
    ) -> Result<Option<Vec<Oid>>> {
        // Walk ONLY first parents. `graph_descendant_of` is the wrong test: it
        // is true when `stop` is reachable through a merge's SECOND parent, but
        // the drainer diffs each commit against its first parent, so a `stop` on
        // a side branch is not a clean range — the walk would run past it to
        // root and re-enqueue all of history. Reaching root without hitting
        // `stop` => fall back (None); hitting it => the bounded range.
        let mut chain = Vec::new();
        let mut cur = Some(tip);
        while let Some(c) = cur {
            if Some(c) == stop_exclusive {
                chain.reverse();
                return Ok(Some(chain));
            }
            chain.push(c);
            cur = self.git_commit_first_parent(c)?;
        }
        match stop_exclusive {
            None => {
                chain.reverse();
                Ok(Some(chain))
            }
            // `stop` is not on tip's first-parent chain — no clean range.
            Some(_) => Ok(None),
        }
    }

    /// turbovault-lri: whether `path` (repo-root-relative) is excluded by
    /// any active `.gitignore`. Thin wrapper over libgit2's
    /// `is_path_ignored`. Used by the substrate's `include_ignored`
    /// policy enforcement.
    pub fn is_path_ignored(&self, path: &str) -> Result<bool> {
        Ok(self.repo.is_path_ignored(Path::new(path))?)
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

    /// hq8: `commit_locks()` must hand back THE shared registry (scratch
    /// worktrees share it, GWS.9), not a fresh default. Kills the
    /// `Arc::new(Default::default())` mutation survivor.
    #[test]
    fn commit_locks_returns_the_shared_registry() {
        let tmp = TempDir::new().unwrap();
        init_unborn(tmp.path());
        let locks = std::sync::Arc::new(CommitLocks::new());
        let vr = VaultRepo::open_with_locks(tmp.path(), std::sync::Arc::clone(&locks)).unwrap();
        assert!(
            std::sync::Arc::ptr_eq(&vr.commit_locks(), &locks),
            "commit_locks() must return the registry the repo was opened with"
        );
    }

    /// hq8: `worktree_key()` must be the real workdir, not `PathBuf::default()`
    /// (empty) — else every worktree keys to the same lock. Kills the
    /// `Default::default()` mutation survivor.
    #[test]
    fn worktree_key_is_the_workdir_not_default() {
        let tmp = TempDir::new().unwrap();
        init_unborn(tmp.path());
        let vr = VaultRepo::open(tmp.path()).unwrap();
        let key = vr.worktree_key();
        assert!(
            !key.as_os_str().is_empty(),
            "worktree_key must not be empty"
        );
        assert_eq!(
            std::fs::canonicalize(&key).unwrap(),
            std::fs::canonicalize(tmp.path()).unwrap(),
            "worktree_key is the repo workdir"
        );
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
        assert_eq!(vr.with_commit_lock(|| Ok(42)).unwrap(), 42);
    }

    #[test]
    fn commit_lock_serializes_independent_repo_handles() {
        let tmp = TempDir::new().unwrap();
        init_unborn(tmp.path());
        let first = VaultRepo::open(tmp.path()).unwrap();
        let second = VaultRepo::open(tmp.path()).unwrap();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let holder = std::thread::spawn(move || {
            first
                .with_commit_lock(|| {
                    entered_tx.send("first").unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                })
                .unwrap();
        });
        assert_eq!(entered_rx.recv().unwrap(), "first");

        let (second_tx, second_rx) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            second
                .with_commit_lock(|| {
                    second_tx.send(()).unwrap();
                    Ok(())
                })
                .unwrap();
        });
        assert!(
            second_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "independent handle entered while the cross-process lock was held"
        );
        release_tx.send(()).unwrap();
        second_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("waiter enters after release");
        holder.join().unwrap();
        waiter.join().unwrap();
    }
}
