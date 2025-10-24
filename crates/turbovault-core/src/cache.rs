//! Cross-platform persistent cache for vault configuration and state
//!
//! This module provides a cache mechanism to persist vault registrations and
//! the active vault across server restarts. This solves the "amnesia" problem
//! where Claude Desktop starts a new process for each conversation, losing
//! all runtime vault state.
//!
//! PROJECT-AWARE CACHING:
//! Each project has its own vault registry, identified by:
//! 1. Project marker detection: .git/, .obsidian/, Cargo.toml, package.json
//! 2. Working directory: the directory where the server was started
//! 3. SHA256 hash of the working directory path for safe file naming
//!
//! Cache structure:
//! ~/.cache/turbovault/projects/{project_hash}/vaults.yaml
//! ~/.cache/turbovault/projects/{project_hash}/metadata.json
//!
//! Cache location:
//! - Linux/macOS: ~/.cache/turbovault/ or $XDG_CACHE_HOME/turbovault/
//! - Windows: %LOCALAPPDATA%\turbovault\cache\
//! - Fallback: ~/.turbovault/cache/ (all platforms)

use crate::config::VaultConfig;
use crate::error::{Error, Result};
use std::path::{Path, PathBuf};
use serde::{Deserialize, Serialize};
use tokio::fs;
use sha2::{Sha256, Digest};

/// Cache metadata: which vault is active and when cache was last updated
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMetadata {
    /// Currently active vault name
    pub active_vault: String,
    /// Unix timestamp of last cache update
    pub last_updated: u64,
    /// Cache format version for future compatibility
    pub version: u32,
    /// Project identifier (working directory hash)
    pub project_id: String,
    /// Working directory path (for reference)
    pub working_dir: String,
}

/// Persistent cache for vault state - PROJECT-AWARE
pub struct VaultCache {
    cache_dir: PathBuf,
    project_cache_dir: PathBuf,
    vaults_file: PathBuf,
    metadata_file: PathBuf,
    project_id: String,
    working_dir: PathBuf,
}

impl VaultCache {
    /// Initialize cache in the appropriate platform-specific directory
    /// Auto-detects project using markers (.git, .obsidian, Cargo.toml, package.json)
    pub async fn init() -> Result<Self> {
        let cache_dir = Self::get_cache_dir()?;
        let working_dir = std::env::current_dir()
            .map_err(Error::io)?;
        let project_id = Self::get_project_id(&working_dir)?;
        let project_cache_dir = cache_dir.join("projects").join(&project_id);

        // Create project cache directory if it doesn't exist
        if !project_cache_dir.exists() {
            fs::create_dir_all(&project_cache_dir).await.map_err(|e| {
                Error::io(e)
            })?;
        }

        Ok(Self {
            cache_dir,
            project_cache_dir: project_cache_dir.clone(),
            vaults_file: project_cache_dir.join("vaults.yaml"),
            metadata_file: project_cache_dir.join("metadata.json"),
            project_id,
            working_dir,
        })
    }

    /// Initialize cache with a specific project (useful for testing)
    pub async fn init_with_project(project_root: &Path) -> Result<Self> {
        let cache_dir = Self::get_cache_dir()?;
        let project_id = Self::get_project_id(project_root)?;
        let project_cache_dir = cache_dir.join("projects").join(&project_id);

        // Create project cache directory if it doesn't exist
        if !project_cache_dir.exists() {
            fs::create_dir_all(&project_cache_dir).await.map_err(|e| {
                Error::io(e)
            })?;
        }

        Ok(Self {
            cache_dir,
            project_cache_dir: project_cache_dir.clone(),
            vaults_file: project_cache_dir.join("vaults.yaml"),
            metadata_file: project_cache_dir.join("metadata.json"),
            project_id,
            working_dir: project_root.to_path_buf(),
        })
    }

    /// Detect project by looking for markers in parent directories
    /// Returns a hash of the project root directory path
    fn get_project_id(start_path: &Path) -> Result<String> {
        // Look for project markers going up the directory tree
        let markers = vec![".git", ".obsidian", "Cargo.toml", "package.json", ".project"];
        
        let mut current = start_path.to_path_buf();
        loop {
            for marker in &markers {
                let marker_path = current.join(marker);
                if marker_path.exists() {
                    // Found a project marker - use this directory as project root
                    let canonical = current.canonicalize()
                        .unwrap_or_else(|_| current.clone());
                    let project_id = Self::hash_path(&canonical);
                    log::debug!(
                        "Detected project root: {} (hash: {})",
                        canonical.display(),
                        project_id
                    );
                    return Ok(project_id);
                }
            }

            // Move up one directory
            if !current.pop() {
                // Reached filesystem root without finding markers
                // Use the original start path as project identifier
                let canonical = start_path.canonicalize()
                    .unwrap_or_else(|_| start_path.to_path_buf());
                let project_id = Self::hash_path(&canonical);
                log::debug!(
                    "No project marker found, using start path: {} (hash: {})",
                    canonical.display(),
                    project_id
                );
                return Ok(project_id);
            }
        }
    }

