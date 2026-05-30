//! Git-backed write tools (GWS.12).
//!
//! Mirrors the mutating surface of [`crate::FileTools`] + [`crate::BatchTools`]
//! but routes every change through the git substrate's
//! [`turbovault_git::VaultRepo::apply_transaction`]. Reads still go through the
//! shared [`VaultManager`] — working tree == HEAD, so working-tree reads agree
//! with the git tip.
//!
//! Selected per vault by [`turbovault_core::config::WriteBackend::Git`]. Lives
//! beside `FileTools` only until the cutover (GWS.15), then this becomes the
//! sole write surface.
//!
//! **Discipline (do not violate):** `GitFileTools` reads from `VaultManager`
//! but must **never** call its mutators (`write_file`/`edit_file`/`delete_file`
//! /`move_file`). All mutations route through [`VaultRepo::apply_transaction`].

use crate::file_tools::{NoteInfo, WriteMode};
use crate::read_set::{ReadSet, apply_read_set_to_transaction};
use futures::future::BoxFuture;
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_batch::{BatchOperation, BatchResult, OperationRecord};
use turbovault_core::prelude::*;
use turbovault_git::{CommitHook, CommitLocks, Oid, Transaction, VaultRepo};
use turbovault_vault::{EditEngine, EditResult, VaultManager};

/// Callback invoked **before** returning a `ConcurrencyError` from
/// [`GitFileTools::apply_txn`] (GWS.14b). The MCP server installs one that
/// drains the per-vault reindex queue — so the agent's re-read (which the
/// error tells it to do) sees a coherent graph + search state, not the
/// pre-conflict snapshot.
///
/// Boxed-future shape rather than a plain `async fn` so the type can be
/// stored on the `GitFileTools` struct without each call site naming an
/// `impl Future`.
pub type CasCollisionFlush = Arc<dyn Fn() -> BoxFuture<'static, Result<()>> + Send + Sync>;

/// Write-side tools backed by the git substrate.
///
/// Holds the vault path + a shared `CommitLocks` registry rather than an
/// `Arc<VaultRepo>` — `VaultRepo` wraps a `git2::Repository` which is `!Sync`
/// (raw pointer), so it cannot live inside an `async fn` future that needs
/// to be `Send`. The substrate handle is opened fresh inside a
/// `spawn_blocking` task per call (open is ~µs); the shared `CommitLocks`
/// keeps cross-call commit-section serialization intact.
#[derive(Clone)]
pub struct GitFileTools {
    pub manager: Arc<VaultManager>,
    pub vault_path: PathBuf,
    pub commit_locks: Arc<CommitLocks>,
    /// Optional post-commit hook installed on every `VaultRepo` opened
    /// inside `apply_txn`. Plumbed for GWS.14 lazy GSU: the MCP server
    /// passes a closure that pushes the new commit onto a per-vault
    /// `ReindexQueue`. `None` = no reindex wiring (acceptable for tests
    /// that don't care about derived state).
    pub commit_hook: Option<CommitHook>,
    /// Optional flush callback fired BEFORE returning a `ConcurrencyError`
    /// (GWS.14b). Drains the reindex queue so the agent's re-read sees
    /// coherent derived state. `None` = skip flush; callers see the raw
    /// concurrency error and the graph stays as stale as the last
    /// flush-on-query did.
    pub flush_on_collision: Option<CasCollisionFlush>,
}

impl GitFileTools {
    /// Construct without a reindex hook (graph + search stay stale until
    /// another path triggers their rebuild). Tests use this; the MCP
    /// server uses [`Self::new_with_hook`].
    pub fn new(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
    ) -> Self {
        Self {
            manager,
            vault_path,
            commit_locks,
            commit_hook: None,
            flush_on_collision: None,
        }
    }

    /// Construct with a reindex hook fired post-commit. The MCP server
    /// installs one that pushes onto a per-vault [`crate::ReindexQueue`].
    pub fn new_with_hook(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
        commit_hook: CommitHook,
    ) -> Self {
        Self {
            manager,
            vault_path,
            commit_locks,
            commit_hook: Some(commit_hook),
            flush_on_collision: None,
        }
    }

    /// Construct with both a reindex hook AND a CAS-collision flush callback
    /// (GWS.14b). The flush callback runs BEFORE the `ConcurrencyError` is
    /// returned to the caller, so the agent's re-read sees coherent derived
    /// state.
    pub fn new_with_hook_and_flush(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
        commit_hook: CommitHook,
        flush_on_collision: CasCollisionFlush,
    ) -> Self {
        Self {
            manager,
            vault_path,
            commit_locks,
            commit_hook: Some(commit_hook),
            flush_on_collision: Some(flush_on_collision),
        }
    }

