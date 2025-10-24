//! Multi-vault management system for enterprise deployments
//!
//! Enables managing multiple Obsidian vaults simultaneously with:
//! - Vault isolation and independent lifecycle
//! - Default vault concept
//! - Setting inheritance and per-vault overrides
//! - Centralized configuration

use crate::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// Information about a registered vault
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VaultInfo {
    /// Unique vault name
    pub name: String,
    /// Vault directory path
    pub path: std::path::PathBuf,
    /// Whether this is the active/default vault
    pub is_default: bool,
    /// Configuration for this vault
    pub config: VaultConfig,
}

/// Multi-vault manager coordinating multiple vaults
pub struct MultiVaultManager {
    /// All registered vaults
    vaults: Arc<RwLock<HashMap<String, VaultConfig>>>,
    /// Currently active/default vault
    default_vault: Arc<RwLock<String>>,
    /// Server-level configuration
    config: ServerConfig,
}

impl MultiVaultManager {
    /// Create a new multi-vault manager from server configuration
    pub fn new(config: ServerConfig) -> Result<Self> {
        // Allow zero vaults - vaults can be added at runtime via add_vault tool
        let vaults = Arc::new(RwLock::new(
            config
                .vaults
                .iter()
                .map(|v| (v.name.clone(), v.clone()))
                .collect(),
        ));

        // Find default vault if any configured, otherwise None (will be set via set_active_vault)
        let default_name = if config.vaults.is_empty() {
            String::new() // Empty string indicates no default set
        } else {
            config
                .vaults
                .iter()
                .find(|v| v.is_default)
                .map(|v| v.name.clone())
                .or_else(|| config.vaults.first().map(|v| v.name.clone()))
                .unwrap_or_default()
        };

        Ok(Self {
            vaults,
            default_vault: Arc::new(RwLock::new(default_name)),
            config,
        })
    }

    /// Create an empty multi-vault manager (vault-agnostic server startup)
    pub fn empty(config: ServerConfig) -> Result<Self> {
        // Create with no vaults pre-configured
        Ok(Self {
            vaults: Arc::new(RwLock::new(HashMap::new())),
            default_vault: Arc::new(RwLock::new(String::new())),
            config,
        })
    }

    /// Add a new vault to the manager
    pub async fn add_vault(&self, vault_config: VaultConfig) -> Result<()> {
        let mut vaults = self.vaults.write().await;

        if vaults.contains_key(&vault_config.name) {
            return Err(Error::invalid_path(format!(
                "Vault '{}' already exists",
                vault_config.name
            )));
        }

        let is_first_vault = vaults.is_empty();
        vaults.insert(vault_config.name.clone(), vault_config.clone());

        // If this is the first vault, automatically set it as default
        if is_first_vault {
            drop(vaults); // Release write lock before acquiring default_vault lock
            *self.default_vault.write().await = vault_config.name;
        }

        Ok(())
    }

    /// Remove a vault from the manager
    pub async fn remove_vault(&self, name: &str) -> Result<()> {
        let mut vaults = self.vaults.write().await;

        if !vaults.contains_key(name) {
            return Err(Error::not_found(format!("Vault '{}' not found", name)));
        }

        let current_default = self.default_vault.read().await;

        // If removing the default vault, we need to handle it
        if *current_default == name {
            drop(current_default); // Release read lock
            vaults.remove(name);

            // If there are other vaults, set the first one as default; otherwise, clear it
            if let Some((first_name, _)) = vaults.iter().next() {
                *self.default_vault.write().await = first_name.clone();
            } else {
                *self.default_vault.write().await = String::new();
            }
        } else {
            vaults.remove(name);
        }

        Ok(())
    }

    /// Get configuration for a specific vault
    pub async fn get_vault_config(&self, name: &str) -> Result<VaultConfig> {
        let vaults = self.vaults.read().await;
        vaults
            .get(name)
            .cloned()
            .ok_or_else(|| Error::not_found(format!("Vault '{}' not found", name)))
    }

    /// Get the active/default vault name
    pub async fn get_active_vault(&self) -> String {
        self.default_vault.read().await.clone()
    }

    /// Set a different vault as the active vault
    pub async fn set_active_vault(&self, name: &str) -> Result<()> {
        let vaults = self.vaults.read().await;

        if !vaults.contains_key(name) {
            return Err(Error::not_found(format!("Vault '{}' not found", name)));
        }

        *self.default_vault.write().await = name.to_string();
        Ok(())
    }

    /// List all registered vaults
    pub async fn list_vaults(&self) -> Result<Vec<VaultInfo>> {
        let vaults = self.vaults.read().await;
        let default = self.default_vault.read().await.clone();

        let infos = vaults
            .iter()
            .map(|(name, config)| VaultInfo {
                name: name.clone(),
                path: config.path.clone(),
                is_default: name == &default,
                config: config.clone(),
            })
            .collect();

        Ok(infos)
    }

