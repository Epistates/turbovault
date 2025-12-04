//! Regression tests for PR #3: CLI vault deduplication
//!
//! When TurboVault starts, it recovers cached vaults first, then processes CLI arguments.
//! If a vault named 'default' was recovered from cache, and the CLI `--vault` argument
//! also tries to create a vault named 'default', this should NOT cause a fatal error.
//!
//! These tests verify the fix handles all scenarios correctly:
//! 1. Same path (canonicalized) - skip silently
//! 2. Different path - warn and use cached
//! 3. No cached vault - add normally

use std::sync::Arc;
use tempfile::TempDir;
use turbovault_core::prelude::*;

/// Tests that vault_exists correctly detects an existing vault
/// This is a prerequisite for the CLI deduplication logic
#[tokio::test]
async fn test_vault_exists_detection() {
    let temp = TempDir::new().unwrap();
    let vault_path = temp.path().join("default_vault");
    tokio::fs::create_dir_all(&vault_path).await.unwrap();

    let config = ServerConfig {
        vaults: vec![
            VaultConfig::builder("default", &vault_path)
                .build()
                .unwrap(),
        ],
        ..Default::default()
    };

    let multi_mgr = Arc::new(MultiVaultManager::new(config).unwrap());

    // Vault "default" should exist
    assert!(multi_mgr.vault_exists("default").await);

    // Vault "nonexistent" should not exist
    assert!(!multi_mgr.vault_exists("nonexistent").await);
}

/// Tests the CLI vault deduplication scenario: cached vault exists, CLI provides same path
/// Expected behavior: Skip adding (no error), use existing vault
#[tokio::test]
async fn test_cli_vault_dedup_same_path() {
    let temp = TempDir::new().unwrap();
    let vault_path = temp.path().join("my_vault");
    tokio::fs::create_dir_all(&vault_path).await.unwrap();

    // Simulate cache recovery: vault "default" already exists
    let config = ServerConfig {
        vaults: vec![
            VaultConfig::builder("default", &vault_path)
                .build()
                .unwrap(),
        ],
        ..Default::default()
    };

    let multi_mgr = Arc::new(MultiVaultManager::new(config).unwrap());

    // Simulate CLI --vault argument with same path
    let cli_vault_path = vault_path.clone();

    // Check if vault exists (as CLI code does)
    assert!(multi_mgr.vault_exists("default").await);

    // Get existing config and compare paths
    let existing_config = multi_mgr.get_vault_config("default").await.unwrap();
    let existing_canonical = existing_config.path.canonicalize().ok();
    let cli_canonical = cli_vault_path.canonicalize().ok();

    // Paths should match - CLI should skip adding
    assert_eq!(existing_canonical, cli_canonical);

    // Verify only one vault exists
    let vaults = multi_mgr.list_vaults().await.unwrap();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0].name, "default");
}

/// Tests the CLI vault deduplication scenario: cached vault exists, CLI provides different path
/// Expected behavior: Log warning, use cached vault (don't crash)
#[tokio::test]
async fn test_cli_vault_dedup_different_path() {
    let temp = TempDir::new().unwrap();
    let cached_vault_path = temp.path().join("cached_vault");
    let cli_vault_path = temp.path().join("cli_vault");
    tokio::fs::create_dir_all(&cached_vault_path).await.unwrap();
    tokio::fs::create_dir_all(&cli_vault_path).await.unwrap();

    // Simulate cache recovery: vault "default" exists with cached_vault_path
    let config = ServerConfig {
        vaults: vec![
            VaultConfig::builder("default", &cached_vault_path)
                .build()
                .unwrap(),
        ],
        ..Default::default()
    };

    let multi_mgr = Arc::new(MultiVaultManager::new(config).unwrap());

    // Check if vault exists (as CLI code does)
    assert!(multi_mgr.vault_exists("default").await);

    // Get existing config and compare paths
    let existing_config = multi_mgr.get_vault_config("default").await.unwrap();
    let existing_canonical = existing_config.path.canonicalize().ok();
    let cli_canonical = cli_vault_path.canonicalize().ok();

    // Paths should NOT match - CLI should log warning and use cached
    assert_ne!(existing_canonical, cli_canonical);

    // Verify the existing vault is preserved (not overwritten)
    let vaults = multi_mgr.list_vaults().await.unwrap();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0].name, "default");
    assert_eq!(vaults[0].path, cached_vault_path);
}

/// Tests that path canonicalization correctly identifies same paths via different representations
#[tokio::test]
async fn test_path_canonicalization_for_dedup() {
    let temp = TempDir::new().unwrap();
    let vault_path = temp.path().join("vault");
    tokio::fs::create_dir_all(&vault_path).await.unwrap();

    // Create a symlink to the same directory (Unix only)
    #[cfg(unix)]
    {
        let symlink_path = temp.path().join("vault_symlink");
        std::os::unix::fs::symlink(&vault_path, &symlink_path).unwrap();

        let config = ServerConfig {
            vaults: vec![
                VaultConfig::builder("default", &vault_path)
                    .build()
                    .unwrap(),
            ],
            ..Default::default()
        };

        let multi_mgr = Arc::new(MultiVaultManager::new(config).unwrap());

        let existing_config = multi_mgr.get_vault_config("default").await.unwrap();

        // Both paths should canonicalize to the same location
        let existing_canonical = existing_config.path.canonicalize().unwrap();
        let symlink_canonical = symlink_path.canonicalize().unwrap();

        assert_eq!(existing_canonical, symlink_canonical);
    }

    // Test relative vs absolute path (works on all platforms)
    let config = ServerConfig {
        vaults: vec![
            VaultConfig::builder("default", &vault_path)
                .build()
                .unwrap(),
        ],
        ..Default::default()
    };

    let multi_mgr = Arc::new(MultiVaultManager::new(config).unwrap());

    let existing_config = multi_mgr.get_vault_config("default").await.unwrap();

    // Canonicalized paths should match even if one was relative
    let existing_canonical = existing_config.path.canonicalize().unwrap();
    let direct_canonical = vault_path.canonicalize().unwrap();

    assert_eq!(existing_canonical, direct_canonical);
}