    // -------- Reads (forwarded to VaultManager / fs) --------

    /// Read a file from the vault (working tree == HEAD, so this is the
    /// committed bytes).
    pub async fn read_file(&self, path: &str) -> Result<String> {
        self.manager.read_file(&PathBuf::from(path)).await
    }

    /// Lightweight metadata for multiple files — same shape as
    /// [`crate::FileTools::get_notes_info`].
    pub async fn get_notes_info(&self, paths: &[String]) -> Result<Vec<NoteInfo>> {
        let files = crate::FileTools::new(Arc::clone(&self.manager));
        files.get_notes_info(paths).await
    }

    // -------- Writes (route through VaultRepo) --------

    /// Write a file — overwrite by default, append/prepend for the other
    /// modes (mirrors [`crate::FileTools::write_file_with_mode`]).
    ///
    /// `expected_hash`, when present, must be a **git blob oid hex string**
    /// (40 hex chars). The substrate's version token is the blob oid (not a
    /// SHA-256 content hash). A non-Oid string is rejected loudly rather than
    /// silently dropping CAS protection.
    pub async fn write_file_with_mode(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        self.write_file_with_read_set(path, content, mode, expected_hash, None)
            .await
    }

    /// Overwrite shortcut — equivalent to `write_file_with_mode(.., Overwrite, None)`.
    pub async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        self.write_file_with_mode(path, content, WriteMode::Overwrite, None)
            .await
    }

    /// Edit a file via SEARCH/REPLACE blocks. Reads working-tree bytes,
    /// applies the blocks in memory, and commits the result as one
    /// transaction. `dry_run = true` returns the preview without committing.
    pub async fn edit_file(
        &self,
        path: &str,
        edits: &str,
        expected_hash: Option<&str>,
        dry_run: bool,
    ) -> Result<EditResult> {
        let expected = parse_blob_oid(expected_hash)?;

        // Read current bytes from the working tree.
        let current = self.read_file(path).await?;

        // Apply SEARCH/REPLACE blocks.
        let engine = EditEngine::new();
        let blocks = engine.parse_blocks(edits)?;
        let (mut result, new_content) = engine.apply_edits(&current, &blocks, dry_run)?;

        // turbovault-6sj / TV-011: overwrite the SHA-256s that EditEngine
        // computed with git blob OIDs so the returned `old_hash`/`new_hash`
        // round-trip via `expected_hash` against the substrate's
        // `expect_blob` precondition. Same in the dry-run path so callers
        // can use a preview's `new_hash` as the next `expected_hash`.
        result.old_hash = VaultRepo::blob_oid_of(current.as_bytes())
            .map_err(|e| Error::config_error(format!("blob_oid_of(current): {}", e)))?
            .to_string();
        result.new_hash = VaultRepo::blob_oid_of(new_content.as_bytes())
            .map_err(|e| Error::config_error(format!("blob_oid_of(new): {}", e)))?
            .to_string();

        if dry_run {
            return Ok(result);
        }

        let txn = build_upsert_txn(format!("edit_file {}", path), path, &new_content, expected);
        self.apply_txn(&txn).await?;
        Ok(result)
    }

    /// Delete a file. `expected_hash` (blob oid hex) enforces a CAS
    /// precondition — pass `None` for a blind delete.
    pub async fn delete_file(&self, path: &str) -> Result<()> {
        self.delete_file_with_hash(path, None).await
    }

    /// Delete with optional blob-oid CAS.
    pub async fn delete_file_with_hash(
        &self,
        path: &str,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        let expected = parse_blob_oid(expected_hash)?;
        let mut txn = Transaction::new(format!("delete_file {}", path)).remove(path);
        if let Some(oid) = expected {
            txn = txn.expect_blob(path, oid);
        }
        self.apply_txn(&txn).await
    }

    /// Move a file — `remove(from) + upsert(to, bytes)` in one commit.
    pub async fn move_file(&self, from: &str, to: &str) -> Result<()> {
        self.move_file_with_hash(from, to, None).await
    }

    /// Move with optional blob-oid CAS on the source path.
    pub async fn move_file_with_hash(
        &self,
        from: &str,
        to: &str,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        let expected_from = parse_blob_oid(expected_hash)?;
        let content = self.read_file(from).await?;

        let mut txn = Transaction::new(format!("move_file {} -> {}", from, to))
            .remove(from)
            .upsert(to, content.into_bytes());
        if let Some(oid) = expected_from {
            txn = txn.expect_blob(from, oid);
        }
        // Destination is always required to be absent — refuses to clobber.
        txn = txn.expect_absent(to);
        self.apply_txn(&txn).await
    }

    /// Copy a file — read source, commit target (no source change). One
    /// commit. Same expect-absent guard on the destination as `move_file`.
    pub async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        let content = self.read_file(from).await?;
        let txn = Transaction::new(format!("copy_file {} -> {}", from, to))
            .upsert(to, content.into_bytes())
            .expect_absent(to);
        self.apply_txn(&txn).await
    }

    // -------- Batch (the atomicity win) --------

    /// Translate every [`BatchOperation`] to a single [`Transaction`] and
    /// commit as **one atomic commit** — either every op lands or none do.
    /// This is the spec-promised behavior the legacy [`BatchTools`] never
    /// actually delivered (the legacy path stopped at `failed_at` and left
    /// partial state on disk).
    pub async fn batch_execute(&self, operations: Vec<BatchOperation>) -> Result<BatchResult> {
        self.batch_execute_with_read_set(operations, None).await
    }

    // -------- internals --------

    async fn translate_op(&self, txn: Transaction, op: &BatchOperation) -> Result<Transaction> {
        Ok(match op {
            BatchOperation::CreateNote { path, content } => txn.create(path, content.as_bytes()),
            BatchOperation::WriteNote { path, content } => txn.upsert(path, content.as_bytes()),
            BatchOperation::DeleteNote { path } => txn.remove(path),
            BatchOperation::MoveNote { from, to } => {
                let content = self.read_file(from).await?;
                txn.remove(from).upsert(to, content.into_bytes())
            }
            BatchOperation::UpdateLinks {
                file,
                old_target,
                new_target,
            } => {
                let current = self.read_file(file).await?;
                let updated = current.replace(old_target, new_target);
                txn.upsert(file, updated.into_bytes())
            }
        })
    }

    /// GWS.5fm: write with an optional read-set precondition. Equivalent
    /// to [`Self::write_file_with_mode`] when `read_set` is `None`. When
    /// `Some`, every `(path, oid)` in the read-set becomes an additional
    /// `expect_blob` precondition on the same transaction — a concurrent
    /// change to any of those source files aborts the write loudly
    /// (`ConcurrencyError`) so the agent re-reads + re-decides.
    pub async fn write_file_with_read_set(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
        expected_hash: Option<&str>,
        read_set: Option<&ReadSet>,
    ) -> Result<()> {
        let final_content = self.resolve_write_content(path, content, mode).await?;
        let expected = parse_blob_oid(expected_hash)?;
        let txn = build_upsert_txn(
            format!("write_file {}", path),
            path,
            &final_content,
            expected,
        );
        self.apply_txn_augmented(txn, read_set).await
    }

    /// GWS.5fm: batch with an optional read-set precondition. The read-set's
    /// preconditions ride alongside the per-op preconditions, so the whole
    /// batch aborts atomically if any read-set source file changed.
    pub async fn batch_execute_with_read_set(
        &self,
        operations: Vec<BatchOperation>,
        read_set: Option<&ReadSet>,
    ) -> Result<BatchResult> {
        let started = std::time::Instant::now();
        let transaction_id = uuid::Uuid::new_v4().to_string();
        let total = operations.len();

        if operations.is_empty() {
            return Ok(BatchResult {
                success: false,
                executed: 0,
                total: 0,
                failed_at: None,
                changes: vec![],
                errors: vec!["Batch cannot be empty".to_string()],
                records: vec![],
                transaction_id,
                duration_ms: started.elapsed().as_millis() as u64,
            });
        }

        let mut txn = Transaction::new(format!("batch_execute ({} ops)", total));
        let mut changes = Vec::with_capacity(total);
        let mut records = Vec::with_capacity(total);

        for (idx, op) in operations.iter().enumerate() {
            let operation_desc = format!("{:?}", op);
            let affected = op.affected_files();
            match self.translate_op(txn, op).await {
                Ok(next) => {
                    txn = next;
                    changes.push(describe_op(op));
                    records.push(OperationRecord {
                        operation_index: idx,
                        operation: operation_desc,
                        success: true,
                        error: None,
                        affected_files: affected,
                    });
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    records.push(OperationRecord {
                        operation_index: idx,
                        operation: operation_desc,
                        success: false,
                        error: Some(err_msg.clone()),
                        affected_files: affected,
                    });
                    return Ok(BatchResult {
                        success: false,
                        executed: idx,
                        total,
                        failed_at: Some(idx),
                        changes,
                        errors: vec![err_msg],
                        records,
                        transaction_id,
                        duration_ms: started.elapsed().as_millis() as u64,
                    });
                }
            }
        }

        match self.apply_txn_augmented(txn, read_set).await {
            Ok(()) => Ok(BatchResult {
                success: true,
                executed: total,
                total,
                failed_at: None,
                changes,
                errors: vec![],
                records,
                transaction_id,
                duration_ms: started.elapsed().as_millis() as u64,
            }),
            Err(e) => Ok(BatchResult {
                success: false,
                executed: 0,
                total,
                failed_at: None,
                changes: vec![],
                errors: vec![e.to_string()],
                records,
                transaction_id,
                duration_ms: started.elapsed().as_millis() as u64,
            }),
        }
    }

    /// Internal helper used by every commit path. Applies `txn` after
    /// optionally augmenting it with `read_set` preconditions.
    async fn apply_txn_augmented(
        &self,
        txn: Transaction,
        read_set: Option<&ReadSet>,
    ) -> Result<()> {
        let txn = match read_set {
            Some(rs) => apply_read_set_to_transaction(txn, rs)?,
            None => txn,
        };
        self.apply_txn(&txn).await
    }

    /// Compute the bytes to write given mode + path. Extracted so
    /// `write_file_with_mode` and `write_file_with_read_set` share one
    /// implementation.
    async fn resolve_write_content(
        &self,
        path: &str,
        content: &str,
        mode: WriteMode,
    ) -> Result<String> {
        Ok(match mode {
            WriteMode::Overwrite => content.to_string(),
            WriteMode::Append => {
                let existing = self.read_file(path).await.unwrap_or_default();
                if existing.is_empty() {
                    content.to_string()
                } else {
                    format!("{}\n{}", existing, content)
                }
            }
            WriteMode::Prepend => {
                let existing = self.read_file(path).await.unwrap_or_default();
                if existing.is_empty() {
                    content.to_string()
                } else if existing.starts_with("---\n") || existing.starts_with("---\r\n") {
                    if let Some(end_idx) = find_frontmatter_end(&existing) {
                        let (fm, body) = existing.split_at(end_idx);
                        format!("{}\n{}\n{}", fm.trim_end(), content, body.trim_start())
                    } else {
                        format!("{}\n{}", content, existing)
                    }
                } else {
                    format!("{}\n{}", content, existing)
                }
            }
        })
    }

    async fn apply_txn(&self, txn: &Transaction) -> Result<()> {
        // `VaultRepo` is `Send` but `!Sync`; the substrate work is blocking
        // libgit2. Move it to the blocking pool. The `Arc<CommitLocks>` is
        // shared across calls so cross-call commit-section serialization
        // survives even though we open a fresh `VaultRepo` per call. The
        // optional commit hook is cloned in and installed on each per-call
        // open so the substrate fires it after a successful materialize.
        let path = self.vault_path.clone();
        let locks = Arc::clone(&self.commit_locks);
        let hook = self.commit_hook.clone();
        let txn = txn.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<()> {
            let repo = match hook {
                Some(h) => VaultRepo::open_with_locks_and_hook(&path, locks, h),
                None => VaultRepo::open_with_locks(&path, locks),
            }
            .map_err(git_err_to_core)?;
            repo.apply_transaction(&txn)
                .map(|_| ())
                .map_err(git_err_to_core)
        })
        .await
        .map_err(|e| Error::config_error(format!("git transaction task failed: {}", e)))?;

        // GWS.14b: on the reconsideration-domino abort, drain the reindex
        // queue BEFORE returning the error so the agent's re-read sees a
        // coherent graph/search state. In-process bursts where the conflict
        // is against THIS process's own earlier commit benefit directly;
        // cross-process conflicts (§8.4) still need a separate listener.
        if let Err(ref e) = result
            && matches!(e, Error::ConcurrencyError { .. })
            && let Some(flush) = &self.flush_on_collision
            && let Err(flush_err) = flush().await
        {
            log::warn!(
                "GWS.14b CAS-collision flush failed (returning original error): {}",
                flush_err
            );
        }
        result
    }
}

