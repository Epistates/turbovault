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

    /// Execute batch operations sequentially, stopping at the first failure.
    ///
    /// Each individual file mutation is atomic, but the batch as a whole is
    /// not transactional: operations completed before a failure remain
    /// applied and are reported in [`BatchResult::changes`].
    pub async fn batch_execute(&self, operations: Vec<BatchOperation>) -> Result<BatchResult> {
        let executor = BatchExecutor::from_manager(self.manager.clone());
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
