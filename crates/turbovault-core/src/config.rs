//! Configuration types for the Obsidian server.
//!
//! Follows a builder pattern for complex configuration with validation.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;

/// Configuration for a single vault
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VaultConfig {
    /// Unique identifier for this vault
    pub name: String,
    /// Path to the vault directory
    pub path: PathBuf,
    /// Whether this is the default vault
    pub is_default: bool,

    // Optional overrides
    pub watch_for_changes: Option<bool>,
    pub max_file_size: Option<u64>,
    pub allowed_extensions: Option<HashSet<String>>,
    pub excluded_paths: Option<HashSet<String>>,
    pub enable_caching: Option<bool>,
    pub cache_ttl: Option<u64>,
    pub template_dirs: Option<Vec<PathBuf>>,
    pub allowed_operations: Option<HashSet<String>>,
}

impl VaultConfig {
    /// Create a new vault config with builder
    pub fn builder(name: impl Into<String>, path: impl Into<PathBuf>) -> VaultConfigBuilder {
        VaultConfigBuilder::new(name, path)
    }

    /// Validate the vault configuration
    pub fn validate(&self) -> Result<()> {
        if self.name.is_empty() {
            return Err(Error::config_error("Vault name cannot be empty"));
        }

        if !self.path.exists() {
            return Err(Error::config_error(format!(
                "Vault path does not exist: {}",
                self.path.display()
            )));
        }

        if !self.path.is_dir() {
            return Err(Error::config_error(format!(
                "Vault path is not a directory: {}",
                self.path.display()
            )));
        }

        Ok(())
    }
}

/// Builder for VaultConfig
pub struct VaultConfigBuilder {
    name: String,
    path: PathBuf,
    is_default: bool,
    watch_for_changes: Option<bool>,
    max_file_size: Option<u64>,
    allowed_extensions: Option<HashSet<String>>,
    excluded_paths: Option<HashSet<String>>,
    enable_caching: Option<bool>,
    cache_ttl: Option<u64>,
    template_dirs: Option<Vec<PathBuf>>,
    allowed_operations: Option<HashSet<String>>,
}

impl VaultConfigBuilder {
    /// Create a new builder
    pub fn new(name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
            is_default: false,
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

    /// Mark as default vault
    pub fn as_default(mut self) -> Self {
        self.is_default = true;
        self
    }

    /// Set watch_for_changes
    pub fn watch_for_changes(mut self, watch: bool) -> Self {
        self.watch_for_changes = Some(watch);
        self
    }

    /// Build and validate
    pub fn build(self) -> Result<VaultConfig> {
        let config = VaultConfig {
            name: self.name,
            path: self.path,
            is_default: self.is_default,
            watch_for_changes: self.watch_for_changes,
            max_file_size: self.max_file_size,
            allowed_extensions: self.allowed_extensions,
            excluded_paths: self.excluded_paths,
            enable_caching: self.enable_caching,
            cache_ttl: self.cache_ttl,
            template_dirs: self.template_dirs,
            allowed_operations: self.allowed_operations,
        };
        config.validate()?;
        Ok(config)
    }
}

/// Global server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    /// List of configured vaults
    pub vaults: Vec<VaultConfig>,
    /// Configuration profile name
    pub profile: String,

    // Core settings
    pub watch_for_changes: bool,
    pub max_file_size: u64,
    pub allowed_extensions: HashSet<String>,
    pub excluded_paths: HashSet<String>,
    pub enable_caching: bool,
    pub cache_ttl: u64,
    pub log_level: String,

    // Advanced settings
    pub template_dirs: Vec<PathBuf>,
    pub default_template_variables: serde_json::Value,
    pub editor_backup_enabled: bool,
    pub editor_atomic_writes: bool,
    pub max_backup_files: usize,
    pub max_edit_history: usize,
    pub backup_retention_days: u32,

    // Link graph settings
    pub link_graph_enabled: bool,
    pub link_suggestions_enabled: bool,
    pub max_link_suggestions: usize,
    pub link_similarity_threshold: f32,

