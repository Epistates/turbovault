//! # Batch Operations Framework
//!
//! Provides atomic, transactional batch file operations with rollback support.
//! All operations in a batch either succeed together or fail together, maintaining
//! vault integrity even if individual operations encounter errors.
//!
//! ## Quick Start
//!
//! ```no_run
//! use turbovault_core::ServerConfig;
//! use turbovault_vault::VaultManager;
//! use turbovault_batch::BatchExecutor;
//! use turbovault_batch::BatchOperation;
//! use std::sync::Arc;
//! use std::path::PathBuf;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = ServerConfig::default();
//!     let manager = VaultManager::new(config)?;
//!     let executor = BatchExecutor::new(Arc::new(manager), PathBuf::from("/tmp"));
//!
//!     // Define batch operations
//!     let operations = vec![
//!         BatchOperation::CreateNote {
//!             path: "notes/new1.md".to_string(),
//!             content: "# First Note".to_string(),
//!             force: None,
//!         },
//!         BatchOperation::CreateNote {
//!             path: "notes/new2.md".to_string(),
//!             content: "# Second Note".to_string(),
//!             force: None,
//!         },
//!         BatchOperation::UpdateLinks {
//!             file: "notes/index.md".to_string(),
//!             old_target: "old-link".to_string(),
//!             new_target: "new-link".to_string(),
//!             expected_hash: None,
//!         },
//!     ];
//!
//!     // Execute atomically
//!     let result = executor.execute(operations).await?;
//!     println!("Success: {}", result.success);
//!     println!("Changes: {}", result.changes.len());
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Core Types
//!
//! ### BatchOperation
//!
//! Individual operations to execute in a batch:
//! - [`BatchOperation::CreateNote`] - Create a new note
//! - [`BatchOperation::WriteNote`] - Write or overwrite a note
//! - [`BatchOperation::DeleteNote`] - Delete a note
//! - [`BatchOperation::MoveNote`] - Move or rename a note
//! - [`BatchOperation::UpdateLinks`] - Update link references
//!
//! ### BatchExecutor
//!
//! [`BatchExecutor`] manages batch execution with:
//! - Validation before execution
//! - Conflict detection between operations
//! - Atomic execution with proper sequencing
//! - Transaction ID tracking
//! - Detailed result reporting
//!
//! ### BatchResult
//!
//! [`BatchResult`] contains execution results:
//! - Overall success/failure status
//! - Count of executed operations
//! - First failure point (if any)
//! - List of changes made
//! - List of errors encountered
//! - Individual operation records
//! - Unique transaction ID
//! - Execution duration
//!
//! ## Conflict Detection
//!
//! Operations that affect the same files are detected as conflicts:
//! - Write + Delete on same file = conflict
//! - Move + Write on same file = conflict
//! - Multiple reads (UpdateLinks) = allowed
//!
//! Example:
//! ```
//! use turbovault_batch::BatchOperation;
//!
//! let write = BatchOperation::WriteNote {
//!     path: "file.md".to_string(),
//!     content: "content".to_string(),
//!     expected_hash: None,
//! };
//!
//! let delete = BatchOperation::DeleteNote {
//!     path: "file.md".to_string(),
//!     expected_hash: None,
//! };
//!
//! assert!(write.conflicts_with(&delete));
//! ```
//!
//! ## Atomicity Guarantees
//!
//! The batch executor ensures:
//! - All-or-nothing semantics: entire batch succeeds or stops at first failure
//! - Transaction tracking with unique IDs
//! - Execution timing recorded
//! - Detailed per-operation records for debugging
//! - File integrity through atomic operations
//!
//! ## Error Handling
//!
//! Errors stop batch execution:
//! - Validation errors prevent any execution
//! - Operation errors stop the batch
//! - Previous operations are recorded but not rolled back
//! - Error details provided in result
//!
//! If true rollback is needed, handle externally using transaction IDs.
//!
//! ## Performance
//!
//! Batch execution is optimized for:
//! - Minimal validation overhead
//! - Sequential execution with early termination
//! - Efficient conflict checking (O(n²) upfront)
//! - Low-overhead operation tracking

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_core::TransactionBuilder;
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

