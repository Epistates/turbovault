//! Test tilde expansion in vault paths

use turbovault_core::config::WriteBackend;
use turbovault_core::prelude::*;
use turbovault_tools::VaultLifecycleTools;

#[tokio::test]
async fn test_add_vault_with_absolute_temp_path() {
    use tempfile::TempDir;

    let temp = TempDir::new().unwrap();

    // Create directory structure
    let test_dir = temp.path().join("test_vault");
    tokio::fs::create_dir_all(&test_dir).await.unwrap();
    tokio::fs::create_dir_all(test_dir.join(".obsidian"))
        .await
        .unwrap();

    let config = ServerConfig {
        vaults: vec![],
        ..Default::default()
    };

    let multi_mgr = std::sync::Arc::new(MultiVaultManager::new(config).unwrap());
    let lifecycle_tools = VaultLifecycleTools::new(multi_mgr.clone());

    // Add existing vault
    let vault_info = lifecycle_tools
        .add_vault_from_path("test_add", &test_dir, WriteBackend::Direct, None)
        .await
        .unwrap();

    // Verify path is absolute
    assert!(vault_info.path.is_absolute(), "Path should be absolute");
    assert!(vault_info.path.exists(), "Vault directory should exist");
}
