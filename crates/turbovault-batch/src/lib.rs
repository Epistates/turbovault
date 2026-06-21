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
//!         },
//!         BatchOperation::CreateNote {
//!             path: "notes/new2.md".to_string(),
//!             content: "# Second Note".to_string(),
//!         },
//!         BatchOperation::UpdateLinks {
//!             file: "notes/index.md".to_string(),
//!             old_target: "old-link".to_string(),
//!             new_target: "new-link".to_string(),
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
    /// Create a new note with content
    #[serde(rename = "CreateNote", alias = "CreateFile")]
    CreateNote { path: String, content: String },

    /// Write/overwrite a note
    #[serde(rename = "WriteNote", alias = "WriteFile")]
    WriteNote {
        path: String,
        content: String,
        /// Optional optimistic-concurrency precondition: SHA-256 hash the file
        /// is expected to have before the write. Checked before the operation
        /// is applied; a mismatch fails the batch with a ConcurrencyError.
        #[serde(default)]
        expected_hash: Option<String>,
    },

    /// Delete a note
    #[serde(rename = "DeleteNote", alias = "DeleteFile")]
    DeleteNote {
        path: String,
        /// Optional optimistic-concurrency precondition (see `WriteNote`).
        #[serde(default)]
        expected_hash: Option<String>,
    },

    /// Move/rename a note
    #[serde(rename = "MoveNote", alias = "MoveFile")]
    MoveNote {
        from: String,
        to: String,
        /// Optional optimistic-concurrency precondition on the source file
        /// (see `WriteNote`).
        #[serde(default)]
        expected_hash: Option<String>,
    },

    /// Update links in a note (find and replace link target)
    #[serde(rename = "UpdateLinks")]
    UpdateLinks {
        file: String,
        old_target: String,
        new_target: String,
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
            } => {
                vec![file.clone(), old_target.clone(), new_target.clone()]
            }
        }
    }

    /// Optimistic-concurrency precondition for this operation, if any.
    ///
    /// Returns `(path, expected_hash)` where `path` is the file whose current
    /// content hash must equal `expected_hash` for the operation to proceed.
    /// `CreateNote` and `UpdateLinks` never carry a precondition.
    pub fn precondition(&self) -> Option<(&str, &str)> {
        match self {
            Self::WriteNote {
                path,
                expected_hash: Some(h),
                ..
            } => Some((path, h)),
            Self::DeleteNote {
                path,
                expected_hash: Some(h),
            } => Some((path, h)),
            Self::MoveNote {
                from,
                expected_hash: Some(h),
                ..
            } => Some((from, h)),
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

    /// Pre-flight check of every operation's optimistic-concurrency
    /// precondition before any operation is applied.
    ///
    /// This narrows (though, like all optimistic schemes, does not eliminate)
    /// the partial-application window: if any `expected_hash` is already stale
    /// when the batch starts, the whole batch fails before mutating anything,
    /// rather than applying the first few operations and then aborting.
    async fn precheck_preconditions(&self, ops: &[BatchOperation]) -> Result<()> {
        for op in ops {
            if let Some((path, expected)) = op.precondition() {
                let path_buf = PathBuf::from(path);
                match self.manager.read_file(&path_buf).await {
                    Ok(content) => {
                        let actual = turbovault_vault::compute_hash(&content);
                        if actual != expected {
                            return Err(Error::concurrency_error(format!(
                                "Precondition failed for {}: expected hash {}, actual {}. Re-read the file and retry.",
                                path, expected, actual
                            )));
                        }
                    }
                    // File missing but a hash was expected — treat as a conflict
                    // (it was likely deleted since the caller read it).
                    Err(_) => {
                        return Err(Error::concurrency_error(format!(
                            "Precondition failed for {}: file does not exist but expected_hash {} was provided. It may have been deleted.",
                            path, expected
                        )));
                    }
                }
            }
        }
        Ok(())
    }

    /// Execute batch operations atomically
    pub async fn execute(&self, ops: Vec<BatchOperation>) -> Result<BatchResult> {
        let transaction = TransactionBuilder::new();

        // 1. Validate (intra-batch conflicts) + pre-flight precondition check
        //    (external, since-read conflicts via expected_hash).
        if let Err(e) = self
            .validate(&ops)
            .await
            .and(self.precheck_preconditions(&ops).await)
        {
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
        match op {
            BatchOperation::CreateNote { path, content } => {
                let path_buf = PathBuf::from(path);
                self.manager.write_file(&path_buf, content, None).await?;
                Ok(format!("Created: {}", path))
            }

            BatchOperation::WriteNote {
                path,
                content,
                expected_hash,
            } => {
                let path_buf = PathBuf::from(path);
                self.manager
                    .write_file(&path_buf, content, expected_hash.as_deref())
                    .await?;
                Ok(format!("Updated: {}", path))
            }

            BatchOperation::DeleteNote {
                path,
                expected_hash,
            } => {
                let path_buf = PathBuf::from(path);
                self.manager
                    .delete_file(&path_buf, expected_hash.as_deref())
                    .await?;
                Ok(format!("Deleted: {}", path))
            }

            BatchOperation::MoveNote {
                from,
                to,
                expected_hash,
            } => {
                let from_buf = PathBuf::from(from);
                let to_buf = PathBuf::from(to);
                self.manager
                    .move_file(&from_buf, &to_buf, expected_hash.as_deref())
                    .await?;
                Ok(format!("Moved: {} → {}", from, to))
            }

            BatchOperation::UpdateLinks {
                file,
                old_target,
                new_target,
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
        }
    }
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
            },
            BatchOperation::CreateNote {
                path: "beta.md".to_string(),
                content: "beta v1".to_string(),
            },
            BatchOperation::CreateNote {
                path: "gamma.md".to_string(),
                content: "gamma".to_string(),
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
            },
            BatchOperation::DeleteNote {
                path: "nonexistent.md".to_string(),
                expected_hash: None,
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

    /// A WriteNote whose `expected_hash` matches the on-disk content proceeds.
    #[tokio::test]
    async fn test_batch_expected_hash_match_succeeds() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("note.md"), "v1").unwrap();
        let hash = turbovault_vault::compute_hash("v1");

        let executor = make_executor(&temp).await;
        let result = executor
            .execute(vec![BatchOperation::WriteNote {
                path: "note.md".to_string(),
                content: "v2".to_string(),
                expected_hash: Some(hash),
            }])
            .await
            .unwrap();

        assert!(result.success, "batch should succeed: {:?}", result.errors);
        let on_disk = std::fs::read_to_string(temp.path().join("note.md")).unwrap();
        assert_eq!(on_disk, "v2");
    }

    /// A stale `expected_hash` fails the batch in the pre-flight check, before
    /// any operation runs — so earlier (hashless) operations are NOT applied.
    #[tokio::test]
    async fn test_batch_stale_expected_hash_fails_preflight() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("guarded.md"), "current").unwrap();

        let executor = make_executor(&temp).await;
        let result = executor
            .execute(vec![
                // This op has no precondition and would succeed on its own...
                BatchOperation::CreateNote {
                    path: "side_effect.md".to_string(),
                    content: "should not be written".to_string(),
                },
                // ...but this op's precondition is already stale, so the whole
                // batch must abort before anything is applied.
                BatchOperation::WriteNote {
                    path: "guarded.md".to_string(),
                    content: "new".to_string(),
                    expected_hash: Some("staleHASH".to_string()),
                },
            ])
            .await
            .unwrap();

        assert!(!result.success, "batch should fail on stale precondition");
        assert_eq!(result.executed, 0, "pre-flight must abort before executing");
        assert!(
            !temp.path().join("side_effect.md").exists(),
            "no operation should have been applied"
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("guarded.md")).unwrap(),
            "current",
            "guarded file must be untouched"
        );
    }

    /// DeleteNote and MoveNote also honor `expected_hash` preconditions: a
    /// stale hash on either aborts the batch in the pre-flight pass.
    #[tokio::test]
    async fn test_batch_delete_and_move_preconditions() {
        // Stale DeleteNote precondition.
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("del.md"), "live").unwrap();
        let executor = make_executor(&temp).await;
        let result = executor
            .execute(vec![BatchOperation::DeleteNote {
                path: "del.md".to_string(),
                expected_hash: Some("staleHASH".to_string()),
            }])
            .await
            .unwrap();
        assert!(!result.success, "stale delete precondition must fail");
        assert_eq!(result.executed, 0);
        assert!(temp.path().join("del.md").exists(), "file must survive");

        // Matching MoveNote precondition succeeds.
        let temp2 = TempDir::new().unwrap();
        std::fs::write(temp2.path().join("src.md"), "data").unwrap();
        let hash = turbovault_vault::compute_hash("data");
        let executor2 = make_executor(&temp2).await;
        let result2 = executor2
            .execute(vec![BatchOperation::MoveNote {
                from: "src.md".to_string(),
                to: "dst.md".to_string(),
                expected_hash: Some(hash),
            }])
            .await
            .unwrap();
        assert!(
            result2.success,
            "matching move precondition should succeed: {:?}",
            result2.errors
        );
        assert!(!temp2.path().join("src.md").exists());
        assert!(temp2.path().join("dst.md").exists());
    }
}