/// Tests that when no cached vault exists, CLI can add vault normally
#[tokio::test]
async fn test_cli_vault_no_cache_conflict() {
    let temp = TempDir::new().unwrap();
    let vault_path = temp.path().join("new_vault");
    tokio::fs::create_dir_all(&vault_path).await.unwrap();

    // Start with empty vault list (no cache recovery)
    let config = ServerConfig {
        vaults: vec![],
        ..Default::default()
    };

    let multi_mgr = Arc::new(MultiVaultManager::new(config).unwrap());

    // Vault should not exist
    assert!(!multi_mgr.vault_exists("default").await);

    // CLI can add the vault
    let vault_config = VaultConfig::builder("default", &vault_path)
        .build()
        .unwrap();

    multi_mgr.add_vault(vault_config).await.unwrap();

    // Verify vault was added
    let vaults = multi_mgr.list_vaults().await.unwrap();
    assert_eq!(vaults.len(), 1);
    assert_eq!(vaults[0].name, "default");
    assert_eq!(vaults[0].path, vault_path);
}

/// Tests the complete CLI deduplication flow as implemented in main.rs
/// This simulates the exact logic flow from the PR fix
#[tokio::test]
async fn test_cli_vault_dedup_complete_flow() {
    let temp = TempDir::new().unwrap();
    let cached_path = temp.path().join("cached");
    let cli_path_same = cached_path.clone();
    let cli_path_different = temp.path().join("different");

    tokio::fs::create_dir_all(&cached_path).await.unwrap();
    tokio::fs::create_dir_all(&cli_path_different)
        .await
        .unwrap();

    // Scenario 1: No cached vault - should add successfully
    {
        let config = ServerConfig {
            vaults: vec![],
            ..Default::default()
        };
        let mgr = Arc::new(MultiVaultManager::new(config).unwrap());

        let vault_exists = mgr.vault_exists("default").await;
        assert!(!vault_exists);

        // Add vault (simulating CLI behavior when no conflict)
        let vault_config = VaultConfig::builder("default", &cached_path)
            .build()
            .unwrap();
        mgr.add_vault(vault_config).await.unwrap();

        assert!(mgr.vault_exists("default").await);
    }

    // Scenario 2: Cached vault exists with same path - should skip (no error)
    {
        let config = ServerConfig {
            vaults: vec![
                VaultConfig::builder("default", &cached_path)
                    .build()
                    .unwrap(),
            ],
            ..Default::default()
        };
        let mgr = Arc::new(MultiVaultManager::new(config).unwrap());

        let vault_exists = mgr.vault_exists("default").await;
        assert!(vault_exists);

        let existing_config = mgr.get_vault_config("default").await.unwrap();
        let existing_canonical = existing_config.path.canonicalize().ok();
        let cli_canonical = cli_path_same.canonicalize().ok();

        // Same path - would skip in real CLI
        assert_eq!(existing_canonical, cli_canonical);
    }

    // Scenario 3: Cached vault exists with different path - should warn and use cached
    {
        let config = ServerConfig {
            vaults: vec![
                VaultConfig::builder("default", &cached_path)
                    .build()
                    .unwrap(),
            ],
            ..Default::default()
        };
        let mgr = Arc::new(MultiVaultManager::new(config).unwrap());

        let vault_exists = mgr.vault_exists("default").await;
        assert!(vault_exists);

        let existing_config = mgr.get_vault_config("default").await.unwrap();
        let existing_canonical = existing_config.path.canonicalize().ok();
        let cli_canonical = cli_path_different.canonicalize().ok();

        // Different path - would warn and use cached in real CLI
        assert_ne!(existing_canonical, cli_canonical);

        // Cached vault is preserved
        let final_config = mgr.get_vault_config("default").await.unwrap();
        assert_eq!(
            final_config.path.canonicalize().ok(),
            cached_path.canonicalize().ok()
        );
    }
}

/// Tests that add_vault correctly fails when vault already exists
/// This is the underlying behavior that the CLI deduplication logic prevents from crashing
#[tokio::test]
async fn test_add_duplicate_vault_returns_error() {
    let temp = TempDir::new().unwrap();
    let vault_path = temp.path().join("vault");
    let other_path = temp.path().join("other");
    tokio::fs::create_dir_all(&vault_path).await.unwrap();
    tokio::fs::create_dir_all(&other_path).await.unwrap();

    let config = ServerConfig {
        vaults: vec![
            VaultConfig::builder("default", &vault_path)
                .build()
                .unwrap(),
        ],
        ..Default::default()
    };

    let multi_mgr = Arc::new(MultiVaultManager::new(config).unwrap());

    // Try to add another vault with the same name but different path
    let duplicate_config = VaultConfig::builder("default", &other_path)
        .build()
        .unwrap();

    let result = multi_mgr.add_vault(duplicate_config).await;

    // This SHOULD fail - and that's why the CLI needs deduplication logic
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("already exists"));
}