    /// Hash a path to create a safe filename
    fn hash_path(path: &Path) -> String {
        let path_str = path.to_string_lossy();
        let mut hasher = Sha256::new();
        hasher.update(path_str.as_bytes());
        let result = hasher.finalize();
        format!("{:x}", result)[..16].to_string() // Use first 16 chars of hash
    }

    /// Get the platform-specific cache directory
    fn get_cache_dir() -> Result<PathBuf> {
        if let Ok(cache_home) = std::env::var("XDG_CACHE_HOME") {
            // Linux with XDG_CACHE_HOME set
            return Ok(PathBuf::from(cache_home).join("turbovault"));
        }

        // Use platform-specific defaults
        #[cfg(target_os = "windows")]
        {
            if let Ok(local_app_data) = std::env::var("LOCALAPPDATA") {
                return Ok(PathBuf::from(local_app_data).join("turbovault").join("cache"));
            }
        }

        #[cfg(not(target_os = "windows"))]
        {
            if let Ok(home) = std::env::var("HOME") {
                return Ok(PathBuf::from(home).join(".cache").join("turbovault"));
            }
        }

        // Fallback to ~/.turbovault/cache/ for all platforms
        if let Ok(home) = std::env::var("HOME") {
            return Ok(PathBuf::from(home).join(".turbovault").join("cache"));
        }

        Err(Error::config_error(
            "Cannot determine cache directory: HOME not set and no platform-specific override found".to_string()
        ))
    }

    /// Save vault configurations and metadata
    pub async fn save_vaults(
        &self,
        vaults: &[VaultConfig],
        active_vault: &str,
    ) -> Result<()> {
        // Save vaults as YAML
        let vaults_yaml = serde_yaml::to_string(vaults).map_err(|e| {
            Error::config_error(format!("Failed to serialize vaults: {}", e))
        })?;

        fs::write(&self.vaults_file, vaults_yaml).await.map_err(|e| {
            Error::io(e)
        })?;

        // Save metadata as JSON
        let metadata = CacheMetadata {
            active_vault: active_vault.to_string(),
            last_updated: Self::current_timestamp(),
            version: 1,
            project_id: self.project_id.clone(),
            working_dir: self.working_dir.to_string_lossy().to_string(),
        };

        let metadata_json = serde_json::to_string_pretty(&metadata).map_err(|e| {
            Error::config_error(format!("Failed to serialize metadata: {}", e))
        })?;

        fs::write(&self.metadata_file, metadata_json).await.map_err(|e| {
            Error::io(e)
        })?;

        log::debug!(
            "Saved {} vaults to project cache {} (active: {})",
            vaults.len(),
            self.project_id,
            active_vault
        );

        Ok(())
    }

    /// Load vault configurations from cache
    pub async fn load_vaults(&self) -> Result<Vec<VaultConfig>> {
        if !self.vaults_file.exists() {
            return Ok(Vec::new()); // No cache yet
        }

        let content = fs::read_to_string(&self.vaults_file).await.map_err(|e| {
            Error::io(e)
        })?;

        let vaults = serde_yaml::from_str(&content).map_err(|e| {
            Error::config_error(format!("Invalid vaults cache format: {}", e))
        })?;

        log::debug!("Loaded vaults from project cache {}", self.project_id);

        Ok(vaults)
    }

    /// Load metadata (active vault, etc.)
    pub async fn load_metadata(&self) -> Result<CacheMetadata> {
        if !self.metadata_file.exists() {
            return Ok(CacheMetadata {
                active_vault: String::new(),
                last_updated: 0,
                version: 1,
                project_id: self.project_id.clone(),
                working_dir: self.working_dir.to_string_lossy().to_string(),
            });
        }

        let content = fs::read_to_string(&self.metadata_file).await.map_err(|e| {
            Error::io(e)
        })?;

        let metadata = serde_json::from_str(&content).map_err(|e| {
            Error::config_error(format!("Invalid metadata cache format: {}", e))
        })?;

        Ok(metadata)
    }

    /// Clear all cached data for this project
    pub async fn clear(&self) -> Result<()> {
        if self.vaults_file.exists() {
            fs::remove_file(&self.vaults_file).await.map_err(|e| {
                Error::io(e)
            })?;
        }

        if self.metadata_file.exists() {
            fs::remove_file(&self.metadata_file).await.map_err(|e| {
                Error::io(e)
            })?;
        }

        log::info!("Cache cleared for project {}", self.project_id);
        Ok(())
    }

    /// Get cache directory for diagnostics
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Get project cache directory for diagnostics
    pub fn project_cache_dir(&self) -> &Path {
        &self.project_cache_dir
    }

    /// Get project identifier for diagnostics
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Get working directory for diagnostics
    pub fn working_dir(&self) -> &Path {
        &self.working_dir
    }

    fn current_timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hash_path() {
        let path1 = Path::new("/home/user/projects/vault");
        let path2 = Path::new("/home/user/projects/vault");
        let hash1 = VaultCache::hash_path(path1);
        let hash2 = VaultCache::hash_path(path2);
        assert_eq!(hash1, hash2, "Same paths should hash to same value");
    }

    #[tokio::test]
    async fn test_cache_operations() {
        // This test would require more setup with temporary directories
        // Skipping detailed implementation for now
    }
}
