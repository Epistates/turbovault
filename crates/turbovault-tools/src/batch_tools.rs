//! Batch operation tools for coordinated multi-file operations

use std::sync::Arc;
use turbovault_batch::{BatchExecutor, BatchOperation, BatchResult};
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

/// Batch operation tools
pub struct BatchTools {
    pub manager: Arc<VaultManager>,
}

impl BatchTools {
    /// Create new batch tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Execute batch operations atomically
    pub async fn batch_execute(&self, operations: Vec<BatchOperation>) -> Result<BatchResult> {
        // Create temp directory for this batch (kept persistent)
        let temp_dir_handle = tempfile::tempdir().map_err(|e| {
            Error::config_error(format!("Failed to create temp directory for batch: {}", e))
        })?;

        let temp_dir = temp_dir_handle.path().to_path_buf();
        // Keep the temp directory persistent after this function returns
        let _temp_dir_persistent = temp_dir_handle.keep(); // Persist temp dir for batch operations

        let executor = BatchExecutor::new(self.manager.clone(), temp_dir);
        executor.execute(operations).await
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_batch_tools_creation() {
        // Tests in integration tests file
    }
}
