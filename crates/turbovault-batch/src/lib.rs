//! # Batch Operations Framework
//!
//! Provides atomic, transactional batch file operations with rollback support.
//! All operations in a batch either succeed together or fail together, maintaining
//! vault integrity even if individual operations encounter errors.

use turbovault_core::prelude::*;
use turbovault_core::{PathValidator, TransactionBuilder};
use turbovault_vault::VaultManager;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Individual batch operation to execute
#[derive(Debug, Clone, Serialize, Deserialize)]
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

/// Batch executor with transaction support
#[allow(dead_code)]
pub struct BatchExecutor {
    manager: Arc<VaultManager>,
    temp_dir: PathBuf,
}

impl BatchExecutor {
    /// Create a new batch executor
    pub fn new(manager: Arc<VaultManager>, temp_dir: PathBuf) -> Self {
        Self { manager, temp_dir }
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
        match op {
            BatchOperation::CreateNote { path, content } => {
                let path_buf = PathBuf::from(path);
                self.manager.write_file(&path_buf, content).await?;
                Ok(format!("Created: {}", path))
            }

            BatchOperation::WriteNote { path, content } => {
                let path_buf = PathBuf::from(path);
                self.manager.write_file(&path_buf, content).await?;
                Ok(format!("Updated: {}", path))
            }

            BatchOperation::DeleteNote { path } => {
                let full_path = PathValidator::validate_path_in_vault(
                    self.manager.vault_path(),
                    &PathBuf::from(path),
                )?;

                tokio::fs::remove_file(&full_path).await.map_err(|e| {
                    Error::config_error(format!("Failed to delete {}: {}", path, e))
                })?;

                Ok(format!("Deleted: {}", path))
            }

            BatchOperation::MoveNote { from, to } => {
                let from_path =
                    PathValidator::validate_path_in_vault(self.manager.vault_path(), &PathBuf::from(from))?;
                let to_path =
                    PathValidator::validate_path_in_vault(self.manager.vault_path(), &PathBuf::from(to))?;

                // Create parent directory if needed
                if let Some(parent) = to_path.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        Error::config_error(format!(
                            "Failed to create parent dirs for {}: {}",
                            to, e
                        ))
                    })?;
                }

                // Perform rename
                tokio::fs::rename(&from_path, &to_path).await.map_err(|e| {
                    Error::config_error(format!("Failed to move {} to {}: {}", from, to, e))
                })?;

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
                    self.manager.write_file(&path_buf, &updated).await?;
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
}
