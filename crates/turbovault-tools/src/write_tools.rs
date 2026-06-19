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

    /// turbovault-lri: builder-style override for the underlying
    /// [`GitFileTools::include_ignored`] policy. No-op on the legacy arm
    /// (the legacy backend doesn't consult `.gitignore` at all). When
    /// `false`, every mutation pre-checks each touched path against the
    /// worktree's `.gitignore` matcher and refuses the transaction with
    /// a typed error if any path would be ignored. Default `true`.
    pub fn with_include_ignored(self, include_ignored: bool) -> Self {
        match self {
            Self::Git(g) => Self::Git(g.with_include_ignored(include_ignored)),
            other => other,
        }
    }

    /// turbovault-a0l (PERF-1): install the cached per-vault `VaultRepo` handle
    /// on the git arm so writes reuse it instead of opening per call. No-op on
    /// the legacy arm (no substrate handle).
    pub fn with_cached_repo(self, cached_repo: CachedRepo) -> Self {
        match self {
            Self::Git(g) => Self::Git(g.with_cached_repo(cached_repo)),
            other => other,
        }
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

    /// Strict create (turbovault-947 / write-note CAS-by-default).
    ///
    /// **Git backend:** the substrate's `Transaction::create` carries an
    /// `expect_absent` precondition — a concurrent winner makes the loser's
    /// CAS fail loudly with `ConcurrencyError`. This is the safety the
    /// MCP layer's pre-check cannot provide on its own (TOCTOU window).
    ///
    /// **Legacy backend:** delegates to `write_file` (best-effort; legacy
    /// has no atomic create primitive). The MCP layer's pre-check is the
    /// only protection — concurrent creates can still race. Known limit of
    /// the legacy path; documented, not fixed (per the legacy-stays
    /// direction).
    pub async fn create_file(&self, path: &str, content: &str) -> Result<()> {
        match self {
            Self::Legacy { files, .. } => files.write_file(path, content).await,
            Self::Git(g) => g.create_file(path, content).await,
        }
    }

    // -------- turbovault-0bh: caller-supplied commit message variants --------
    //
    // Each `_with_message` method behaves identically to its base sibling
    // except that on the git backend the caller's `message` becomes the
    // commit subject (and body, when newline-separated). Legacy backend
    // silently ignores `message` — legacy writes don't produce commits.

    pub async fn write_file_with_mode_and_message(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<()> {
        match self {
            Self::Legacy { files, .. } => {
                files
                    .write_file_with_mode(path, content, mode, expected_hash)
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
            Self::Legacy { files, .. } => files.write_file(path, content).await,
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
            Self::Legacy { files, .. } => {
                files.edit_file(path, edits, expected_hash, dry_run).await
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
            Self::Legacy { files, .. } => files.delete_file_with_hash(path, expected_hash).await,
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
            Self::Legacy { files, .. } => files.move_file_with_hash(from, to, expected_hash).await,
            Self::Git(g) => {
                g.move_file_with_hash_and_message(from, to, expected_hash, message)
                    .await
            }
        }
    }

    /// turbovault-oz6: list inbound backlinks for a path. Both backends
    /// resolve via the same in-memory link graph (kept coherent by the
    /// substrate's CommitHook + drainer / external-ref listener for git;
    /// kept manually-coherent by VaultManager mutators for legacy).
    pub async fn list_inbound_backlinks(&self, path: &str) -> Result<Vec<String>> {
        match self {
            Self::Git(g) => g.list_inbound_backlinks(path).await,
            Self::Legacy { files, .. } => {
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
    /// **Git backend only** — legacy refuses loudly (no atomic multi-file
    /// primitive).
    pub async fn delete_file_with_link_rewrite_to_stale(
        &self,
        path: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<MoveWithLinksResult> {
        match self {
            Self::Legacy { .. } => Err(Error::config_error(
                "Atomic delete + wikilink wrap-as-stale requires write_backend=git. The legacy backend has no multi-file atomic primitive; use force=true on the legacy delete (rename-only — links will dangle) or switch to git.",
            )),
            Self::Git(g) => {
                g.delete_file_with_link_rewrite_to_stale(path, expected_hash, message)
                    .await
            }
        }
    }

    /// turbovault-lqr: atomic move + inbound-wikilink rewrite.
    /// **Git backend only** — legacy refuses loudly (no atomic multi-file
    /// primitive; the substrate's killer feature that the legacy path
    /// cannot match).
    pub async fn move_file_with_link_updates(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
        message: &str,
    ) -> Result<MoveWithLinksResult> {
        match self {
            Self::Legacy { .. } => Err(Error::config_error(
                "Atomic move + wikilink update requires write_backend=git. The legacy backend has no multi-file atomic primitive; use the legacy `move_file` flow (rename only; links will dangle) or switch to git.",
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
            Self::Legacy { batch, .. } => {
                legacy_batch_refusal(&operations)?;
                // Legacy doesn't commit; message ignored.
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
            Self::Legacy { batch, .. } => {
                // turbovault-c0e / 0g4: legacy backend has no per-op CAS
                // primitive and no git-only ops (per the legacy-stays direction
                // in turbovault-6fo.16). Refuse loudly rather than silently
                // dropping the precondition or partially applying.
                legacy_batch_refusal(&operations)?;
                batch.batch_execute(operations).await
            }
            Self::Git(g) => g.batch_execute(operations).await,
        }
    }
}

/// turbovault-c0e: scan a batch for ops that declare a per-op CAS
/// precondition the legacy backend cannot honor. Returns the index of the
/// first one found, or `None` if every op is precondition-free.
///
/// `CreateNote` is NOT flagged — `force: Some(true)` and `force: None` both
/// map cleanly to the legacy "blind create/overwrite" behavior. The
/// `expect_absent` semantic the git backend adds is git-only; legacy
/// callers accept the no-CAS risk by virtue of choosing the legacy backend.
fn first_op_with_precondition(operations: &[BatchOperation]) -> Option<usize> {
    operations.iter().position(|op| match op {
        BatchOperation::CreateNote { .. } => false,
        BatchOperation::WriteNote { expected_hash, .. }
        | BatchOperation::DeleteNote { expected_hash, .. }
        | BatchOperation::MoveNote { expected_hash, .. }
        | BatchOperation::UpdateLinks { expected_hash, .. } => expected_hash.is_some(),
        // turbovault-0g4: git-substrate-only ops (EditNote, …) are refused by
        // `legacy_batch_refusal`'s git-only check (which runs first); don't
        // preempt that clearer message with a precondition error here.
        _ => false,
    })
}

/// turbovault-0g4: index + name of the first git-substrate-only op in a batch
/// (one with no legacy executor equivalent — see
/// [`turbovault_batch::BatchOperation::git_only_kind`]), or `None` if every op
/// is legacy-capable.
fn first_git_only_op(operations: &[BatchOperation]) -> Option<(usize, &'static str)> {
    operations
        .iter()
        .enumerate()
        .find_map(|(i, op)| op.git_only_kind().map(|kind| (i, kind)))
}

/// turbovault-0g4 + c0e: the two refusals the legacy batch dispatch performs
/// upfront (zero side effects), in priority order:
/// 1. git-substrate-only ops (no legacy equivalent), then
/// 2. per-op CAS preconditions (no legacy batch-level CAS).
///
/// Refusing here — rather than letting the executor partially apply or return
/// a softer `BatchResult { success: false }` — keeps `write_backend=legacy`
/// behavior unchanged and the error shape consistent across both refusals.
fn legacy_batch_refusal(operations: &[BatchOperation]) -> Result<()> {
    if let Some((idx, kind)) = first_git_only_op(operations) {
        return Err(Error::config_error(format!(
            "BatchOperation at index {idx} ({kind}) requires write_backend=git; the legacy batch executor has no equivalent. Switch the vault to the git backend to use it."
        )));
    }
    if let Some(idx) = first_op_with_precondition(operations) {
        return Err(Error::config_error(format!(
            "BatchOperation at index {idx} carries a per-op CAS precondition (expected_hash), but write_backend=legacy has no batch-level CAS. Use write_backend=git for per-op CAS, or drop the precondition."
        )));
    }
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

    /// turbovault-c0e: legacy backend refuses a batch with per-op
    /// preconditions loudly. Caller switches to git backend or drops the
    /// precondition rather than silently losing CAS.
    #[tokio::test]
    async fn legacy_batch_refuses_per_op_precondition() {
        let tmp = TempDir::new().unwrap();
        let tools = legacy_tools(&tmp).await;
        let ops = vec![BatchOperation::WriteNote {
            path: "a.md".into(),
            content: "v".into(),
            expected_hash: Some("0123456789abcdef0123456789abcdef01234567".into()),
        }];
        let err = tools.batch_execute(ops).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("write_backend=legacy") && msg.contains("CAS"),
            "expected legacy-refuse error, got: {msg}"
        );
    }

    /// turbovault-0g4.1: a git-substrate-only op (EditNote) in a legacy batch
    /// is refused with a clear write_backend=git message, and NO earlier op is
    /// applied (validate() rejects upfront, zero side effects). Keeps the
    /// legacy backend's behavior unchanged for users who never had these ops.
    #[tokio::test]
    async fn legacy_batch_refuses_git_only_edit_note() {
        let tmp = TempDir::new().unwrap();
        let tools = legacy_tools(&tmp).await;
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
            "no op applied on a refused legacy batch"
        );
    }

    /// turbovault-c0e: precondition-FREE batches still pass through to the
    /// legacy executor unchanged.
    #[tokio::test]
    async fn legacy_batch_passes_through_when_no_preconditions() {
        let tmp = TempDir::new().unwrap();
        let tools = legacy_tools(&tmp).await;
        let ops = vec![BatchOperation::WriteNote {
            path: "a.md".into(),
            content: "v".into(),
            expected_hash: None,
        }];
        let res = tools.batch_execute(ops).await.unwrap();
        assert!(res.success);
    }

    /// turbovault-947: legacy dispatch has no atomic create primitive — the
    /// fallback is `write_file` which blind-overwrites. Documented limit;
    /// the MCP layer's pre-check is the only protection on legacy.
    #[tokio::test]
    async fn legacy_create_file_is_blind_fallback() {
        let tmp = TempDir::new().unwrap();
        let tools = legacy_tools(&tmp).await;
        tools.write_file("dup.md", "v1").await.unwrap();
        // Legacy intentionally allows this — known limit.
        tools.create_file("dup.md", "v2").await.unwrap();
        assert_eq!(tools.read_file("dup.md").await.unwrap(), "v2");
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