    // Search settings
    pub full_text_search_enabled: bool,
    pub index_rebuild_interval: u64,

    // Multi-vault
    pub multi_vault_enabled: bool,

    // Admin
    pub metrics_enabled: bool,
    pub debug_mode: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            vaults: vec![],
            profile: "default".to_string(),
            watch_for_changes: true,
            max_file_size: 10 * 1024 * 1024, // 10MB
            allowed_extensions: [".md", ".txt", ".canvas"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            excluded_paths: [".obsidian", ".git", ".DS_Store", "node_modules"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            enable_caching: true,
            cache_ttl: 3600,
            log_level: "INFO".to_string(),
            template_dirs: vec![],
            default_template_variables: serde_json::json!({}),
            editor_backup_enabled: true,
            editor_atomic_writes: true,
            max_backup_files: 100,
            max_edit_history: 100,
            backup_retention_days: 7,
            link_graph_enabled: true,
            link_suggestions_enabled: true,
            max_link_suggestions: 10,
            link_similarity_threshold: 0.3,
            full_text_search_enabled: true,
            index_rebuild_interval: 3600,
            multi_vault_enabled: false,
            metrics_enabled: false,
            debug_mode: false,
        }
    }
}

impl ServerConfig {
    /// Create new configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate configuration
    pub fn validate(&self) -> Result<()> {
        if self.vaults.is_empty() {
            return Err(Error::config_error("At least one vault must be configured"));
        }

        // Check unique vault names
        let names: HashSet<_> = self.vaults.iter().map(|v| &v.name).collect();
        if names.len() != self.vaults.len() {
            return Err(Error::config_error("Vault names must be unique"));
        }

        // Check unique default vaults
        let defaults: Vec<_> = self.vaults.iter().filter(|v| v.is_default).collect();
        if defaults.len() > 1 {
            return Err(Error::config_error("Only one vault can be default"));
        }

        // Validate each vault
        for vault in &self.vaults {
            vault.validate()?;
        }

        Ok(())
    }

    /// Get default vault config
    pub fn default_vault(&self) -> Result<&VaultConfig> {
        self.vaults
            .iter()
            .find(|v| v.is_default)
            .or_else(|| self.vaults.first())
            .ok_or_else(|| Error::config_error("No default vault configured"))
    }

    /// Save vault configuration to file (for persistence)
    pub async fn save_vaults(&self, path: &Path) -> Result<()> {
        let yaml = serde_yaml::to_string(&self.vaults)
            .map_err(|e| Error::config_error(format!("Failed to serialize vaults: {}", e)))?;

        tokio::fs::write(path, yaml).await.map_err(|e| {
            Error::config_error(format!(
                "Failed to save vaults to {}: {}",
                path.display(),
                e
            ))
        })
    }

    /// Load vault configuration from file
    pub async fn load_vaults(path: &Path) -> Result<Vec<VaultConfig>> {
        if !path.exists() {
            return Ok(Vec::new()); // Return empty if file doesn't exist
        }

        let content = tokio::fs::read_to_string(path).await.map_err(|e| {
            Error::config_error(format!(
                "Failed to load vaults from {}: {}",
                path.display(),
                e
            ))
        })?;

        let vaults = serde_yaml::from_str(&content)
            .map_err(|e| Error::config_error(format!("Invalid vault configuration: {}", e)))?;

        Ok(vaults)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_vault_config_builder() {
        let temp = TempDir::new().unwrap();
        let vault = VaultConfig::builder("main", temp.path())
            .as_default()
            .watch_for_changes(true)
            .build();

        assert!(vault.is_ok());
        let v = vault.unwrap();
        assert_eq!(v.name, "main");
        assert!(v.is_default);
    }

    #[test]
    fn test_server_config_validation() {
        let mut config = ServerConfig::new();
        config.vaults.clear();
        assert!(config.validate().is_err());
    }
}