    /// Get effective settings for a vault (inherited + overridden)
    pub async fn get_effective_vault_settings(&self, vault_name: &str) -> Result<VaultConfig> {
        let vault_config = self.get_vault_config(vault_name).await?;

        // Start with server defaults
        let effective = vault_config.clone();

        // Apply vault-specific overrides
        // (VaultConfig already contains the overrides, so we just return it)

        Ok(effective)
    }

    /// Get vault count
    pub async fn vault_count(&self) -> usize {
        self.vaults.read().await.len()
    }

    /// Check if a vault exists
    pub async fn vault_exists(&self, name: &str) -> bool {
        self.vaults.read().await.contains_key(name)
    }

    /// Get the active vault config
    pub async fn get_active_vault_config(&self) -> Result<VaultConfig> {
        let active_name = self.default_vault.read().await.clone();
        if active_name.is_empty() {
            return Err(Error::not_found(
                "No vault is currently active. Please add a vault using add_vault tool."
                    .to_string(),
            ));
        }
        self.get_vault_config(&active_name).await
    }
}

impl Clone for MultiVaultManager {
    fn clone(&self) -> Self {
        Self {
            vaults: self.vaults.clone(),
            default_vault: self.default_vault.clone(),
            config: self.config.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_vault(name: &str, is_default: bool) -> VaultConfig {
        VaultConfig {
            name: name.to_string(),
            path: std::path::PathBuf::from(format!("/tmp/{}", name)),
            is_default,
            watch_for_changes: None,
            max_file_size: None,
            allowed_extensions: None,
            excluded_paths: None,
            enable_caching: None,
            cache_ttl: None,
            template_dirs: None,
            allowed_operations: None,
        }
    }

    fn create_test_config() -> ServerConfig {
        let mut config = ServerConfig::new();
        config.vaults = vec![
            create_test_vault("vault1", true),
            create_test_vault("vault2", false),
        ];
        config
    }

    #[test]
    fn test_multi_vault_manager_creation() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config);
        assert!(manager.is_ok());
    }

    #[test]
    fn test_can_create_empty_vaults() {
        // Vault-agnostic design: allow empty vaults for runtime addition
        let config = ServerConfig::new(); // Empty vaults
        let manager = MultiVaultManager::new(config);
        assert!(manager.is_ok());
        let mgr = manager.unwrap();
        // Should start with no default vault set
        let rt = tokio::runtime::Runtime::new().unwrap();
        let default = rt.block_on(async { mgr.get_active_vault().await });
        assert!(default.is_empty());
    }

    #[tokio::test]
    async fn test_get_active_vault() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();
        let active = manager.get_active_vault().await;
        assert_eq!(active, "vault1");
    }

    #[tokio::test]
    async fn test_set_active_vault() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();

        manager.set_active_vault("vault2").await.unwrap();
        let active = manager.get_active_vault().await;
        assert_eq!(active, "vault2");
    }

    #[tokio::test]
    async fn test_set_invalid_active_vault() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();

        let result = manager.set_active_vault("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_add_vault() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();

        let new_vault = create_test_vault("vault3", false);
        manager.add_vault(new_vault).await.unwrap();

        assert!(manager.vault_exists("vault3").await);
    }

    #[tokio::test]
    async fn test_add_duplicate_vault_fails() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();

        let dup_vault = create_test_vault("vault1", false);
        let result = manager.add_vault(dup_vault).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_remove_vault() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();

        manager.remove_vault("vault2").await.unwrap();
        assert!(!manager.vault_exists("vault2").await);
    }

    #[tokio::test]
    async fn test_remove_default_vault_reassigns() {
        // Removing the default vault should succeed and reassign to another vault
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();

        // vault1 is the default, vault2 is the backup
        assert_eq!(manager.get_active_vault().await, "vault1");

        let result = manager.remove_vault("vault1").await;
        assert!(result.is_ok());
        assert!(!manager.vault_exists("vault1").await);

        // Should now default to vault2
        let new_default = manager.get_active_vault().await;
        assert_eq!(new_default, "vault2");
    }

    #[tokio::test]
    async fn test_list_vaults() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();

        let vaults = manager.list_vaults().await.unwrap();
        assert_eq!(vaults.len(), 2);
        assert!(vaults.iter().any(|v| v.name == "vault1" && v.is_default));
        assert!(vaults.iter().any(|v| v.name == "vault2" && !v.is_default));
    }

    #[tokio::test]
    async fn test_vault_count() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();

        assert_eq!(manager.vault_count().await, 2);

        manager
            .add_vault(create_test_vault("vault3", false))
            .await
            .ok();
        assert_eq!(manager.vault_count().await, 3);
    }

    #[tokio::test]
    async fn test_get_effective_vault_settings() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();

        let settings = manager
            .get_effective_vault_settings("vault1")
            .await
            .unwrap();
        assert_eq!(settings.name, "vault1");
    }

    #[tokio::test]
    async fn test_clone() {
        let config = create_test_config();
        let manager = MultiVaultManager::new(config).unwrap();
        let manager2 = manager.clone();

        assert_eq!(manager.vault_count().await, manager2.vault_count().await);
    }
}
