//! Vault lifecycle management tools
//!
//! Manages vault creation, registration, and switching via MultiVaultManager.
//! Provides MCP tools for users to create new vaults or register existing ones.

use turbovault_core::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Vault lifecycle operations
pub struct VaultLifecycleTools {
    /// Multi-vault manager for registration/switching
    multi_manager: Arc<MultiVaultManager>,
}

impl VaultLifecycleTools {
    /// Create new lifecycle tools
    pub fn new(multi_manager: Arc<MultiVaultManager>) -> Self {
        Self { multi_manager }
    }

    /// Create a new vault at the specified path
    ///
    /// # Arguments
    /// - `name`: Unique vault identifier (no spaces)
    /// - `path`: Directory to create vault in (supports tilde expansion)
    /// - `template`: Optional template name ("default", "research", "team")
    ///
    /// # Returns
    /// VaultInfo with the created vault details (includes fully resolved path)
    ///
    /// # Errors
    /// - Invalid name (empty, spaces)
    /// - Path I/O errors
    /// - Vault already registered
    pub async fn create_vault(
        &self,
        name: &str,
        path: &Path,
        template: Option<&str>,
    ) -> Result<VaultInfo> {
        // Validation: name format
        if name.is_empty() {
            return Err(Error::config_error(
                "Vault name cannot be empty".to_string(),
            ));
        }

        if name.contains(' ') {
            return Err(Error::config_error(
                "Vault name cannot contain spaces".to_string(),
            ));
        }

        if name.len() > 64 {
            return Err(Error::config_error(
                "Vault name too long (max 64 chars)".to_string(),
            ));
        }

        // Check if already registered
        if self.multi_manager.vault_exists(name).await {
            return Err(Error::config_error(format!(
                "Vault '{}' already registered",
                name
            )));
        }

        // Expand tilde and convert to absolute path
        let expanded_path = Self::expand_path(path)?;

        // If path doesn't exist, create it
        if !expanded_path.exists() {
            tokio::fs::create_dir_all(&expanded_path)
                .await
                .map_err(|e| {
                    Error::config_error(format!("Failed to create vault directory: {}", e))
                })?;
        }

        if !expanded_path.is_dir() {
            return Err(Error::invalid_path(format!(
                "Path is not a directory: {}",
                expanded_path.display()
            )));
        }

        // Create .obsidian directory (marks directory as Obsidian vault)
        let obsidian_dir = expanded_path.join(".obsidian");
        tokio::fs::create_dir_all(&obsidian_dir)
            .await
            .map_err(|e| {
                Error::config_error(format!("Failed to create .obsidian directory: {}", e))
            })?;

        // Initialize template structure if specified
        if let Some(tmpl) = template {
            self.initialize_template_structure(&expanded_path, tmpl)
                .await?;
        } else {
            // Create default structure
            self.initialize_default_structure(&expanded_path).await?;
        }

        // Create vault configuration (uses expanded path)
        let config = VaultConfig::builder(name, &expanded_path).build()?;

        // Register with multi-vault manager
        self.multi_manager.add_vault(config).await?;

        // Return vault info
        self.get_vault_info(name).await
    }

    /// Add an existing vault (path must exist and be a directory)
    ///
    /// # Arguments
    /// - `name`: Unique vault identifier
    /// - `path`: Existing vault directory path (supports tilde expansion)
    ///
    /// # Returns
    /// VaultInfo with the registered vault details
    pub async fn add_vault_from_path(&self, name: &str, path: &Path) -> Result<VaultInfo> {
        // Validation: name format
        if name.is_empty() || name.contains(' ') {
            return Err(Error::config_error(
                "Invalid vault name (cannot be empty or contain spaces)".to_string(),
            ));
        }

        // Check if already registered
        if self.multi_manager.vault_exists(name).await {
            return Err(Error::config_error(format!(
                "Vault '{}' already registered",
                name
            )));
        }

        // Expand tilde and convert to absolute path
        let expanded_path = Self::expand_path(path)?;

        // Verify path exists and is directory
        if !expanded_path.exists() || !expanded_path.is_dir() {
            return Err(Error::invalid_path(format!(
                "Path does not exist or is not a directory: {}",
                expanded_path.display()
            )));
        }

        // Create vault config
        let config = VaultConfig::builder(name, &expanded_path).build()?;

        // Register with multi-vault manager
        self.multi_manager.add_vault(config).await?;

        // Return vault info
        self.get_vault_info(name).await
    }

    /// List all registered vaults
    pub async fn list_vaults(&self) -> Result<Vec<VaultInfo>> {
        self.multi_manager.list_vaults().await
    }

    /// Get configuration for a specific vault
    pub async fn get_vault_config(&self, name: &str) -> Result<VaultConfig> {
        self.multi_manager.get_vault_config(name).await
    }

    /// Get the currently active vault
    pub async fn get_active_vault(&self) -> Result<String> {
        Ok(self.multi_manager.get_active_vault().await)
    }

    /// Set the active vault (all subsequent operations use this vault)
    pub async fn set_active_vault(&self, name: &str) -> Result<()> {
        self.multi_manager.set_active_vault(name).await
    }

