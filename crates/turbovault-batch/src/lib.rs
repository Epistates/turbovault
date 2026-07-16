//! # Batch Operations Framework
//!
//! Provides validated, fail-fast batches of vault file operations.
//! Individual mutations use [`VaultManager`] atomic file operations, but a batch
//! is not an all-or-nothing transaction: operations completed before a failure
//! remain applied and are reported in [`BatchResult`].
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
//!     let executor = BatchExecutor::from_manager(Arc::new(manager));
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
//!     // Execute sequentially, stopping at the first failure
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
//! - Sequential, fail-fast execution with proper ordering
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
//! };
//!
//! let delete = BatchOperation::DeleteNote {
//!     path: "file.md".to_string(),
//! };
//!
//! assert!(write.conflicts_with(&delete));
//! ```
//!
//! ## Execution Guarantees
//!
//! The batch executor ensures:
//! - The batch is validated for conflicting operations before execution
//! - Execution stops at the first operation failure
//! - Transaction tracking with unique IDs
//! - Execution timing recorded
//! - Detailed per-operation records for debugging
//! - Each individual file write uses the vault manager's atomic write path
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
    WriteNote { path: String, content: String },

    /// Delete a note
    #[serde(rename = "DeleteNote", alias = "DeleteFile")]
    DeleteNote { path: String },

    /// Move/rename a note
    #[serde(rename = "MoveNote", alias = "MoveFile")]
    MoveNote { from: String, to: String },

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
            Self::DeleteNote { path } => vec![path.clone()],
            Self::MoveNote { from, to } => vec![from.clone(), to.clone()],
            Self::UpdateLinks {
                file,
                old_target,
                new_target,
            } => {
                vec![file.clone(), old_target.clone(), new_target.clone()]
            }
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

/// Batch executor with validation, sequencing, and transaction identifiers.
pub struct BatchExecutor {
    manager: Arc<VaultManager>,
}

impl BatchExecutor {
    /// Create a batch executor.
    ///
    /// `temp_dir` is retained for source compatibility with earlier releases;
    /// the current fail-fast executor does not stage data in a temporary
    /// directory. Prefer [`Self::from_manager`] in new code.
    pub fn new(manager: Arc<VaultManager>, _temp_dir: PathBuf) -> Self {
        Self { manager }
    }

    /// Create a batch executor without the legacy unused temporary-directory
    /// argument.
    pub fn from_manager(manager: Arc<VaultManager>) -> Self {
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

    /// Execute batch operations sequentially and stop at the first failure.
    ///
    /// Operations completed before a failure are not rolled back. Callers must
    /// inspect [`BatchResult::success`], [`BatchResult::failed_at`], and
    /// [`BatchResult::changes`] to determine what was applied.
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
        match op {
            BatchOperation::CreateNote { path, content } => {
                let path_buf = PathBuf::from(path);
                self.manager.write_file(&path_buf, content, None).await?;
                Ok(format!("Created: {}", path))
            }

            BatchOperation::WriteNote { path, content } => {
                let path_buf = PathBuf::from(path);
                self.manager.write_file(&path_buf, content, None).await?;
                Ok(format!("Updated: {}", path))
            }

            BatchOperation::DeleteNote { path } => {
                let path_buf = PathBuf::from(path);
                self.manager.delete_file(&path_buf, None).await?;
                Ok(format!("Deleted: {}", path))
            }

            BatchOperation::MoveNote { from, to } => {
                let from_buf = PathBuf::from(from);
                let to_buf = PathBuf::from(to);
                self.manager.move_file(&from_buf, &to_buf, None).await?;
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
        };
        let op2 = BatchOperation::DeleteNote {
            path: "file.md".to_string(),
        };

        assert!(op1.conflicts_with(&op2));
        assert!(op2.conflicts_with(&op1));
    }

    #[test]
    fn test_no_conflict_different_files() {
        let op1 = BatchOperation::WriteNote {
            path: "file1.md".to_string(),
            content: "content".to_string(),
        };
        let op2 = BatchOperation::WriteNote {
            path: "file2.md".to_string(),
            content: "content".to_string(),
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
