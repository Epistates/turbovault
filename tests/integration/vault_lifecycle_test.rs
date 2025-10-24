//! Integration tests for vault lifecycle management

use turbo_vault_core::prelude::*;
use turbo_vault_tools::VaultLifecycleTools;
use std::path::PathBuf;
use tempfile::TempDir;

#[tokio::test]
async fn test_create_vault() {
    let temp = TempDir::new().unwrap();
    let vault_path = temp.path().join("test_vault");

    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("default", temp.path())
            .build()
            .unwrap()],
        ..Default::default()
    };

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());

    let result = multi_mgr.list_vaults().await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].name, "default");
}

#[tokio::test]
async fn test_add_vault() {
    let temp = TempDir::new().unwrap();
    let vault1_path = temp.path().join("vault1");
    let vault2_path = temp.path().join("vault2");

    tokio::fs::create_dir_all(&vault1_path).await.unwrap();
    tokio::fs::create_dir_all(&vault2_path).await.unwrap();

    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("vault1", &vault1_path)
            .build()
            .unwrap()],
        ..Default::default()
    };

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());

    let vault2_config = VaultConfig::builder("vault2", &vault2_path)
        .build()
        .unwrap();

    multi_mgr.add_vault(vault2_config).await.unwrap();

    let vaults = multi_mgr.list_vaults().await.unwrap();
    assert_eq!(vaults.len(), 2);
    assert!(vaults.iter().any(|v| v.name == "vault1"));
    assert!(vaults.iter().any(|v| v.name == "vault2"));
}

#[tokio::test]
async fn test_set_active_vault() {
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

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());

    // Initially vault1 is active (first in list, and it's marked default)
    let active = multi_mgr.get_active_vault().await;
    assert_eq!(active, "vault1");

    // Switch to vault2
    multi_mgr.set_active_vault("vault2").await.unwrap();
    let active = multi_mgr.get_active_vault().await;
    assert_eq!(active, "vault2");
}

#[tokio::test]
async fn test_cannot_remove_active_vault() {
    let temp = TempDir::new().unwrap();
    let vault1_path = temp.path().join("vault1");

    tokio::fs::create_dir_all(&vault1_path).await.unwrap();

    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("vault1", &vault1_path)
            .build()
            .unwrap()],
        ..Default::default()
    };

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());

    // Try to remove active vault
    let result = multi_mgr.remove_vault("vault1").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_cannot_create_duplicate_vault() {
    let temp = TempDir::new().unwrap();
    let vault1_path = temp.path().join("vault1");

    tokio::fs::create_dir_all(&vault1_path).await.unwrap();

    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("vault1", &vault1_path)
            .build()
            .unwrap()],
        ..Default::default()
    };

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());

    let vault_dup = VaultConfig::builder("vault1", temp.path().join("another"))
        .build()
        .unwrap();

    // Try to add vault with duplicate name
    let result = multi_mgr.add_vault(vault_dup).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_list_vaults() {
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

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());

    let vaults = multi_mgr.list_vaults().await.unwrap();
    assert_eq!(vaults.len(), 2);
    assert_eq!(vaults[0].name, "vault1");
    assert_eq!(vaults[1].name, "vault2");
}

#[tokio::test]
async fn test_get_vault_config() {
    let temp = TempDir::new().unwrap();
    let vault1_path = temp.path().join("vault1");

    tokio::fs::create_dir_all(&vault1_path).await.unwrap();

    let config = ServerConfig {
        vaults: vec![VaultConfig::builder("vault1", &vault1_path)
            .build()
            .unwrap()],
        ..Default::default()
    };

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());

    let vault_config = multi_mgr.get_vault_config("vault1").await.unwrap();
    assert_eq!(vault_config.name, "vault1");
    assert_eq!(vault_config.path, vault1_path);
}

#[tokio::test]
async fn test_remove_vault() {
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

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());

    // Remove non-active vault
    multi_mgr.remove_vault("vault2").await.unwrap();

    let vaults = multi_mgr.list_vaults().await.unwrap();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0].name, "vault1");
}

#[tokio::test]
async fn test_create_vault_with_tilde_expansion() {
    let temp = TempDir::new().unwrap();
    let vault_path_with_tilde = format!("~/work/test_vault_{}", uuid::Uuid::new_v4());

    let config = ServerConfig {
        vaults: vec![],
        ..Default::default()
    };

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());
    let lifecycle_tools = VaultLifecycleTools::new(multi_mgr.clone());

    // Create vault with tilde path
    let vault_info = lifecycle_tools
        .create_vault("test_tilde", &PathBuf::from(&vault_path_with_tilde), None)
        .await
        .unwrap();

    // Verify vault was created with expanded path
    assert_eq!(vault_info.name, "test_tilde");
    assert!(!vault_info.path.to_string_lossy().contains('~'), "Path should not contain tilde");
    assert!(vault_info.path.is_absolute(), "Path should be absolute");

    // Verify vault directory exists at expanded path
    assert!(vault_info.path.exists(), "Vault directory should exist");
    assert!(vault_info.path.join(".obsidian").exists(), ".obsidian directory should exist");

    // Cleanup
    tokio::fs::remove_dir_all(&vault_info.path).await.ok();
}