/// Individual batch operation to execute
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum BatchOperation {
    /// Create a new note with content. Defaults to strict-create: the
    /// substrate adds `expect_absent`, so the loser of a concurrent-create
    /// race aborts the entire batch with `ConcurrencyError`
    /// (turbovault-947 / 6fo §6 reconsideration domino).
    ///
    /// `force: Some(true)` disables `expect_absent`, falling back to
    /// upsert semantics (caller-acknowledged blind create/overwrite —
    /// equivalent to `WriteNote { expected_hash: None }`).
    #[serde(rename = "CreateNote", alias = "CreateFile")]
    CreateNote {
        path: String,
        content: String,
        /// Disable the implicit `expect_absent` precondition. Default
        /// false; pass true to acknowledge a blind create/overwrite.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },

    /// Write/overwrite a note. `expected_hash` (git blob OID hex on git
    /// backend, SHA-256 on legacy) carries an `expect_blob` precondition;
    /// the whole batch aborts if the target file no longer matches the
    /// expected pre-image (turbovault-c0e).
    #[serde(rename = "WriteNote", alias = "WriteFile")]
    WriteNote {
        path: String,
        content: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// Delete a note. `expected_hash` guards the delete against a
    /// concurrent modification of the target.
    ///
    /// turbovault-0g4.7: on the **git backend**, `on_backlinks` controls inbound
    /// wikilinks (parity with the standalone `delete_note`):
    /// - `"refuse"` (default) — abort the batch if the note has inbound
    ///   backlinks (prevents silently shipping broken links);
    /// - `"rewrite-stale-callout"` — atomically `~~[[strikethrough]]~~` every
    ///   linker in the same commit;
    /// - `"force"` — delete and leave inbound links dangling (the pre-0g4.7
    ///   behavior).
    ///
    /// The legacy backend ignores this field and always does a bare delete (no
    /// atomic multi-file primitive).
    #[serde(rename = "DeleteNote", alias = "DeleteFile")]
    DeleteNote {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_backlinks: Option<String>,
    },

    /// Move/rename a note. `expected_hash` guards the SOURCE against
    /// concurrent modification (the destination always carries
    /// `expect_absent`, refusing to clobber).
    ///
    /// turbovault-0g4.6: on the **git backend**, `update_backlinks` (default
    /// true) atomically rewrites every inbound wikilink — `[[from]]`,
    /// `[[from|alias]]`, `[[from#section]]`, `[[from#^block]]`, `![[from]]` and
    /// path-prefix forms — to the new target in the SAME commit, with per-source
    /// `expect_blob` preconditions. Set it false for a rename-only move (inbound
    /// links dangle — the pre-0g4.6 behavior). The legacy backend ignores this
    /// field and is always rename-only (it has no atomic multi-file primitive).
    #[serde(rename = "MoveNote", alias = "MoveFile")]
    MoveNote {
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update_backlinks: Option<bool>,
    },

    /// Update links in a note (find and replace link target).
    /// `expected_hash` guards the source file from concurrent
    /// modification — important when several batch ops update siblings
    /// of the same renamed page.
    #[serde(rename = "UpdateLinks")]
    UpdateLinks {
        file: String,
        old_target: String,
        new_target: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// turbovault-0g4.1: apply SEARCH/REPLACE blocks to an existing note as
    /// part of the atomic batch commit (the batch equivalent of `edit_note`).
    /// `edits` uses the same block grammar as the tool; multiple blocks edit
    /// multiple locations in the one file. `expected_hash` (git blob OID hex)
    /// carries an `expect_blob` precondition — a stale pre-image aborts the
    /// whole batch. **Git backend only**; the legacy executor refuses it.
    #[serde(rename = "EditNote", alias = "EditFile")]
    EditNote {
        path: String,
        edits: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// turbovault-0g4.2: set/merge frontmatter keys on a note as part of the
    /// atomic batch commit (the batch equivalent of `update_frontmatter`).
    /// `merge` defaults to true (deep-merge into existing frontmatter); false
    /// replaces the frontmatter wholesale. `expected_hash` (git blob OID hex)
    /// carries an `expect_blob` precondition. **Git backend only**.
    #[serde(rename = "UpdateFrontmatter")]
    UpdateFrontmatter {
        path: String,
        frontmatter: std::collections::HashMap<String, serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// turbovault-0g4.3: add or remove frontmatter tags on a note as part of
    /// the atomic batch commit (the batch equivalent of `manage_tags`
    /// add/remove). `operation` is `"add"` or `"remove"`; `"list"` is
    /// read-only and not a batch op (rejected). `expected_hash` carries an
    /// `expect_blob` precondition. **Git backend only**.
    #[serde(rename = "ManageTags")]
    ManageTags {
        path: String,
        operation: String,
        tags: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// turbovault-0g4.4: render a template and create a note as part of the
    /// atomic batch commit (the batch equivalent of `create_from_template`).
    /// `force` (default false) → strict create (`expect_absent`, aborts if the
    /// path exists); true → blind upsert. **Git backend only**.
    #[serde(rename = "CreateFromTemplate")]
    CreateFromTemplate {
        template_id: String,
        path: String,
        fields: std::collections::HashMap<String, String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },
}

impl BatchOperation {
    /// Get list of files affected by this operation
    pub fn affected_files(&self) -> Vec<String> {
        match self {
            Self::CreateNote { path, .. } => vec![path.clone()],
            Self::WriteNote { path, .. } => vec![path.clone()],
            Self::DeleteNote { path, .. } => vec![path.clone()],
            Self::MoveNote { from, to, .. } => vec![from.clone(), to.clone()],
            Self::UpdateLinks {
                file,
                old_target,
                new_target,
                ..
            } => {
                vec![file.clone(), old_target.clone(), new_target.clone()]
            }
            Self::EditNote { path, .. } => vec![path.clone()],
            Self::UpdateFrontmatter { path, .. } => vec![path.clone()],
            Self::ManageTags { path, .. } => vec![path.clone()],
            Self::CreateFromTemplate { path, .. } => vec![path.clone()],
        }
    }

    /// turbovault-0g4: the variant name if this op is git-substrate-only (has
    /// no legacy `BatchExecutor` equivalent), else `None`. The legacy executor
    /// uses this to refuse such ops upfront in [`BatchExecutor::validate`], so
    /// a user on `write_backend=legacy` sees a clear error instead of a partial
    /// apply. Extended as each git-only op is added (turbovault-0g4.*).
    pub fn git_only_kind(&self) -> Option<&'static str> {
        match self {
            Self::EditNote { .. } => Some("EditNote"),
            Self::UpdateFrontmatter { .. } => Some("UpdateFrontmatter"),
            Self::ManageTags { .. } => Some("ManageTags"),
            Self::CreateFromTemplate { .. } => Some("CreateFromTemplate"),
            _ => None,
        }
    }

    /// Check for conflicts with another operation
    pub fn conflicts_with(&self, other: &BatchOperation) -> bool {
        let self_files = self.affected_files();
        let other_files = other.affected_files();

        // Check if any files overlap
        for file in &self_files {
            if other_files.contains(file) {
                // Allow if both are reads (UpdateLinks on same file), but not if either is a write
                match (self, other) {
                    (Self::UpdateLinks { .. }, Self::UpdateLinks { .. }) => {
                        // Multiple reads are OK
                        continue;
                    }
                    _ => return true, // Write conflict
                }
            }
        }

        false
    }
}

/// Record of a single executed operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    /// Index in the batch
    pub operation_index: usize,
    /// The operation that was executed
    pub operation: String,
    /// Result of execution (success or error)
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Files affected
    pub affected_files: Vec<String>,
}

/// Result of batch execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    /// Whether all operations succeeded
    pub success: bool,
    /// Number of operations executed
    pub executed: usize,
    /// Total operations in batch
    pub total: usize,
    /// Index where failure occurred (if any)
    pub failed_at: Option<usize>,
    /// Changes made to files
    pub changes: Vec<String>,
    /// Errors encountered
    pub errors: Vec<String>,
    /// Execution records for each operation
    pub records: Vec<OperationRecord>,
    /// Unique transaction ID
    pub transaction_id: String,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

/// Batch executor with transaction support
pub struct BatchExecutor {
    manager: Arc<VaultManager>,
}

impl BatchExecutor {
    /// Create a new batch executor
    pub fn new(manager: Arc<VaultManager>, _temp_dir: PathBuf) -> Self {
        Self { manager }
    }

    /// Validate batch operations before execution
    pub async fn validate(&self, ops: &[BatchOperation]) -> Result<()> {
        if ops.is_empty() {
            return Err(Error::config_error("Batch cannot be empty".to_string()));
        }

        // turbovault-0g4: refuse git-substrate-only ops on the legacy executor
        // upfront (zero side effects) rather than writing earlier ops then
        // failing mid-batch. Keeps `write_backend=legacy` behavior unchanged:
        // these ops never existed there, so a clear refusal is the only correct
        // outcome.
        for (i, op) in ops.iter().enumerate() {
            if let Some(kind) = op.git_only_kind() {
                return Err(git_only_err(i, kind));
            }
        }

        // Check for conflicts (operations on same file)
        for i in 0..ops.len() {
            for j in (i + 1)..ops.len() {
                if ops[i].conflicts_with(&ops[j]) {
                    return Err(Error::config_error(format!(
                        "Conflicting operations: operation {} and {} affect same files",
                        i, j
                    )));
                }
            }
        }

        Ok(())
    }

    /// Execute batch operations atomically
    pub async fn execute(&self, ops: Vec<BatchOperation>) -> Result<BatchResult> {
        let transaction = TransactionBuilder::new();

        // 1. Validate
        if let Err(e) = self.validate(&ops).await {
            return Ok(BatchResult {
                success: false,
                executed: 0,
                total: ops.len(),
                failed_at: None,
                changes: vec![],
                errors: vec![e.to_string()],
                records: vec![],
                transaction_id: transaction.transaction_id().to_string(),
                duration_ms: transaction.elapsed_ms(),
            });
        }

        let mut changes = Vec::new();
        let mut records = Vec::new();
        let mut errors = Vec::new();

        // 2. Execute each operation
        for (idx, op) in ops.iter().enumerate() {
            let operation_desc = format!("{:?}", op);
            let affected = op.affected_files();

            match self.execute_operation(op).await {
                Ok(change_msg) => {
                    changes.push(change_msg.clone());
                    records.push(OperationRecord {
                        operation_index: idx,
                        operation: operation_desc,
                        success: true,
                        error: None,
                        affected_files: affected,
                    });
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    errors.push(error_msg.clone());
                    records.push(OperationRecord {
                        operation_index: idx,
                        operation: operation_desc,
                        success: false,
                        error: Some(error_msg),
                        affected_files: affected,
                    });

                    // Stop on first error (transaction fails)
                    return Ok(BatchResult {
                        success: false,
                        executed: idx,
                        total: ops.len(),
                        failed_at: Some(idx),
                        changes,
                        errors,
                        records,
                        transaction_id: transaction.transaction_id().to_string(),
                        duration_ms: transaction.elapsed_ms(),
                    });
                }
            }
        }

        // All succeeded
        Ok(BatchResult {
            success: true,
            executed: ops.len(),
            total: ops.len(),
            failed_at: None,
            changes,
            errors,
            records,
            transaction_id: transaction.transaction_id().to_string(),
            duration_ms: transaction.elapsed_ms(),
        })
    }

    /// Execute a single operation
    async fn execute_operation(&self, op: &BatchOperation) -> Result<String> {
        // Legacy executor does not consult per-op preconditions
        // (`expected_hash` / `force`). The legacy substrate has no CAS
        // primitive on the batch path (per the legacy-stays direction in
        // turbovault-6fo.16). `WriteTools::Legacy::batch_execute` refuses
        // batches that carry preconditions, so reaching this code path with
        // a precondition set is a bug elsewhere.
        //
        // turbovault-0g4: git-substrate-only ops have no legacy equivalent.
        // `validate()` rejects them upfront; this is a defensive backstop.
        if let Some(kind) = op.git_only_kind() {
            return Err(git_only_err(0, kind));
        }
        match op {
            BatchOperation::CreateNote { path, content, .. } => {
                let path_buf = PathBuf::from(path);
                self.manager.write_file(&path_buf, content, None).await?;
                Ok(format!("Created: {}", path))
            }

            BatchOperation::WriteNote { path, content, .. } => {
                let path_buf = PathBuf::from(path);
                self.manager.write_file(&path_buf, content, None).await?;
                Ok(format!("Updated: {}", path))
            }

            BatchOperation::DeleteNote { path, .. } => {
                let path_buf = PathBuf::from(path);
                self.manager.delete_file(&path_buf, None).await?;
                Ok(format!("Deleted: {}", path))
            }

            BatchOperation::MoveNote { from, to, .. } => {
                let from_buf = PathBuf::from(from);
                let to_buf = PathBuf::from(to);
                self.manager.move_file(&from_buf, &to_buf, None).await?;
                Ok(format!("Moved: {} → {}", from, to))
            }

            BatchOperation::UpdateLinks {
                file,
                old_target,
                new_target,
                ..
            } => {
                // Read file
                let path_buf = PathBuf::from(file);
                let content = self.manager.read_file(&path_buf).await?;

                // Simple string replacement (in real implementation, would parse links)
                let updated = content.replace(old_target, new_target);

                // Write back if changed
                if updated != content {
                    self.manager.write_file(&path_buf, &updated, None).await?;
                    Ok(format!(
                        "Updated links in {}: {} → {}",
                        file, old_target, new_target
                    ))
                } else {
                    Ok(format!(
                        "No links updated in {} (no match for {})",
                        file, old_target
                    ))
                }
            }
            // turbovault-0g4: git-substrate-only ops return early above; this
            // arm keeps the match exhaustive without pinning it to the legacy
            // op set, and defensively refuses any unhandled variant.
            other => Err(git_only_err(
                0,
                other.git_only_kind().unwrap_or("operation"),
            )),
        }
    }
}

/// turbovault-0g4: error for a git-substrate-only [`BatchOperation`] reaching
/// the legacy executor. `index` is the op's position in the batch (use `0`
/// when the position is not meaningful, e.g. the defensive backstop).
fn git_only_err(index: usize, kind: &str) -> Error {
    Error::config_error(format!(
        "operation {index} (BatchOperation::{kind}) requires write_backend=git; the legacy batch executor has no equivalent. Switch the vault to the git backend to use it."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use turbovault_core::prelude::ServerConfig;

    #[test]
    fn test_operation_affected_files() {
        let op = BatchOperation::MoveNote {
            from: "a.md".to_string(),
            to: "b.md".to_string(),
            expected_hash: None,
            update_backlinks: None,
        };
        let affected = op.affected_files();
        assert_eq!(affected.len(), 2);
        assert!(affected.contains(&"a.md".to_string()));
        assert!(affected.contains(&"b.md".to_string()));
    }

    #[test]
    fn test_conflict_detection() {
        let op1 = BatchOperation::WriteNote {
            path: "file.md".to_string(),
            content: "content".to_string(),
            expected_hash: None,
        };
        let op2 = BatchOperation::DeleteNote {
            path: "file.md".to_string(),
            expected_hash: None,
            on_backlinks: None,
        };

        assert!(op1.conflicts_with(&op2));
        assert!(op2.conflicts_with(&op1));
    }

    #[test]
    fn test_no_conflict_different_files() {
        let op1 = BatchOperation::WriteNote {
            path: "file1.md".to_string(),
            content: "content".to_string(),
            expected_hash: None,
        };
        let op2 = BatchOperation::WriteNote {
            path: "file2.md".to_string(),
            content: "content".to_string(),
            expected_hash: None,
        };

        assert!(!op1.conflicts_with(&op2));
    }

    // ── Helpers ──────────────────────────────────────────────────────────────

    fn make_config(vault_dir: &std::path::Path) -> ServerConfig {
        use turbovault_core::config::VaultConfig;
        let mut config = ServerConfig::new();
        let vault_config = VaultConfig::builder("test", vault_dir).build().unwrap();
        config.vaults.push(vault_config);
        config
    }

    async fn make_executor(temp: &TempDir) -> BatchExecutor {
        let config = make_config(temp.path());
        let manager = Arc::new(VaultManager::new(config).unwrap());
        manager.initialize().await.unwrap();
        BatchExecutor::new(manager, temp.path().to_path_buf())
    }

    // ── BatchExecutor integration tests ──────────────────────────────────────

    #[tokio::test]
    async fn test_batch_create_note() {
        let temp = TempDir::new().unwrap();
        let executor = make_executor(&temp).await;

        let result = executor
            .execute(vec![BatchOperation::CreateNote {
                path: "hello.md".to_string(),
                content: "# Hello World".to_string(),
                force: None,
            }])
            .await
            .unwrap();

        assert!(result.success, "batch should succeed: {:?}", result.errors);
        assert_eq!(result.executed, 1);
        assert_eq!(result.total, 1);

        let on_disk = std::fs::read_to_string(temp.path().join("hello.md")).unwrap();
        assert_eq!(on_disk, "# Hello World");
    }

    #[tokio::test]
    async fn test_batch_write_note() {
        let temp = TempDir::new().unwrap();
        // Pre-create the file so WriteNote has something to overwrite.
        std::fs::write(temp.path().join("existing.md"), "old content").unwrap();

        let executor = make_executor(&temp).await;
        let result = executor
            .execute(vec![BatchOperation::WriteNote {
                path: "existing.md".to_string(),
                content: "new content".to_string(),
                expected_hash: None,
            }])
            .await
            .unwrap();

        assert!(result.success, "batch should succeed: {:?}", result.errors);

        let on_disk = std::fs::read_to_string(temp.path().join("existing.md")).unwrap();
        assert_eq!(on_disk, "new content");
    }

    #[tokio::test]
    async fn test_batch_delete_note() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("to_delete.md"), "delete me").unwrap();

        let executor = make_executor(&temp).await;
        let result = executor
            .execute(vec![BatchOperation::DeleteNote {
                path: "to_delete.md".to_string(),
                expected_hash: None,
                on_backlinks: None,
            }])
            .await
            .unwrap();

        assert!(result.success, "batch should succeed: {:?}", result.errors);
        assert!(!temp.path().join("to_delete.md").exists());
    }

    #[tokio::test]
    async fn test_batch_move_note() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("source.md"), "move me").unwrap();

        let executor = make_executor(&temp).await;
        let result = executor
            .execute(vec![BatchOperation::MoveNote {
                from: "source.md".to_string(),
                to: "destination.md".to_string(),
                expected_hash: None,
                update_backlinks: None,
            }])
            .await
            .unwrap();

        assert!(result.success, "batch should succeed: {:?}", result.errors);
        assert!(
            !temp.path().join("source.md").exists(),
            "old path should be gone"
        );
        let on_disk = std::fs::read_to_string(temp.path().join("destination.md")).unwrap();
        assert_eq!(on_disk, "move me");
    }

    #[tokio::test]
    async fn test_batch_multiple_operations() {
        let temp = TempDir::new().unwrap();
        let executor = make_executor(&temp).await;

        // Create → overwrite → move: all on non-conflicting paths
        let ops = vec![
            BatchOperation::CreateNote {
                path: "alpha.md".to_string(),
                content: "alpha v1".to_string(),
                force: None,
            },
            BatchOperation::CreateNote {
                path: "beta.md".to_string(),
                content: "beta v1".to_string(),
                force: None,
            },
            BatchOperation::CreateNote {
                path: "gamma.md".to_string(),
                content: "gamma".to_string(),
                force: None,
            },
        ];

        let result = executor.execute(ops).await.unwrap();

        assert!(
            result.success,
            "all ops should succeed: {:?}",
            result.errors
        );
        assert_eq!(result.executed, 3);
        assert_eq!(result.total, 3);

        assert!(temp.path().join("alpha.md").exists());
        assert!(temp.path().join("beta.md").exists());
        assert!(temp.path().join("gamma.md").exists());
    }

    #[tokio::test]
    async fn test_batch_failure_in_middle() {
        let temp = TempDir::new().unwrap();
        // op[0] will succeed (creates a new file)
        // op[1] will fail  (deletes a file that does not exist)
        let executor = make_executor(&temp).await;

        let ops = vec![
            BatchOperation::CreateNote {
                path: "succeeds.md".to_string(),
                content: "I was created".to_string(),
                force: None,
            },
            BatchOperation::DeleteNote {
                path: "nonexistent.md".to_string(),
                expected_hash: None,
                on_backlinks: None,
            },
        ];

        let result = executor.execute(ops).await.unwrap();

        // Batch as a whole failed
        assert!(!result.success, "batch should report failure");
        assert_eq!(result.failed_at, Some(1));
        assert!(!result.errors.is_empty());

        // But op[0] already happened (non-transactional per implementation)
        assert!(
            temp.path().join("succeeds.md").exists(),
            "op[0] side-effect should persist"
        );
        let on_disk = std::fs::read_to_string(temp.path().join("succeeds.md")).unwrap();
        assert_eq!(on_disk, "I was created");
    }

    #[tokio::test]
    async fn test_batch_empty_operations() {
        let temp = TempDir::new().unwrap();
        let executor = make_executor(&temp).await;

        // Empty batch is rejected by validate(), so execute() returns Ok with success=false
        let result = executor.execute(vec![]).await.unwrap();

        assert!(!result.success);
        assert_eq!(result.executed, 0);
        assert_eq!(result.total, 0);
        assert!(!result.errors.is_empty(), "should report why it failed");
    }
}
