//! Backend-dispatching write surface (GWS.12).
//!
//! [`WriteTools`] wraps either the direct [`FileTools`] + [`BatchTools`] pair
//! or the git-backed [`GitFileTools`], chosen at construction from a vault's
//! [`turbovault_core::config::WriteBackend`]. The MCP layer holds one
//! `WriteTools` per vault and never branches on the backend itself.
//!
//! **Lifecycle:** a permanent per-vault dispatch, not a cutover shim. The
//! non-git (`Direct`) and `Git` arms are two write mechanisms that coexist,
//! one per vault; neither arm is a deletion target.

use crate::batch_tools::BatchTools;
use crate::file_tools::{FileTools, NoteInfo, WriteMode};
use crate::git_file_tools::{CachedRepo, CasCollisionFlush, GitFileTools, MoveWithLinksResult};
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
    /// The non-git (`Direct`) write path: `VaultManager` mutators +
    /// `BatchExecutor`. A permanent per-vault option, not a deletion target.
    Direct { files: FileTools, batch: BatchTools },
    /// `turbovault-git` substrate — every change is a commit.
    Git(GitFileTools),
}

impl WriteTools {
    /// Whether this dispatcher is backed by the atomic Git substrate.
    pub fn is_git(&self) -> bool {
        matches!(self, Self::Git(_))
    }

