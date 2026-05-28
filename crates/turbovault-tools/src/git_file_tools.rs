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
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_batch::{BatchOperation, BatchResult, OperationRecord};
use turbovault_core::prelude::*;
use turbovault_git::{CommitLocks, Oid, Transaction, VaultRepo};
use turbovault_vault::{EditEngine, EditResult, VaultManager};

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
}

impl GitFileTools {
    /// Construct the git-backed write surface. `manager` is used only for
    /// reads and validation helpers (paths, link graph). `vault_path` and
    /// `commit_locks` together identify the git repo + its in-process
    /// commit-section mutex registry (one per worktree).
    pub fn new(
        manager: Arc<VaultManager>,
        vault_path: PathBuf,
        commit_locks: Arc<CommitLocks>,
    ) -> Self {
        Self {
            manager,
            vault_path,
            commit_locks,
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
        let final_content = match mode {
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
        };

        let expected = parse_blob_oid(expected_hash)?;
        let txn = build_upsert_txn(
            format!("write_file {}", path),
            path,
            &final_content,
            expected,
        );
        self.apply_txn(&txn).await
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
        let (result, new_content) = engine.apply_edits(&current, &blocks, dry_run)?;

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

        match self.apply_txn(&txn).await {
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

    async fn apply_txn(&self, txn: &Transaction) -> Result<()> {
        // `VaultRepo` is `Send` but `!Sync`; the substrate work is blocking
        // libgit2. Move it to the blocking pool. The `Arc<CommitLocks>` is
        // shared across calls so cross-call commit-section serialization
        // survives even though we open a fresh `VaultRepo` per call.
        let path = self.vault_path.clone();
        let locks = Arc::clone(&self.commit_locks);
        let txn = txn.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let repo = VaultRepo::open_with_locks(&path, locks).map_err(git_err_to_core)?;
            repo.apply_transaction(&txn)
                .map(|_| ())
                .map_err(git_err_to_core)
        })
        .await
        .map_err(|e| Error::config_error(format!("git transaction task failed: {}", e)))?
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
            Error::config_error(format!(
                "expected_hash for git backend must be a 40-char git blob oid hex (got: {:?})",
                hex
            ))
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
    async fn write_file_with_garbage_hash_is_loud_error() {
        let (_tmp, tools) = setup().await;
        let err = tools
            .write_file_with_mode("a.md", "v1", WriteMode::Overwrite, Some("not-a-hash"))
            .await
            .unwrap_err();
        // Loud configuration error, not silent drop.
        assert!(matches!(err, Error::ConfigError { .. }), "got: {err:?}");
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
}