fn build_upsert_txn(
    message: String,
    path: &str,
    content: &str,
    expected: Option<Oid>,
) -> Transaction {
    let mut txn = Transaction::new(message).upsert(path, content.as_bytes().to_vec());
    if let Some(oid) = expected {
        txn = txn.expect_blob(path, oid);
    }
    txn
}

fn parse_blob_oid(s: Option<&str>) -> Result<Option<Oid>> {
    match s {
        None => Ok(None),
        Some(hex) => Oid::from_str(hex).map(Some).map_err(|_| {
            // `ConcurrencyError` (not `ConfigError`) so callers can switch on
            // ONE error type for both backends. Cross-restart edge case
            // (server flipped `write_backend` between the client's read and
            // write) lands here for SHA-256→git, and as a hash-mismatch
            // `ConcurrencyError` for git→SHA-256 — same shape, same fix
            // (re-read + retry).
            Error::ConcurrencyError {
                reason: format!(
                    "expected_hash for git backend must be a 40-char git blob oid hex (got {:?}). Re-read the file and retry with the fresh token.",
                    hex
                ),
            }
        }),
    }
}

fn describe_op(op: &BatchOperation) -> String {
    match op {
        BatchOperation::CreateNote { path, .. } => format!("created {}", path),
        BatchOperation::WriteNote { path, .. } => format!("wrote {}", path),
        BatchOperation::DeleteNote { path } => format!("deleted {}", path),
        BatchOperation::MoveNote { from, to } => format!("moved {} -> {}", from, to),
        BatchOperation::UpdateLinks { file, .. } => format!("updated links in {}", file),
    }
}

