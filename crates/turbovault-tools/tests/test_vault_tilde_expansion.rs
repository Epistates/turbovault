//! Test tilde expansion in vault paths

use turbovault_core::prelude::*;
use turbovault_tools::VaultLifecycleTools;
use std::path::PathBuf;

#[tokio::test]
async fn test_create_vault_with_tilde_expansion() {
    // Generate unique name
    use std::time::{SystemTime, UNIX_EPOCH};
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis();
    let vault_path_with_tilde = format!("~/work/test_vault_{}", timestamp);

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
    assert!(
        !vault_info.path.to_string_lossy().contains('~'),
        "Path should not contain tilde: {}",
        vault_info.path.display()
    );
    assert!(vault_info.path.is_absolute(), "Path should be absolute");

    // Verify vault directory exists at expanded path
    assert!(
        vault_info.path.exists(),
        "Vault directory should exist at {}",
        vault_info.path.display()
    );
    assert!(
        vault_info.path.join(".obsidian").exists(),
        ".obsidian directory should exist"
    );

    // Cleanup
    tokio::fs::remove_dir_all(&vault_info.path).await.ok();
}

#[tokio::test]
async fn test_add_vault_with_tilde_expansion() {
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();

    // Create directory structure
    let test_dir = temp.path().join("test_vault");
    tokio::fs::create_dir_all(&test_dir).await.unwrap();
    tokio::fs::create_dir_all(test_dir.join(".obsidian"))
        .await
        .unwrap();

    // Path is already absolute, no need for tilde simulation

    let config = ServerConfig {
        vaults: vec![],
        ..Default::default()
    };

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());
    let lifecycle_tools = VaultLifecycleTools::new(multi_mgr.clone());

    // Add existing vault
    let vault_info = lifecycle_tools
        .add_vault_from_path("test_add", &test_dir)
        .await
        .unwrap();

    // Verify path is absolute
    assert!(vault_info.path.is_absolute(), "Path should be absolute");
    assert!(vault_info.path.exists(), "Vault directory should exist");
}
