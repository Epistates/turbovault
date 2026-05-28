//! Backend-dispatching write surface (GWS.12).
//!
//! [`WriteTools`] wraps either the legacy [`FileTools`] + [`BatchTools`] pair
//! or the git-backed [`GitFileTools`], chosen at construction from a vault's
//! [`turbovault_core::config::WriteBackend`]. The MCP layer holds one
//! `WriteTools` per vault and never branches on the backend itself.
//!
//! **Lifecycle:** this enum exists for the parallel window — Phase 2 of the
//! git-substrate cutover (GWS.12 → GWS.15). At cutover (GWS.15) the `Legacy`
//! arm is deleted, `WriteTools` collapses to bare `GitFileTools`, and the
//! type either disappears or becomes a thin alias.

use crate::batch_tools::BatchTools;
use crate::file_tools::{FileTools, NoteInfo, WriteMode};
use crate::git_file_tools::{CasCollisionFlush, GitFileTools};
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_batch::{BatchOperation, BatchResult};
use turbovault_core::prelude::*;
use turbovault_git::{CommitHook, CommitLocks};
use turbovault_vault::{EditResult, VaultManager};

/// Per-vault write surface. One dispatch site per method; the MCP layer is
/// backend-agnostic.
#[derive(Clone)]
pub enum WriteTools {
    /// Pre-cutover `VaultManager` mutators + `BatchExecutor`. Deletion target
    /// at GWS.15.
    Legacy { files: FileTools, batch: BatchTools },
    /// `turbovault-git` substrate — every change is a commit.
    Git(GitFileTools),
}

impl WriteTools {
    /// Construct the legacy dispatch wrapping the existing `VaultManager`-backed
    /// tools.
    pub fn legacy(manager: Arc<VaultManager>) -> Self {
        Self::Legacy {
            files: FileTools::new(Arc::clone(&manager)),
            batch: BatchTools::new(manager),
        }
    }

    /// Construct the git-backed dispatch. `manager` is shared with the read
    /// path; `vault_path` + `commit_locks` open a `VaultRepo` per call
    /// (libgit2 is `!Sync`; see `GitFileTools` for why).
    pub fn git(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
    ) -> Self {
        Self::Git(GitFileTools::new(manager, vault_path, commit_locks))
    }

    /// Git-backed dispatch WITH a GWS.14 reindex hook installed on every
    /// per-call `VaultRepo`. The MCP server uses this; bare `Self::git`
    /// stays for tests / migrations that don't run the reindex stack.
    pub fn git_with_hook(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
        commit_hook: CommitHook,
    ) -> Self {
        Self::Git(GitFileTools::new_with_hook(
            manager,
            vault_path,
            commit_locks,
            commit_hook,
        ))
    }

    /// Git-backed dispatch with reindex hook AND CAS-collision flush
    /// (GWS.14b). The flush runs before `apply_txn` returns a
    /// `ConcurrencyError`, so the agent's re-read sees coherent derived
    /// state.
    pub fn git_with_hook_and_flush(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
        commit_hook: CommitHook,
        flush_on_collision: CasCollisionFlush,
    ) -> Self {
        Self::Git(GitFileTools::new_with_hook_and_flush(
            manager,
            vault_path,
            commit_locks,
            commit_hook,
            flush_on_collision,
        ))
    }

    // -------- Reads (forwarded; both backends use working-tree bytes) --------

    pub async fn read_file(&self, path: &str) -> Result<String> {
        match self {
            Self::Legacy { files, .. } => files.read_file(path).await,
            Self::Git(g) => g.read_file(path).await,
        }
    }

    pub async fn get_notes_info(&self, paths: &[String]) -> Result<Vec<NoteInfo>> {
        match self {
            Self::Legacy { files, .. } => files.get_notes_info(paths).await,
            Self::Git(g) => g.get_notes_info(paths).await,
        }
    }

    // -------- Writes --------

    pub async fn write_file_with_mode(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        match self {
            Self::Legacy { files, .. } => {
                files
                    .write_file_with_mode(path, content, mode, expected_hash)
                    .await
            }
            Self::Git(g) => {
                g.write_file_with_mode(path, content, mode, expected_hash)
                    .await
            }
        }
    }

