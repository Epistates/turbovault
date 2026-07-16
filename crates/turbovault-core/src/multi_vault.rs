//! Multi-vault management system for enterprise deployments
//!
//! Enables managing multiple Obsidian vaults simultaneously with:
//! - Vault isolation and independent lifecycle
//! - Default vault concept
//! - Setting inheritance and per-vault overrides
//! - Centralized configuration

use crate::config::WriteBackend;
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
    /// turbovault-17q: the vault's write backend as a lowercase string
    /// (`"legacy"` | `"git"`), surfaced at the top level so agents can discover
    /// which vaults are on the git substrate (GWS) without parsing the nested
    /// `config`. The full `git:` block stays in `config` / `get_vault_config`.
    pub write_backend: String,
    /// turbovault-17q / 5nn: whether mutations on this vault require an explicit
    /// commit message. Only meaningful on the git backend (always `false` on
    /// legacy, which produces no commits). Lets an agent learn the requirement
    /// up front instead of discovering it via a refused write.
    pub require_commit_message: bool,
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

    /// Maximum number of vaults that can be registered simultaneously
    const MAX_VAULTS: usize = 50;

    /// Add a new vault to the manager
    pub async fn add_vault(&self, vault_config: VaultConfig) -> Result<()> {
        let mut vaults = self.vaults.write().await;

        if vaults.len() >= Self::MAX_VAULTS {
            return Err(Error::config_error(format!(
                "Maximum vault limit ({}) reached. Remove a vault before adding a new one.",
                Self::MAX_VAULTS
            )));
        }

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

            // Pick a deterministic replacement rather than depending on HashMap
            // iteration order; otherwise identical removals can activate different vaults.
            if let Some(first_name) = vaults.keys().min() {
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
                write_backend: match config.write_backend {
                    WriteBackend::Legacy => "legacy",
                    WriteBackend::Git => "git",
                }
                .to_string(),
                require_commit_message: matches!(config.write_backend, WriteBackend::Git)
                    && config
                        .git
                        .as_ref()
                        .map(|g| g.require_commit_message)
                        .unwrap_or(false),
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
            write_backend: crate::WriteBackend::default(),
            git: None,
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

    /// turbovault-17q: list_vaults surfaces `write_backend` (lowercase) and
    /// `require_commit_message` at the top level so agents can discover which
    /// vaults are on GWS and whether each requires a commit message.
    #[tokio::test]
    async fn list_vaults_surfaces_backend_and_require_commit_message() {
        let mut git_vault = create_test_vault("vault_git", false);
        git_vault.write_backend = crate::WriteBackend::Git;
        git_vault.git = Some(crate::config::VaultGitConfig {
            require_commit_message: true,
            ..Default::default()
        });
        let mut config = ServerConfig::new();
        config.vaults = vec![create_test_vault("vault1", true), git_vault];
        let manager = MultiVaultManager::new(config).unwrap();

        let vaults = manager.list_vaults().await.unwrap();
        let legacy = vaults.iter().find(|v| v.name == "vault1").unwrap();
        assert_eq!(legacy.write_backend, "legacy");
        assert!(
            !legacy.require_commit_message,
            "legacy backend never requires a commit message"
        );

        let git = vaults.iter().find(|v| v.name == "vault_git").unwrap();
        assert_eq!(git.write_backend, "git");
        assert!(
            git.require_commit_message,
            "git vault with require_commit_message=true surfaces it"
        );
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

    /// Verify that removing the currently-active vault automatically
    /// reassigns the active vault to another registered vault.
    #[tokio::test]
    async fn test_remove_active_vault_reassigns() {
        let config = create_test_config(); // vault1 (default) + vault2
        let manager = MultiVaultManager::new(config).unwrap();

        // Make vault2 active so we can remove vault1 and confirm reassignment
        manager.set_active_vault("vault2").await.unwrap();
        assert_eq!(manager.get_active_vault().await, "vault2");

        // Remove the non-active vault (vault1) — active should stay vault2
        manager.remove_vault("vault1").await.unwrap();
        assert!(!manager.vault_exists("vault1").await);
        assert_eq!(
            manager.get_active_vault().await,
            "vault2",
            "active vault should remain vault2 after vault1 is removed"
        );

        // Now remove the active vault (vault2) — active should be cleared/empty
        // because no other vaults remain
        manager.remove_vault("vault2").await.unwrap();
        assert!(!manager.vault_exists("vault2").await);
        let active_after = manager.get_active_vault().await;
        assert!(
            active_after.is_empty(),
            "active vault should be empty when last vault is removed, got: {:?}",
            active_after
        );
    }

    /// Verify that adding more than MAX_VAULTS (50) vaults returns an error.
    #[tokio::test]
    async fn test_add_vault_at_max_limit() {
        let config = ServerConfig::new(); // start empty
        let manager = MultiVaultManager::new(config).unwrap();

        // Fill up to the limit
        for i in 0..MultiVaultManager::MAX_VAULTS {
            let vault = create_test_vault(&format!("vault{}", i), i == 0);
            manager.add_vault(vault).await.unwrap();
        }

        assert_eq!(manager.vault_count().await, MultiVaultManager::MAX_VAULTS);

        // One more should be rejected
        let overflow = create_test_vault("overflow", false);
        let result = manager.add_vault(overflow).await;
        assert!(
            result.is_err(),
            "adding the {}th vault should fail",
            MultiVaultManager::MAX_VAULTS + 1
        );
    }
}