    /// Remove a vault from registration (cannot remove active vault)
    pub async fn remove_vault(&self, name: &str) -> Result<()> {
        self.multi_manager.remove_vault(name).await
    }

    /// Validate that a vault directory is properly formatted
    ///
    /// Checks for:
    /// - .obsidian directory exists
    /// - Has readable permissions
    /// - Contains expected structure
    pub async fn validate_vault(&self, name: &str) -> Result<serde_json::Value> {
        let vault_info = self
            .multi_manager
            .list_vaults()
            .await?
            .into_iter()
            .find(|v| v.name == name)
            .ok_or_else(|| Error::not_found(format!("Vault '{}' not found", name)))?;

        let mut issues = Vec::new();
        let mut is_valid = true;

        // Check .obsidian directory
        let obsidian_dir = vault_info.path.join(".obsidian");
        if !obsidian_dir.exists() {
            issues.push("Missing .obsidian directory".to_string());
            is_valid = false;
        }

        // Check readable
        match tokio::fs::metadata(&vault_info.path).await {
            Ok(meta) => {
                if !meta.is_dir() {
                    issues.push("Vault path is not a directory".to_string());
                    is_valid = false;
                }
            }
            Err(e) => {
                issues.push(format!("Cannot access vault: {}", e));
                is_valid = false;
            }
        }

        Ok(serde_json::json!({
            "vault": name,
            "path": vault_info.path.display().to_string(),
            "is_valid": is_valid,
            "issues": issues,
        }))
    }

    // Private helpers

    /// Expand tilde and environment variables in path, then canonicalize to absolute path
    ///
    /// Uses shellexpand for tilde/env expansion (best-in-class library)
    fn expand_path(path: &Path) -> Result<PathBuf> {
        let path_str = path
            .to_str()
            .ok_or_else(|| Error::invalid_path("Path contains invalid UTF-8".to_string()))?;

        // Expand tilde and environment variables
        let expanded = shellexpand::full(path_str)
            .map_err(|e| Error::invalid_path(format!("Failed to expand path: {}", e)))?;

        let expanded_path = PathBuf::from(expanded.as_ref());

        // Convert to absolute path (canonicalize if exists, otherwise resolve relative to cwd)
        if expanded_path.exists() {
            // Canonicalize to absolute path
            expanded_path
                .canonicalize()
                .map_err(|e| Error::invalid_path(format!("Failed to resolve path: {}", e)))
        } else {
            // Path doesn't exist yet - make it absolute relative to current directory
            if expanded_path.is_absolute() {
                Ok(expanded_path)
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(&expanded_path))
                    .map_err(|e| {
                        Error::invalid_path(format!("Failed to get current directory: {}", e))
                    })
            }
        }
    }

    async fn get_vault_info(&self, name: &str) -> Result<VaultInfo> {
        let vaults = self.multi_manager.list_vaults().await?;
        vaults
            .into_iter()
            .find(|v| v.name == name)
            .ok_or_else(|| Error::not_found(format!("Vault '{}' not found", name)))
    }

    async fn initialize_default_structure(&self, path: &Path) -> Result<()> {
        // Create standard Obsidian directory structure
        let dirs = ["Areas", "Projects", "Resources", "Archive"];
        for dir in &dirs {
            tokio::fs::create_dir_all(path.join(dir))
                .await
                .map_err(|e| {
                    Error::config_error(format!("Failed to create {} directory: {}", dir, e))
                })?;
        }

        // Create README.md
        let vault_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("Vault");
        let readme_content = format!(
            "# {}\n\nWelcome to your Obsidian vault!\n\n## Structure\n\n- **Areas**: Spaces of activities and responsibilities\n- **Projects**: Short-term efforts\n- **Resources**: Reference material\n- **Archive**: Completed or inactive items\n",
            vault_name
        );

        tokio::fs::write(path.join("README.md"), readme_content)
            .await
            .map_err(|e| Error::config_error(format!("Failed to create README.md: {}", e)))?;

        Ok(())
    }

    async fn initialize_template_structure(&self, path: &Path, template: &str) -> Result<()> {
        match template {
            "default" => self.initialize_default_structure(path).await,
            "research" => {
                // Research-specific structure
                let dirs = ["Literature", "Theory", "Findings", "Hypotheses"];
                for dir in &dirs {
                    tokio::fs::create_dir_all(path.join(dir))
                        .await
                        .map_err(|e| {
                            Error::config_error(format!(
                                "Failed to create {} directory: {}",
                                dir, e
                            ))
                        })?;
                }
                Ok(())
            }
            "team" => {
                // Team collaboration structure
                let dirs = ["Team", "Projects", "Decisions", "Documentation"];
                for dir in &dirs {
                    tokio::fs::create_dir_all(path.join(dir))
                        .await
                        .map_err(|e| {
                            Error::config_error(format!(
                                "Failed to create {} directory: {}",
                                dir, e
                            ))
                        })?;
                }
                Ok(())
            }
            _ => Err(Error::config_error(format!(
                "Unknown template: {} (supported: default, research, team)",
                template
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_placeholder() {
        // Tests are in integration tests file
        // This module is kept for future unit tests
    }
}
