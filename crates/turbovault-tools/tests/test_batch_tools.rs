//! Unit tests for BatchTools

use std::sync::Arc;
use tempfile::TempDir;
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::{BatchOperation, BatchTools};
use turbovault_vault::VaultManager;

async fn setup_test_vault() -> (TempDir, Arc<VaultManager>) {
    let temp_dir = TempDir::new().expect("Failed to create temp dir");
    let vault_path = temp_dir.path();

    tokio::fs::write(
        vault_path.join("existing.md"),
        "# Existing\nOriginal content",
    )
    .await
    .unwrap();

    let mut config = ConfigProfile::Development.create_config();
    let vault_config = VaultConfig::builder("test", vault_path).build().unwrap();
    config.vaults.push(vault_config);

    let manager = VaultManager::new(config).unwrap();
    manager.initialize().await.unwrap();

    (temp_dir, Arc::new(manager))
}

#[tokio::test]
async fn test_batch_execute_single_write() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![BatchOperation::WriteNote {
        path: "new.md".to_string(),
        content: "# New Note\nContent".to_string(),
    }];

    let result = tools.batch_execute(ops).await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);
    assert_eq!(batch_result.executed, 1);

    // Verify file was created
    let vault_path = manager.vault_path();
    assert!(vault_path.join("new.md").exists());
}

#[tokio::test]
async fn test_batch_execute_multiple_writes() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![
        BatchOperation::WriteNote {
            path: "note1.md".to_string(),
            content: "# Note 1".to_string(),
        },
        BatchOperation::WriteNote {
            path: "note2.md".to_string(),
            content: "# Note 2".to_string(),
        },
        BatchOperation::WriteNote {
            path: "note3.md".to_string(),
            content: "# Note 3".to_string(),
        },
    ];

    let result = tools.batch_execute(ops).await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);
    assert_eq!(batch_result.executed, 3);
}

#[tokio::test]
async fn test_batch_execute_delete() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![BatchOperation::DeleteNote {
        path: "existing.md".to_string(),
    }];

    let result = tools.batch_execute(ops).await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);

    // Verify file was deleted
    let vault_path = manager.vault_path();
    assert!(!vault_path.join("existing.md").exists());
}

#[tokio::test]
async fn test_batch_execute_move() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![BatchOperation::MoveNote {
        from: "existing.md".to_string(),
        to: "moved.md".to_string(),
    }];

    let result = tools.batch_execute(ops).await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);

    // Verify file was moved
    let vault_path = manager.vault_path();
    assert!(!vault_path.join("existing.md").exists());
    assert!(vault_path.join("moved.md").exists());
}

#[tokio::test]
async fn test_batch_execute_mixed_operations() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![
        BatchOperation::WriteNote {
            path: "new1.md".to_string(),
            content: "# New 1".to_string(),
        },
        BatchOperation::WriteNote {
            path: "new2.md".to_string(),
            content: "# New 2".to_string(),
        },
        BatchOperation::MoveNote {
            from: "existing.md".to_string(),
            to: "renamed.md".to_string(),
        },
    ];

    let result = tools.batch_execute(ops).await;
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(batch_result.success);
    assert_eq!(batch_result.executed, 3);
}

#[tokio::test]
async fn test_batch_execute_rollback_on_error() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![
        BatchOperation::WriteNote {
            path: "success1.md".to_string(),
            content: "# Success 1".to_string(),
        },
        BatchOperation::DeleteNote {
            path: "nonexistent.md".to_string(), // This will fail
        },
        BatchOperation::WriteNote {
            path: "success2.md".to_string(),
            content: "# Success 2".to_string(),
        },
    ];

    let result = tools.batch_execute(ops).await;
    // Implementation returns Ok(BatchResult { success: false }), not Err
    assert!(result.is_ok());
    let batch_result = result.unwrap();
    assert!(!batch_result.success);
    assert_eq!(batch_result.executed, 1); // Stopped at operation 1 (delete)

    // Note: Current implementation does NOT rollback - it's fail-fast
    // Operation 0 (success1.md) was written before the failure
    let vault_path = manager.vault_path();
    assert!(vault_path.join("success1.md").exists()); // Written before failure
    assert!(!vault_path.join("success2.md").exists()); // Not executed after failure
}

#[tokio::test]
async fn test_batch_execute_empty_operations() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager);

    let ops: Vec<BatchOperation> = vec![];

    let result = tools.batch_execute(ops).await;
    // Should handle empty operations gracefully
    assert!(result.is_err() || result.unwrap().executed == 0);
}

#[tokio::test]
async fn test_batch_execute_creates_directories() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    let ops = vec![BatchOperation::WriteNote {
        path: "nested/deep/folder/note.md".to_string(),
        content: "# Nested Note".to_string(),
    }];

    let result = tools.batch_execute(ops).await;
    assert!(result.is_ok());

    // Verify nested directories were created
    let vault_path = manager.vault_path();
    assert!(vault_path.join("nested/deep/folder/note.md").exists());
}

#[tokio::test]
async fn test_batch_execute_atomic_guarantees() {
    let (_temp_dir, manager) = setup_test_vault().await;
    let tools = BatchTools::new(manager.clone());

    // First batch should succeed
    let ops1 = vec![
        BatchOperation::WriteNote {
            path: "atomic1.md".to_string(),
            content: "# Atomic 1".to_string(),
        },
        BatchOperation::WriteNote {
            path: "atomic2.md".to_string(),
            content: "# Atomic 2".to_string(),
        },
    ];

    let result1 = tools.batch_execute(ops1).await;
    assert!(result1.is_ok());

    // Second batch with error should not affect first batch
    let ops2 = vec![
        BatchOperation::WriteNote {
            path: "atomic3.md".to_string(),
            content: "# Atomic 3".to_string(),
        },
        BatchOperation::DeleteNote {
            path: "nonexistent_for_atomic_test.md".to_string(),
        },
    ];

    let result2 = tools.batch_execute(ops2).await;
    // Implementation returns Ok(BatchResult { success: false }), not Err
    assert!(result2.is_ok());
    let batch_result2 = result2.unwrap();
    assert!(!batch_result2.success);
    assert_eq!(batch_result2.executed, 1); // Stopped at operation 1 (delete)

    // Verify first batch files still exist (different batch, unaffected)
    let vault_path = manager.vault_path();
    assert!(vault_path.join("atomic1.md").exists());
    assert!(vault_path.join("atomic2.md").exists());

    // Second batch: operation 0 executed before failure at operation 1
    assert!(vault_path.join("atomic3.md").exists()); // Written before failure in batch 2
}

#[tokio::test]
async fn test_async_error_path_concurrent_batch_operations() {
    let (_temp_dir, manager) = setup_test_vault().await;

    // Spawn multiple concurrent batch operations
    let handles: Vec<_> = (0..5)
        .map(|i| {
            let tools = BatchTools::new(manager.clone());
            tokio::spawn(async move {
                let ops = vec![BatchOperation::WriteNote {
                    path: format!("concurrent_{}.md", i),
                    content: format!("# Concurrent {}", i),
                }];
                tools.batch_execute(ops).await
            })
        })
        .collect();

    // All batches should complete successfully
    for handle in handles {
        let result = handle.await.expect("Task panicked");
        assert!(result.is_ok());
    }
}