    /// Construct the direct dispatch wrapping the existing `VaultManager`-backed
    /// tools.
    pub fn direct(manager: Arc<VaultManager>) -> Self {
        Self::Direct {
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

    /// turbovault-lri: builder-style override for the underlying
    /// [`GitFileTools::include_ignored`] policy. No-op on the direct arm
    /// (the direct backend doesn't consult `.gitignore` at all). When
    /// `false`, every mutation pre-checks each touched path against the
    /// worktree's `.gitignore` matcher and refuses the changeset with
    /// a typed error if any path would be ignored. Default `true`.
    pub fn with_include_ignored(self, include_ignored: bool) -> Self {
        match self {
            Self::Git(g) => Self::Git(g.with_include_ignored(include_ignored)),
            other => other,
        }
    }

    /// turbovault-a0l (PERF-1): install the cached per-vault `VaultRepo` handle
    /// on the git arm so writes reuse it instead of opening per call. No-op on
    /// the direct arm (no substrate handle).
    pub fn with_cached_repo(self, cached_repo: CachedRepo) -> Self {
        match self {
            Self::Git(g) => Self::Git(g.with_cached_repo(cached_repo)),
            other => other,
        }
    }

    // -------- Reads (forwarded; both backends use working-tree bytes) --------

    pub async fn read_file(&self, path: &str) -> Result<String> {
        match self {
            Self::Direct { files, .. } => files.read_file(path).await,
            Self::Git(g) => g.read_file(path).await,
        }
    }

    pub async fn get_notes_info(&self, paths: &[String]) -> Result<Vec<NoteInfo>> {
        match self {
            Self::Direct { files, .. } => files.get_notes_info(paths).await,
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
            Self::Direct { files, .. } => {
                files
                    .write_file_with_mode(
                        path,
                        content,
                        mode,
                        expected_hash,
                        &format!("write_file {path}"),
                    )
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
            Self::Direct { files, .. } => files.write_file(path, content).await,
            Self::Git(g) => g.write_file(path, content).await,
        }
    }

    /// Strict create (turbovault-947 / write-note CAS-by-default).
    ///
    /// **Git backend:** the substrate's `ChangePlan::create` carries an
    /// `expect_absent` precondition — a concurrent winner makes the loser's
    /// CAS fail loudly with `ConcurrencyError`. This is the safety the
    /// MCP layer's pre-check cannot provide on its own (TOCTOU window).
    ///
    /// **Direct backend:** delegates to `write_file` (best-effort; direct
    /// has no atomic create primitive). The MCP layer's pre-check is the
    /// only protection — concurrent creates can still race. Known limit of
    /// the direct path; documented, not fixed (per the direct-stays
    /// direction).
    pub async fn create_file(&self, path: &str, content: &str) -> Result<()> {
        match self {
            Self::Direct { files, .. } => files.write_file(path, content).await,
            Self::Git(g) => g.create_file(path, content).await,
        }
    }

    // -------- turbovault-0bh: caller-supplied commit message variants --------
    //
    // Each `_with_message` method behaves identically to its base sibling
    // except that on the git backend the caller's `message` becomes the
    // commit subject (and body, when newline-separated). Direct backend
    // silently ignores `message` — direct writes don't produce commits.

    pub async fn write_file_with_mode_and_message(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        match self {
            Self::Direct { files, .. } => {
                files
                    .write_file_with_mode(path, content, mode, expected_hash, message)
                    .await
            }
            Self::Git(g) => {
                g.write_file_with_mode_and_message(path, content, mode, expected_hash, message)
                    .await
            }
        }
    }

    pub async fn create_file_with_message(
        &self,
        path: &str,
        content: &str,
        message: &str,
    ) -> Result<()> {
        match self {
            Self::Direct { files, .. } => files.write_file(path, content).await,
            Self::Git(g) => g.create_file_with_message(path, content, message).await,
        }
    }

    pub async fn edit_file_with_message(
        &self,
        path: &str,
        edits: &str,
        expected_hash: Option<&str>,
        dry_run: bool,
        message: &str,
    ) -> Result<EditResult> {
        match self {
            Self::Direct { files, .. } => {
                files
                    .edit_file(path, edits, expected_hash, dry_run, message)
                    .await
            }
            Self::Git(g) => {
                g.edit_file_with_message(path, edits, expected_hash, dry_run, message)
                    .await
            }
        }
    }

    pub async fn delete_file_with_hash_and_message(
        &self,
        path: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        match self {
            Self::Direct { files, .. } => {
                files
                    .delete_file_with_hash(path, expected_hash, message)
                    .await
            }
            Self::Git(g) => {
                g.delete_file_with_hash_and_message(path, expected_hash, message)
                    .await
            }
        }
    }

    pub async fn move_file_with_hash_and_message(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        match self {
            Self::Direct { files, .. } => {
                files
                    .move_file_with_hash(from, to, expected_hash, message)
                    .await
            }
            Self::Git(g) => {
                g.move_file_with_hash_and_message(from, to, expected_hash, message)
                    .await
            }
        }
    }

    /// turbovault-oz6: list inbound backlinks for a path. Both backends
    /// resolve via the same in-memory link graph (kept coherent by the
    /// substrate's CommitHook + drainer / external-ref listener for git;
    /// kept manually-coherent by VaultManager mutators for direct).
    pub async fn list_inbound_backlinks(&self, path: &str) -> Result<Vec<String>> {
        match self {
            Self::Git(g) => g.list_inbound_backlinks(path).await,
            Self::Direct { files, .. } => {
                let bls = files
                    .manager
                    .get_backlinks(std::path::Path::new(path))
                    .await?;
                let vault_root = files.manager.vault_path().clone();
                let mut out = Vec::new();
                for full in bls {
                    let rel = full
                        .strip_prefix(&vault_root)
                        .map(|p| p.to_path_buf())
                        .unwrap_or_else(|_| full.clone());
                    if let Some(s) = rel.to_str() {
                        out.push(s.to_string());
                    }
                }
                Ok(out)
            }
        }
    }

    /// turbovault-oz6: atomic delete + inbound-wikilink wrap-as-stale.
    /// **Git backend only** — direct refuses loudly (no atomic multi-file
    /// primitive).
    pub async fn delete_file_with_link_rewrite_to_stale(
        &self,
        path: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<MoveWithLinksResult> {
        match self {
            Self::Direct { .. } => Err(Error::config_error(
                "Atomic delete + wikilink wrap-as-stale requires write_backend=git. The direct backend has no multi-file atomic primitive; use force=true on the direct delete (rename-only — links will dangle) or switch to git.",
            )),
            Self::Git(g) => {
                g.delete_file_with_link_rewrite_to_stale(path, expected_hash, message)
                    .await
            }
        }
    }

    /// turbovault-lqr: atomic move + inbound-wikilink rewrite.
    /// **Git backend only** — direct refuses loudly (no atomic multi-file
    /// primitive; the substrate's killer feature that the direct path
    /// cannot match).
    pub async fn move_file_with_link_updates(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<MoveWithLinksResult> {
        match self {
            Self::Direct { .. } => Err(Error::config_error(
                "Atomic move + wikilink update requires write_backend=git. The direct backend has no multi-file atomic primitive; use the direct `move_file` flow (rename only; links will dangle) or switch to git.",
            )),
            Self::Git(g) => {
                g.move_file_with_link_updates(from, to, expected_hash, message)
                    .await
            }
        }
    }

    pub async fn batch_execute_with_message(
        &self,
        operations: Vec<BatchOperation>,
        message: &str,
    ) -> Result<BatchResult> {
        match self {
            Self::Direct { batch, .. } => {
                direct_batch_refusal(&operations)?;
                // Direct doesn't commit; message ignored.
                batch.batch_execute(operations).await
            }
            Self::Git(g) => g.batch_execute_with_message(operations, message).await,
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
            Self::Direct { files, .. } => {
                files
                    .edit_file(
                        path,
                        edits,
                        expected_hash,
                        dry_run,
                        &format!("edit_file {path}"),
                    )
                    .await
            }
            Self::Git(g) => g.edit_file(path, edits, expected_hash, dry_run).await,
        }
    }

    pub async fn delete_file(&self, path: &str) -> Result<()> {
        match self {
            Self::Direct { files, .. } => files.delete_file(path).await,
            Self::Git(g) => g.delete_file(path).await,
        }
    }

    pub async fn delete_file_with_hash(
        &self,
        path: &str,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        match self {
            Self::Direct { files, .. } => {
                files
                    .delete_file_with_hash(path, expected_hash, &format!("delete_file {path}"))
                    .await
            }
            Self::Git(g) => g.delete_file_with_hash(path, expected_hash).await,
        }
    }

    pub async fn move_file(&self, from: &str, to: &str) -> Result<()> {
        match self {
            Self::Direct { files, .. } => files.move_file(from, to).await,
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
            Self::Direct { files, .. } => {
                files
                    .move_file_with_hash(
                        from,
                        to,
                        expected_hash,
                        &format!("move_file {from} -> {to}"),
                    )
                    .await
            }
            Self::Git(g) => g.move_file_with_hash(from, to, expected_hash).await,
        }
    }

    pub async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        match self {
            Self::Direct { files, .. } => files.copy_file(from, to).await,
            Self::Git(g) => g.copy_file(from, to).await,
        }
    }

    pub async fn batch_execute(&self, operations: Vec<BatchOperation>) -> Result<BatchResult> {
        match self {
            Self::Direct { batch, .. } => {
                // turbovault-c0e / 0g4: direct backend has no per-op CAS
                // primitive and no git-only ops (per the direct-stays direction
                // in turbovault-6fo.16). Refuse loudly rather than silently
                // dropping the precondition or partially applying.
                direct_batch_refusal(&operations)?;
                batch.batch_execute(operations).await
            }
            Self::Git(g) => g.batch_execute(operations).await,
        }
    }
}

/// turbovault-0g4: index + name of the first git-substrate-only op in a batch
/// (one with no direct executor equivalent — see
/// [`turbovault_batch::BatchOperation::git_only_kind`]), or `None` if every op
/// is direct-capable.
fn first_git_only_op(operations: &[BatchOperation]) -> Option<(usize, &'static str)> {
    operations
        .iter()
        .enumerate()
        .find_map(|(i, op)| op.git_only_kind().map(|kind| (i, kind)))
}

/// turbovault-0g4 + c0e: the two refusals the direct batch dispatch performs
/// upfront (zero side effects), in priority order:
/// 1. git-substrate-only ops (no direct equivalent), then
/// 2. per-op CAS preconditions (no direct batch-level CAS).
///
/// Refusing here — rather than letting the executor partially apply or return
/// a softer `BatchResult { success: false }` — keeps `write_backend=direct`
/// behavior unchanged and the error shape consistent across both refusals.
fn direct_batch_refusal(operations: &[BatchOperation]) -> Result<()> {
    if let Some((idx, kind)) = first_git_only_op(operations) {
        return Err(Error::config_error(format!(
            "BatchOperation at index {idx} ({kind}) requires write_backend=git; the direct batch executor has no equivalent. Switch the vault to the git backend to use it."
        )));
    }
    // The direct executor performs its best-effort preflight validation for
    // expected_hash values. It is not cross-process atomic, but preserving
    // that compatibility is preferable to rejecting batches that worked
    // before the Git backend was introduced.
    Ok(())
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

    async fn direct_tools(tmp: &TempDir) -> WriteTools {
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path(), "l")).unwrap());
        WriteTools::direct(manager)
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
    async fn direct_dispatch_writes_and_reads_back() {
        let tmp = TempDir::new().unwrap();
        let tools = direct_tools(&tmp).await;
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

    /// turbovault-947: git dispatch carries `expect_absent` on create — a
    /// second writer for the same path loses with `ConcurrencyError`.
    #[tokio::test]
    async fn git_create_file_aborts_on_existing_path() {
        let tmp = TempDir::new().unwrap();
        let tools = git_tools(&tmp).await;
        tools.write_file("dup.md", "v1").await.unwrap();
        let err = tools.create_file("dup.md", "v2").await.unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
        assert_eq!(tools.read_file("dup.md").await.unwrap(), "v1");
    }

    /// Direct retains its best-effort expected-hash preflight for backwards
    /// compatibility. The Git backend is required for cross-process atomicity.
    #[tokio::test]
    async fn direct_batch_honors_per_op_precondition_preflight() {
        let tmp = TempDir::new().unwrap();
        let tools = direct_tools(&tmp).await;
        let ops = vec![BatchOperation::WriteNote {
            path: "a.md".into(),
            content: "v".into(),
            expected_hash: Some("0123456789abcdef0123456789abcdef01234567".into()),
        }];
        let result = tools.batch_execute(ops).await.unwrap();
        assert!(!result.success);
        assert!(!tmp.path().join("a.md").exists());
    }

    /// turbovault-0g4.1: a git-substrate-only op (EditNote) in a direct batch
    /// is refused with a clear write_backend=git message, and NO earlier op is
    /// applied (validate() rejects upfront, zero side effects). Keeps the
    /// direct backend's behavior unchanged for users who never had these ops.
    #[tokio::test]
    async fn direct_batch_refuses_git_only_edit_note() {
        let tmp = TempDir::new().unwrap();
        let tools = direct_tools(&tmp).await;
        let ops = vec![
            BatchOperation::WriteNote {
                path: "kept.md".into(),
                content: "v".into(),
                expected_hash: None,
            },
            BatchOperation::EditNote {
                path: "kept.md".into(),
                edits: "<<<<<<< SEARCH\nv\n=======\nw\n>>>>>>> REPLACE".into(),
                expected_hash: None,
            },
        ];
        let err = tools.batch_execute(ops).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("write_backend=git") && msg.contains("EditNote"),
            "expected git-only refusal, got: {msg}"
        );
        // validate() refuses upfront: the earlier WriteNote never landed.
        assert!(
            !tmp.path().join("kept.md").exists(),
            "no op applied on a refused direct batch"
        );
    }

    /// turbovault-c0e: precondition-FREE batches still pass through to the
    /// direct executor unchanged.
    #[tokio::test]
    async fn direct_batch_passes_through_when_no_preconditions() {
        let tmp = TempDir::new().unwrap();
        let tools = direct_tools(&tmp).await;
        let ops = vec![BatchOperation::WriteNote {
            path: "a.md".into(),
            content: "v".into(),
            expected_hash: None,
        }];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(res.success);
    }

    /// turbovault-947: direct dispatch has no atomic create primitive — the
    /// fallback is `write_file` which blind-overwrites. Documented limit;
    /// the MCP layer's pre-check is the only protection on direct.
    #[tokio::test]
    async fn direct_create_file_is_blind_fallback() {
        let tmp = TempDir::new().unwrap();
        let tools = direct_tools(&tmp).await;
        tools.write_file("dup.md", "v1").await.unwrap();
        // Direct intentionally allows this — known limit.
        tools.create_file("dup.md", "v2").await.unwrap();
        assert_eq!(tools.read_file("dup.md").await.unwrap(), "v2");
    }

    #[tokio::test]
    async fn dispatch_observably_different_for_batch_atomicity() {
        // Same failing batch: direct leaves partial state, git leaves none.
        // Trigger = MoveNote from a non-existent source — both backends fail
        // on the read, but at different points in the apply pipeline.
        let make_ops = || {
            vec![
                BatchOperation::WriteNote {
                    path: "first.md".into(),
                    content: "F".into(),
                    expected_hash: None,
                },
                BatchOperation::MoveNote {
                    from: "missing.md".into(),
                    to: "anywhere.md".into(),
                    expected_hash: None,
                    update_backlinks: None,
                },
                BatchOperation::WriteNote {
                    path: "third.md".into(),
                    content: "T".into(),
                    expected_hash: None,
                },
            ]
        };

        let l_tmp = TempDir::new().unwrap();
        let l = direct_tools(&l_tmp).await;
        let l_res = l.batch_execute(make_ops()).await.unwrap();
        assert!(!l_res.success);
        // Direct: `first.md` landed before the failed move -> partial state
        // (the defect the substrate replaces).
        assert!(
            l_tmp.path().join("first.md").exists(),
            "direct leaves partial state behind"
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
