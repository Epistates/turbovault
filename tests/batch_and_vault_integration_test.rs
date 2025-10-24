//! Integration tests for turbovault Tier 1 features

use turbo_vault_batch::{BatchOperation, BatchExecutor};
use turbo_vault_core::prelude::*;
use turbo_vault_vault::VaultManager;
use std::sync::Arc;
use tempfile::TempDir;

// ==================== Batch Operations Tests ====================

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

    let content = manager.read_file(&"doc.md".into()).await.unwrap();
    assert!(content.contains("new-link"));
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
    assert!(!result.success);
}

// ==================== Vault Lifecycle Tests ====================

#[tokio::test]
async fn test_vault_multi_vault_manager() {
    let temp = TempDir::new().unwrap();
    let vault1_path = temp.path().join("vault1");
    let vault2_path = temp.path().join("vault2");

    tokio::fs::create_dir_all(&vault1_path).await.unwrap();
    tokio::fs::create_dir_all(&vault2_path).await.unwrap();

    let config = ServerConfig {
        vaults: vec![
            VaultConfig::builder("vault1", &vault1_path)
                .build()
                .unwrap(),
            VaultConfig::builder("vault2", &vault2_path)
                .build()
                .unwrap(),
        ],
        ..Default::default()
    };

    let multi_mgr = Arc::new(MultiVaultManager::new(config).unwrap());

    // Test list
    let vaults = multi_mgr.list_vaults().await.unwrap();
    assert_eq!(vaults.len(), 2);

    // Test switch
    multi_mgr.set_active_vault("vault2").await.unwrap();
    let active = multi_mgr.get_active_vault().await;
    assert_eq!(active, "vault2");
}

#[tokio::test]
async fn test_vault_cannot_remove_active() {
    let temp = TempDir::new().unwrap();
    let vault1_path = temp.path().join("vault1");

    tokio::fs::create_dir_all(&vault1_path).await.unwrap();

    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("vault1", &vault1_path)
            .build()
            .unwrap()],
        ..Default::default()
    };

    let multi_mgr = Arc::new(MultiVaultManager::new(config).unwrap());

    let result = multi_mgr.remove_vault("vault1").await;
    assert!(result.is_err());
}
