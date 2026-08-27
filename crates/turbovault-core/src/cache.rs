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
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::fs;

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
        let working_dir = std::env::current_dir().map_err(Error::io)?;
        let project_id = Self::get_project_id(&working_dir)?;
        let project_cache_dir = cache_dir.join("projects").join(&project_id);

        // Create project cache directory if it doesn't exist
        if !project_cache_dir.exists() {
            fs::create_dir_all(&project_cache_dir)
                .await
                .map_err(Error::io)?;
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
        Self::init_with_project_in(project_root, Self::get_cache_dir()?).await
    }

    /// Like [`init_with_project`](Self::init_with_project) but with an explicit
    /// cache root instead of the platform default. Lets callers (notably tests)
    /// isolate the cache store without mutating the process-global
    /// `XDG_CACHE_HOME` environment variable.
    pub(crate) async fn init_with_project_in(
        project_root: &Path,
        cache_dir: PathBuf,
    ) -> Result<Self> {
        let project_id = Self::get_project_id(project_root)?;
        let project_cache_dir = cache_dir.join("projects").join(&project_id);

        // Create project cache directory if it doesn't exist
        if !project_cache_dir.exists() {
            fs::create_dir_all(&project_cache_dir)
                .await
                .map_err(Error::io)?;
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
        let markers = vec![
            ".git",
            ".obsidian",
            "Cargo.toml",
            "package.json",
            ".project",
        ];

        let mut current = start_path.to_path_buf();
        loop {
            for marker in &markers {
                let marker_path = current.join(marker);
                if marker_path.exists() {
                    // Found a project marker - use this directory as project root
                    let canonical = current.canonicalize().unwrap_or_else(|_| current.clone());
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
                let canonical = start_path
                    .canonicalize()
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
        crate::bytes_to_lower_hex(result)[..16].to_string() // Use first 16 chars of hash
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
                return Ok(PathBuf::from(local_app_data)
                    .join("turbovault")
                    .join("cache"));
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
    pub async fn save_vaults(&self, vaults: &[VaultConfig], active_vault: &str) -> Result<()> {
        // Save vaults as YAML
        let vaults_yaml = yaml_serde::to_string(vaults)
            .map_err(|e| Error::config_error(format!("Failed to serialize vaults: {}", e)))?;

        fs::write(&self.vaults_file, vaults_yaml)
            .await
            .map_err(Error::io)?;

        // Save metadata as JSON
        let metadata = CacheMetadata {
            active_vault: active_vault.to_string(),
            last_updated: Self::current_timestamp(),
            version: 1,
            project_id: self.project_id.clone(),
            working_dir: self.working_dir.to_string_lossy().to_string(),
        };

        let metadata_json = serde_json::to_string_pretty(&metadata)
            .map_err(|e| Error::config_error(format!("Failed to serialize metadata: {}", e)))?;

        fs::write(&self.metadata_file, metadata_json)
            .await
            .map_err(Error::io)?;

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

        let content = fs::read_to_string(&self.vaults_file)
            .await
            .map_err(Error::io)?;

        let vaults = yaml_serde::from_str(&content)
            .map_err(|e| Error::config_error(format!("Invalid vaults cache format: {}", e)))?;

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

        let content = fs::read_to_string(&self.metadata_file)
            .await
            .map_err(Error::io)?;

        let metadata = serde_json::from_str(&content)
            .map_err(|e| Error::config_error(format!("Invalid metadata cache format: {}", e)))?;

        Ok(metadata)
    }

    /// Clear all cached data for this project
    pub async fn clear(&self) -> Result<()> {
        if self.vaults_file.exists() {
            fs::remove_file(&self.vaults_file)
                .await
                .map_err(Error::io)?;
        }

        if self.metadata_file.exists() {
            fs::remove_file(&self.metadata_file)
                .await
                .map_err(Error::io)?;
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
    use crate::config::VaultConfig;
    use tempfile::TempDir;

    // ── Helpers ──────────────────────────────────────────────────────────────

    /// Build a VaultCache whose cache store lives entirely inside this test's
    /// temp directory, keeping tests isolated from each other and from the real
    /// user cache.
    ///
    /// The cache root is injected explicitly (no `XDG_CACHE_HOME` mutation).
    /// The previous implementation set that process-global env var, which is
    /// NOT thread-safe — `#[tokio::test]` does not serialize test functions
    /// (cargo runs them on parallel threads), so concurrent tests raced on the
    /// env var and could resolve into each other's temp dirs (beads
    /// turbovault-491). Injection removes the shared state entirely.
    async fn make_cache(temp: &TempDir) -> VaultCache {
        VaultCache::init_with_project_in(temp.path(), temp.path().join("cache"))
            .await
            .unwrap()
    }

    fn make_vault_config(name: &str, path: &std::path::Path) -> VaultConfig {
        VaultConfig {
            name: name.to_string(),
            path: path.to_path_buf(),
            is_default: false,
            reconcile_external_changes: None,
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

    // ── Tests ─────────────────────────────────────────────────────────────────

    #[test]
    fn test_hash_path_deterministic() {
        let path = Path::new("/home/user/projects/vault");
        let h1 = VaultCache::hash_path(path);
        let h2 = VaultCache::hash_path(path);
        assert_eq!(h1, h2, "same path must always produce the same hash");
        // The implementation truncates to 16 hex chars
        assert_eq!(h1.len(), 16);
    }

    #[test]
    fn test_hash_path_different_paths() {
        let h1 = VaultCache::hash_path(Path::new("/home/user/vault-a"));
        let h2 = VaultCache::hash_path(Path::new("/home/user/vault-b"));
        assert_ne!(h1, h2, "different paths must produce different hashes");
    }

    #[tokio::test]
    async fn test_init_creates_cache_directory() {
        let temp = TempDir::new().unwrap();
        let cache = VaultCache::init_with_project_in(temp.path(), temp.path().join("cache"))
            .await
            .unwrap();

        // The project cache directory should now exist on disk
        assert!(
            cache.project_cache_dir().exists(),
            "project cache dir should be created by init_with_project"
        );
        assert!(cache.project_cache_dir().is_dir());
    }

    #[tokio::test]
    async fn test_save_and_load_roundtrip() {
        let temp = TempDir::new().unwrap();
        let cache = make_cache(&temp).await;

        let configs = vec![
            make_vault_config("personal", &temp.path().join("personal")),
            make_vault_config("work", &temp.path().join("work")),
        ];

        cache.save_vaults(&configs, "personal").await.unwrap();

        let loaded = cache.load_vaults().await.unwrap();
        assert_eq!(loaded.len(), 2);

        let names: Vec<&str> = loaded.iter().map(|v| v.name.as_str()).collect();
        assert!(names.contains(&"personal"));
        assert!(names.contains(&"work"));

        let meta = cache.load_metadata().await.unwrap();
        assert_eq!(meta.active_vault, "personal");
    }

    #[tokio::test]
    async fn test_clear_removes_data() {
        let temp = TempDir::new().unwrap();
        let cache = make_cache(&temp).await;

        let configs = vec![make_vault_config("v1", &temp.path().join("v1"))];
        cache.save_vaults(&configs, "v1").await.unwrap();

        // Confirm data is there before clearing
        assert_eq!(cache.load_vaults().await.unwrap().len(), 1);

        cache.clear().await.unwrap();

        // After clearing, load_vaults should return an empty vec (no file = no data)
        let after = cache.load_vaults().await.unwrap();
        assert!(after.is_empty(), "cleared cache should return no vaults");
    }

    #[tokio::test]
    async fn test_load_nonexistent_returns_empty() {
        let temp = TempDir::new().unwrap();
        let cache = make_cache(&temp).await;

        // Never called save_vaults — vaults.yaml does not exist yet
        let vaults = cache.load_vaults().await.unwrap();
        assert!(
            vaults.is_empty(),
            "missing cache file should produce empty vec, not an error"
        );
    }

    #[tokio::test]
    async fn test_corrupted_cache_graceful() {
        let temp = TempDir::new().unwrap();
        let cache = make_cache(&temp).await;

        // Write invalid YAML directly to the vaults file
        let vaults_path = cache.project_cache_dir().join("vaults.yaml");
        tokio::fs::write(&vaults_path, b"{ not: [valid: yaml---\x00\xff")
            .await
            .unwrap();

        // load_vaults must not panic; it may return Err or Ok(empty)
        let result = cache.load_vaults().await;
        // Either outcome is acceptable — panic is not
        match result {
            Ok(vaults) => {
                // Tolerate implementations that silently discard corrupt caches
                let _ = vaults;
            }
            Err(_) => {
                // Tolerate implementations that surface the parse error
            }
        }
    }
}