/// Translate a substrate error into the core error space used by the tool
/// layer. Precondition failures (the OCC CAS abort) become `ConcurrencyError`
/// so callers and tests can switch on the same shape they get from the
/// legacy path.
fn git_err_to_core(e: turbovault_git::Error) -> Error {
    match e {
        turbovault_git::Error::PreconditionFailed {
            path,
            expected,
            found,
        } => Error::ConcurrencyError {
            reason: format!(
                "precondition failed for {}: expected {:?}, found {:?}",
                path, expected, found
            ),
        },
        other => Error::config_error(format!("git substrate error: {}", other)),
    }
}

// -------- frontmatter helper (mirrors file_tools, deliberately not re-exported) --------

fn find_frontmatter_end(content: &str) -> Option<usize> {
    let start = if content.starts_with("---\r\n") {
        5
    } else if content.starts_with("---\n") {
        4
    } else {
        return None;
    };
    let bytes = content.as_bytes();
    let check_closing = |pos: usize| -> Option<usize> {
        if !bytes[pos..].starts_with(b"---") {
            return None;
        }
        let after = pos + 3;
        if after >= bytes.len() {
            return Some(after);
        }
        match bytes[after] {
            b'\n' => Some(after + 1),
            b'\r' if after + 1 < bytes.len() && bytes[after + 1] == b'\n' => Some(after + 2),
            _ => None,
        }
    };
    if let Some(end) = check_closing(start) {
        return Some(end);
    }
    let mut i = start;
    while i < bytes.len() {
        let nl = bytes[i..]
            .iter()
            .position(|&b| b == b'\n' || b == b'\r')
            .map(|p| i + p)?;
        let line_start = if bytes[nl] == b'\r' && nl + 1 < bytes.len() && bytes[nl + 1] == b'\n' {
            nl + 2
        } else {
            nl + 1
        };
        if line_start >= bytes.len() {
            break;
        }
        if let Some(end) = check_closing(line_start) {
            return Some(end);
        }
        i = line_start;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path as StdPath;
    use tempfile::TempDir;
    use turbovault_core::config::{ServerConfig, VaultConfig};
    use turbovault_vault::VaultManager;

    fn init_repo(dir: &StdPath) {
        let mut opts = git2::RepositoryInitOptions::new();
        opts.initial_head("main");
        git2::Repository::init_opts(dir, &opts).unwrap();
    }

    fn test_server_config(vault_dir: &StdPath) -> ServerConfig {
        let mut cfg = ServerConfig::new();
        cfg.vaults
            .push(VaultConfig::builder("t", vault_dir).build().unwrap());
        cfg
    }

    async fn setup() -> (TempDir, GitFileTools) {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let tools = GitFileTools::new(manager, tmp.path().to_path_buf(), locks);
        (tmp, tools)
    }

    fn head_oid(tools: &GitFileTools) -> Option<git2::Oid> {
        VaultRepo::open(&tools.vault_path).unwrap().head_oid()
    }

    #[tokio::test]
    async fn write_file_creates_commit_and_materializes() {
        let (tmp, tools) = setup().await;
        tools.write_file("a.md", "alpha").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("a.md")).unwrap(),
            "alpha"
        );
        assert!(head_oid(&tools).is_some(), "commit landed on HEAD");
    }

    #[tokio::test]
    async fn write_file_overwrites_existing() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "v1").await.unwrap();
        tools.write_file("a.md", "v2").await.unwrap();
        assert_eq!(tools.read_file("a.md").await.unwrap(), "v2");
    }

    #[tokio::test]
    async fn write_file_with_stale_blob_oid_aborts_concurrency_error() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "v1").await.unwrap();
        // Use a deliberately wrong blob oid.
        let bogus = VaultRepo::blob_oid_of(b"NOPE").unwrap();
        let err = tools
            .write_file_with_mode("a.md", "v2", WriteMode::Overwrite, Some(&bogus.to_string()))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
        assert_eq!(tools.read_file("a.md").await.unwrap(), "v1");
    }

    #[tokio::test]
    async fn write_file_with_garbage_hash_is_loud_concurrency_error() {
        // A malformed hash from the caller (e.g. cross-restart edge case
        // where the legacy SHA-256 hex still lives in the client) lands as
        // ConcurrencyError, NOT ConfigError — same shape callers handle for
        // any other stale-token failure, single switch arm fixes both
        // backends.
        let (_tmp, tools) = setup().await;
        let err = tools
            .write_file_with_mode("a.md", "v1", WriteMode::Overwrite, Some("not-a-hash"))
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
    }

    #[tokio::test]
    async fn delete_file_removes_and_commits() {
        let (tmp, tools) = setup().await;
        tools.write_file("a.md", "x").await.unwrap();
        tools.delete_file("a.md").await.unwrap();
        assert!(!tmp.path().join("a.md").exists());
    }

    #[tokio::test]
    async fn move_file_atomic_remove_plus_add_one_commit() {
        let (tmp, tools) = setup().await;
        tools.write_file("old.md", "body").await.unwrap();
        let before = head_oid(&tools);
        tools.move_file("old.md", "new.md").await.unwrap();
        assert!(!tmp.path().join("old.md").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("new.md")).unwrap(),
            "body"
        );
        assert_ne!(head_oid(&tools), before, "new commit");
    }

    #[tokio::test]
    async fn move_file_refuses_to_clobber_existing_destination() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "A").await.unwrap();
        tools.write_file("b.md", "B").await.unwrap();
        let err = tools.move_file("a.md", "b.md").await.unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
        // Both files still present, untouched.
        assert_eq!(tools.read_file("a.md").await.unwrap(), "A");
        assert_eq!(tools.read_file("b.md").await.unwrap(), "B");
    }

    #[tokio::test]
    async fn copy_file_writes_destination_only() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "alpha").await.unwrap();
        tools.copy_file("a.md", "b.md").await.unwrap();
        assert_eq!(tools.read_file("a.md").await.unwrap(), "alpha");
        assert_eq!(tools.read_file("b.md").await.unwrap(), "alpha");
    }

    #[tokio::test]
    async fn edit_file_search_replace_commits() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "hello world\n").await.unwrap();
        let edits = "<<<<<<< SEARCH\nhello world\n=======\nhi world\n>>>>>>> REPLACE\n";
        tools.edit_file("a.md", edits, None, false).await.unwrap();
        assert_eq!(tools.read_file("a.md").await.unwrap(), "hi world\n");
    }

    #[tokio::test]
    async fn edit_file_dry_run_does_not_commit() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "hello\n").await.unwrap();
        let head_before = head_oid(&tools);
        let edits = "<<<<<<< SEARCH\nhello\n=======\nbye\n>>>>>>> REPLACE\n";
        let _ = tools.edit_file("a.md", edits, None, true).await.unwrap();
        assert_eq!(head_oid(&tools), head_before, "no commit on dry_run");
        assert_eq!(tools.read_file("a.md").await.unwrap(), "hello\n");
    }

    /// turbovault-6sj / TV-011: edit_file's returned `old_hash`/`new_hash`
    /// must be 40-char git blob OIDs on the git backend (NOT 64-char SHA-256).
    /// Without this, callers cannot use them as `expected_hash` on a follow-up
    /// call — the CAS round-trip breaks.
    #[tokio::test]
    async fn edit_file_returns_blob_oid_hashes_not_sha256() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "hello\n").await.unwrap();
        let edits = "<<<<<<< SEARCH\nhello\n=======\nbye\n>>>>>>> REPLACE\n";
        let result = tools.edit_file("a.md", edits, None, false).await.unwrap();
        assert_eq!(
            result.old_hash.len(),
            40,
            "old_hash must be 40-char blob OID hex, got {:?}",
            result.old_hash
        );
        assert_eq!(
            result.new_hash.len(),
            40,
            "new_hash must be 40-char blob OID hex, got {:?}",
            result.new_hash
        );
        // Sanity: each hash equals the blob OID of the actual content.
        let expected_old = VaultRepo::blob_oid_of(b"hello\n").unwrap().to_string();
        let expected_new = VaultRepo::blob_oid_of(b"bye\n").unwrap().to_string();
        assert_eq!(result.old_hash, expected_old);
        assert_eq!(result.new_hash, expected_new);
    }

    /// turbovault-6sj: the `new_hash` an edit returns must round-trip as
    /// `expected_hash` on the next call. This is the CAS contract the legacy
    /// path delivered; the git backend must do the same.
    #[tokio::test]
    async fn edit_file_new_hash_round_trips_as_expected_hash() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "v1\n").await.unwrap();
        let edits1 = "<<<<<<< SEARCH\nv1\n=======\nv2\n>>>>>>> REPLACE\n";
        let r1 = tools.edit_file("a.md", edits1, None, false).await.unwrap();
        // Use r1.new_hash as the next expected_hash — must succeed because
        // no concurrent change has touched the file.
        let edits2 = "<<<<<<< SEARCH\nv2\n=======\nv3\n>>>>>>> REPLACE\n";
        let r2 = tools
            .edit_file("a.md", edits2, Some(&r1.new_hash), false)
            .await
            .unwrap();
        assert_eq!(
            r2.old_hash, r1.new_hash,
            "old_hash chains to prior new_hash"
        );
        assert_eq!(tools.read_file("a.md").await.unwrap(), "v3\n");
    }

    /// turbovault-6sj: dry-run hashes must match what an actual apply would
    /// produce, so callers can use a preview's `new_hash` to plan the next
    /// `expected_hash`.
    #[tokio::test]
    async fn edit_file_dry_run_hashes_match_real_apply() {
        let (_tmp, tools) = setup().await;
        tools.write_file("a.md", "hello\n").await.unwrap();
        let edits = "<<<<<<< SEARCH\nhello\n=======\nbye\n>>>>>>> REPLACE\n";
        let dry = tools.edit_file("a.md", edits, None, true).await.unwrap();
        let live = tools.edit_file("a.md", edits, None, false).await.unwrap();
        assert_eq!(dry.old_hash, live.old_hash);
        assert_eq!(dry.new_hash, live.new_hash);
    }

    #[tokio::test]
    async fn batch_execute_one_atomic_commit_all_op_types() {
        let (tmp, tools) = setup().await;
        // Seed for delete + move + update-links.
        tools.write_file("seed_del.md", "gone").await.unwrap();
        tools.write_file("seed_mv.md", "moveme").await.unwrap();
        tools
            .write_file("links.md", "see [[old-target]]")
            .await
            .unwrap();
        let head_before = head_oid(&tools);

        let ops = vec![
            BatchOperation::CreateNote {
                path: "new1.md".into(),
                content: "C1".into(),
            },
            BatchOperation::WriteNote {
                path: "new2.md".into(),
                content: "W2".into(),
            },
            BatchOperation::DeleteNote {
                path: "seed_del.md".into(),
            },
            BatchOperation::MoveNote {
                from: "seed_mv.md".into(),
                to: "moved.md".into(),
            },
            BatchOperation::UpdateLinks {
                file: "links.md".into(),
                old_target: "old-target".into(),
                new_target: "new-target".into(),
            },
        ];

        let res = tools.batch_execute(ops).await.unwrap();
        assert!(res.success, "batch should succeed: {:?}", res.errors);
        assert_eq!(res.executed, 5);

        // HEAD advanced by exactly one commit (the batch landed as one txn).
        let head_after = head_oid(&tools).unwrap();
        assert_ne!(Some(head_after), head_before);
        let repo = git2::Repository::open(tmp.path()).unwrap();
        let commit = repo.find_commit(head_after).unwrap();
        assert_eq!(commit.parent_count(), 1, "exactly one new commit");

        // Filesystem state matches.
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("new1.md")).unwrap(),
            "C1"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("new2.md")).unwrap(),
            "W2"
        );
        assert!(!tmp.path().join("seed_del.md").exists());
        assert!(!tmp.path().join("seed_mv.md").exists());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("moved.md")).unwrap(),
            "moveme"
        );
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("links.md")).unwrap(),
            "see [[new-target]]"
        );
    }

    #[tokio::test]
    async fn batch_execute_failure_leaves_no_partial_state() {
        // CreateNote on a path that already exists -> precondition abort.
        // Atomicity contract: zero files from the batch should land.
        let (tmp, tools) = setup().await;
        tools.write_file("exists.md", "already").await.unwrap();
        let head_before = head_oid(&tools);

        let ops = vec![
            BatchOperation::WriteNote {
                path: "untouched1.md".into(),
                content: "X".into(),
            },
            BatchOperation::CreateNote {
                path: "exists.md".into(),
                content: "boom".into(),
            },
            BatchOperation::WriteNote {
                path: "untouched2.md".into(),
                content: "Y".into(),
            },
        ];

        let res = tools.batch_execute(ops).await.unwrap();
        assert!(!res.success);
        // Neither untouched file was written; the existing file is unchanged.
        assert!(!tmp.path().join("untouched1.md").exists());
        assert!(!tmp.path().join("untouched2.md").exists());
        assert_eq!(tools.read_file("exists.md").await.unwrap(), "already");
        assert_eq!(head_oid(&tools), head_before, "no commit on abort");
    }

    #[tokio::test]
    async fn batch_execute_empty_is_a_loud_failure() {
        let (_tmp, tools) = setup().await;
        let res = tools.batch_execute(vec![]).await.unwrap();
        assert!(!res.success);
        assert_eq!(res.total, 0);
    }

    // -------- GWS.14b: CAS-collision flush --------

    #[tokio::test]
    async fn cas_collision_flush_fires_before_concurrency_error_returns() {
        // Wire a sentinel flush callback that flips an Arc<AtomicBool> so we
        // can prove flush ran BEFORE the error reached the caller.
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let commit_hook: CommitHook = Arc::new(|_p, _c| {});

        let flushed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flushed_clone = Arc::clone(&flushed);
        let flush: CasCollisionFlush = Arc::new(move || {
            let f = Arc::clone(&flushed_clone);
            Box::pin(async move {
                f.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        });

        let tools = GitFileTools::new_with_hook_and_flush(
            manager,
            tmp.path().to_path_buf(),
            locks,
            commit_hook,
            flush,
        );

        // Trigger a guaranteed precondition failure: write v1, then update
        // with a stale expected blob.
        tools.write_file("a.md", "v1").await.unwrap();
        let stale_oid = VaultRepo::blob_oid_of(b"WAS_NEVER_HERE").unwrap();
        let err = tools
            .write_file_with_mode(
                "a.md",
                "v2",
                WriteMode::Overwrite,
                Some(&stale_oid.to_string()),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
        assert!(
            flushed.load(std::sync::atomic::Ordering::SeqCst),
            "flush callback must fire before the ConcurrencyError surfaces to the caller"
        );
    }

    #[tokio::test]
    async fn cas_collision_flush_skipped_on_successful_write() {
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let commit_hook: CommitHook = Arc::new(|_p, _c| {});

        let flush_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let flush_calls_clone = Arc::clone(&flush_calls);
        let flush: CasCollisionFlush = Arc::new(move || {
            let c = Arc::clone(&flush_calls_clone);
            Box::pin(async move {
                c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        });

        let tools = GitFileTools::new_with_hook_and_flush(
            manager,
            tmp.path().to_path_buf(),
            locks,
            commit_hook,
            flush,
        );

        tools.write_file("a.md", "alpha").await.unwrap();
        tools.write_file("b.md", "beta").await.unwrap();
        assert_eq!(
            flush_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "flush only fires on ConcurrencyError, never on successful writes"
        );
    }

    #[tokio::test]
    async fn cas_collision_flush_error_does_not_mask_original_concurrency_error() {
        // Even when the flush callback itself errors, the caller still sees
        // the original ConcurrencyError — flush failures are logged + dropped
        // (correctness contract).
        let tmp = TempDir::new().unwrap();
        init_repo(tmp.path());
        let manager = Arc::new(VaultManager::new(test_server_config(tmp.path())).unwrap());
        let locks = Arc::new(CommitLocks::new());
        let commit_hook: CommitHook = Arc::new(|_p, _c| {});

        let flush: CasCollisionFlush =
            Arc::new(|| Box::pin(async { Err(Error::config_error("simulated flush failure")) }));

        let tools = GitFileTools::new_with_hook_and_flush(
            manager,
            tmp.path().to_path_buf(),
            locks,
            commit_hook,
            flush,
        );

        tools.write_file("a.md", "v1").await.unwrap();
        let stale_oid = VaultRepo::blob_oid_of(b"WAS_NEVER_HERE").unwrap();
        let err = tools
            .write_file_with_mode(
                "a.md",
                "v2",
                WriteMode::Overwrite,
                Some(&stale_oid.to_string()),
            )
            .await
            .unwrap_err();
        // Caller sees the original ConcurrencyError, NOT the flush's
        // ConfigError. Flush failures are best-effort.
        assert!(
            matches!(err, Error::ConcurrencyError { .. }),
            "got: {err:?}"
        );
    }
}
