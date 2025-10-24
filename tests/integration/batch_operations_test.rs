//! Integration tests for batch operations

use turbo_vault_batch::{BatchOperation, BatchExecutor};
use turbo_vault_core::prelude::*;
use turbo_vault_vault::VaultManager;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_batch_create_files() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let temp_dir = TempDir::new().unwrap();
    let executor = BatchExecutor::new(manager.clone(), temp_dir.path().to_path_buf());

    let ops = vec![
        BatchOperation::CreateFile {
            path: "file1.md".to_string(),
            content: "content1".to_string(),
        },
        BatchOperation::CreateFile {
            path: "file2.md".to_string(),
            content: "content2".to_string(),
        },
    ];

    let result = executor.execute(ops).await.unwrap();
    assert!(result.success);
    assert_eq!(result.executed, 2);
    assert_eq!(result.changes.len(), 2);
}

#[tokio::test]
async fn test_batch_write_file() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    // Create initial file
    manager
        .write_file(&"test.md".into(), "original")
        .await
        .unwrap();

    let temp_dir = TempDir::new().unwrap();
    let executor = BatchExecutor::new(manager.clone(), temp_dir.path().to_path_buf());

    let ops = vec![BatchOperation::WriteFile {
        path: "test.md".to_string(),
        content: "updated".to_string(),
    }];

    let result = executor.execute(ops).await.unwrap();
    assert!(result.success);
    assert_eq!(result.executed, 1);

    // Verify file was updated
    let content = manager.read_file(&"test.md".into()).await.unwrap();
    assert_eq!(content, "updated");
}

#[tokio::test]
async fn test_batch_conflict_detection() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let temp_dir = TempDir::new().unwrap();
    let executor = BatchExecutor::new(manager.clone(), temp_dir.path().to_path_buf());

    // Two operations on same file should conflict
    let ops = vec![
        BatchOperation::WriteFile {
            path: "file.md".to_string(),
            content: "content1".to_string(),
        },
        BatchOperation::DeleteFile {
            path: "file.md".to_string(),
        },
    ];

    let result = executor.execute(ops).await.unwrap();
    assert!(!result.success); // Should fail due to conflict
    assert_eq!(result.failed_at, None); // Fails at validation, not execution
}

#[tokio::test]
async fn test_batch_independent_operations() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let temp_dir = TempDir::new().unwrap();
    let executor = BatchExecutor::new(manager.clone(), temp_dir.path().to_path_buf());

    // Three independent operations should succeed
    let ops = vec![
        BatchOperation::CreateFile {
            path: "file1.md".to_string(),
            content: "content1".to_string(),
        },
        BatchOperation::CreateFile {
            path: "file2.md".to_string(),
            content: "content2".to_string(),
        },
        BatchOperation::CreateFile {
            path: "file3.md".to_string(),
            content: "content3".to_string(),
        },
    ];

    let result = executor.execute(ops).await.unwrap();
    assert!(result.success);
    assert_eq!(result.executed, 3);
    assert_eq!(result.changes.len(), 3);
}

#[tokio::test]
async fn test_batch_update_links() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    // Create file with link
    manager
        .write_file(&"doc.md".into(), "See [[old-link]] for details")
        .await
        .unwrap();

    let temp_dir = TempDir::new().unwrap();
    let executor = BatchExecutor::new(manager.clone(), temp_dir.path().to_path_buf());

    let ops = vec![BatchOperation::UpdateLinks {
        file: "doc.md".to_string(),
        old_target: "old-link".to_string(),
        new_target: "new-link".to_string(),
    }];

    let result = executor.execute(ops).await.unwrap();
    assert!(result.success);

    // Verify link was updated
    let content = manager.read_file(&"doc.md".into()).await.unwrap();
    assert!(content.contains("new-link"));
    assert!(!content.contains("old-link"));
}

#[tokio::test]
async fn test_batch_empty_operations() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let temp_dir = TempDir::new().unwrap();
    let executor = BatchExecutor::new(manager.clone(), temp_dir.path().to_path_buf());

    // Empty batch should fail validation
    let ops = vec![];

    let result = executor.execute(ops).await.unwrap();
    assert!(!result.success);
}

#[tokio::test]
async fn test_batch_transaction_id() {
    let temp = TempDir::new().unwrap();
    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("test", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let manager = Arc::new(VaultManager::new(config).unwrap());
    manager.initialize().await.unwrap();

    let temp_dir = TempDir::new().unwrap();
    let executor = BatchExecutor::new(manager.clone(), temp_dir.path().to_path_buf());

    let ops = vec![BatchOperation::CreateFile {
        path: "test.md".to_string(),
        content: "content".to_string(),
    }];

    let result = executor.execute(ops).await.unwrap();

    // Transaction ID should be a valid UUID
    assert!(!result.transaction_id.is_empty());
    assert!(result.transaction_id.len() > 20); // UUID is typically 36 chars
}