    pub async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        match self {
            Self::Legacy { files, .. } => files.write_file(path, content).await,
            Self::Git(g) => g.write_file(path, content).await,
        }
    }

    pub async fn edit_file(
        &self,
        path: &str,
        edits: &str,
        expected_hash: Option<&str>,
        dry_run: bool,
    ) -> Result<EditResult> {
        match self {
            Self::Legacy { files, .. } => {
                files.edit_file(path, edits, expected_hash, dry_run).await
            }
            Self::Git(g) => g.edit_file(path, edits, expected_hash, dry_run).await,
        }
    }

    pub async fn delete_file(&self, path: &str) -> Result<()> {
        match self {
            Self::Legacy { files, .. } => files.delete_file(path).await,
            Self::Git(g) => g.delete_file(path).await,
        }
    }

    pub async fn delete_file_with_hash(
        &self,
        path: &str,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        match self {
            Self::Legacy { files, .. } => files.delete_file_with_hash(path, expected_hash).await,
            Self::Git(g) => g.delete_file_with_hash(path, expected_hash).await,
        }
    }

    pub async fn move_file(&self, from: &str, to: &str) -> Result<()> {
        match self {
            Self::Legacy { files, .. } => files.move_file(from, to).await,
            Self::Git(g) => g.move_file(from, to).await,
        }
    }

    pub async fn move_file_with_hash(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        match self {
            Self::Legacy { files, .. } => files.move_file_with_hash(from, to, expected_hash).await,
            Self::Git(g) => g.move_file_with_hash(from, to, expected_hash).await,
        }
    }

    pub async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        match self {
            Self::Legacy { files, .. } => files.copy_file(from, to).await,
            Self::Git(g) => g.copy_file(from, to).await,
        }
    }

    pub async fn batch_execute(&self, operations: Vec<BatchOperation>) -> Result<BatchResult> {
        match self {
            Self::Legacy { batch, .. } => batch.batch_execute(operations).await,
            Self::Git(g) => g.batch_execute(operations).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use turbovault_core::config::{ServerConfig, VaultConfig};
    use turbovault_vault::VaultManager;

    fn test_server_config(vault_dir: &std::path::Path, name: &str) -> ServerConfig {
        let mut cfg = ServerConfig::new();
        cfg.vaults
            .push(VaultConfig::builder(name, vault_dir).build().unwrap());
        cfg
    }

    async fn legacy_tools(tmp: &TempDir) -> WriteTools {
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path(), "l")).unwrap());
        WriteTools::legacy(manager)
    }

    async fn git_tools(tmp: &TempDir) -> WriteTools {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        git2::Repository::init_opts(tmp.path(), &opts).unwrap();
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path(), "g")).unwrap());
        let locks = Arc::new(CommitLocks::new());
        WriteTools::git(manager, tmp.path().to_path_buf(), locks)
    }

    #[tokio::test]
    async fn legacy_dispatch_writes_and_reads_back() {
        let tmp = TempDir::new().unwrap();
        let tools = legacy_tools(&tmp).await;
        tools.write_file("a.md", "alpha").await.unwrap();
        assert_eq!(tools.read_file("a.md").await.unwrap(), "alpha");
    }

    #[tokio::test]
    async fn git_dispatch_writes_and_reads_back() {
        let tmp = TempDir::new().unwrap();
        let tools = git_tools(&tmp).await;
        tools.write_file("a.md", "alpha").await.unwrap();
        assert_eq!(tools.read_file("a.md").await.unwrap(), "alpha");
        // Git backend → commit landed (HEAD points somewhere).
        let repo = git2::Repository::open(tmp.path()).unwrap();
        assert!(repo.head().is_ok(), "HEAD now exists");
        assert!(matches!(tools, WriteTools::Git(_)));
    }

    #[tokio::test]
    async fn dispatch_observably_different_for_batch_atomicity() {
        // Same failing batch: legacy leaves partial state, git leaves none.
        // Trigger = MoveNote from a non-existent source — both backends fail
        // on the read, but at different points in the apply pipeline.
        let make_ops = || {
            vec![
                BatchOperation::WriteNote {
                    path: "first.md".into(),
                    content: "F".into(),
                },
                BatchOperation::MoveNote {
                    from: "missing.md".into(),
                    to: "anywhere.md".into(),
                },
                BatchOperation::WriteNote {
                    path: "third.md".into(),
                    content: "T".into(),
                },
            ]
        };

        let l_tmp = TempDir::new().unwrap();
        let l = legacy_tools(&l_tmp).await;
        let l_res = l.batch_execute(make_ops()).await.unwrap();
        assert!(!l_res.success);
        // Legacy: `first.md` landed before the failed move -> partial state
        // (the defect the substrate replaces).
        assert!(
            l_tmp.path().join("first.md").exists(),
            "legacy leaves partial state behind"
        );

        let g_tmp = TempDir::new().unwrap();
        let g = git_tools(&g_tmp).await;
        let g_res = g.batch_execute(make_ops()).await.unwrap();
        assert!(!g_res.success);
        assert!(
            !g_tmp.path().join("first.md").exists(),
            "git substrate aborts atomically — no partial state"
        );
        assert!(!g_tmp.path().join("third.md").exists());
    }
}
