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

    /// TurboVault's platform cache directory (`~/.cache/turbovault` on
    /// Linux and macOS), where state derived from a vault is kept outside
    /// it. Not created by this call.
    pub fn cache_root() -> Result<PathBuf> {
        Self::get_cache_dir()
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

    /// Record `vaults` in the project's registry, and `active_vault` as the
    /// active one. Each vault is added, or replaces the entry of the same
    /// name. Vaults already in the registry are kept, including ones another
    /// process registered since this one started, so processes sharing a
    /// project (one per Claude Desktop conversation) never drop each other's
    /// vaults. [`Self::remove_vault`] removes one.
    pub async fn save_vaults(&self, vaults: &[VaultConfig], active_vault: &str) -> Result<()> {
        let vaults = vaults.to_vec();
        let count = vaults.len();
        self.update(active_vault, move |registry| {
            for vault in vaults {
                match registry.iter_mut().find(|known| known.name == vault.name) {
                    Some(known) => *known = vault,
                    None => registry.push(vault),
                }
            }
        })
        .await?;
        log::debug!(
            "Saved {} vaults to project cache {} (active: {})",
            count,
            self.project_id,
            active_vault
        );
        Ok(())
    }

    /// Remove the vault called `name` from the project's registry, and record
    /// `active_vault` as the active one.
    pub async fn remove_vault(&self, name: &str, active_vault: &str) -> Result<()> {
        let name = name.to_string();
        self.update(active_vault, move |registry| {
            registry.retain(|vault| vault.name != name)
        })
        .await
    }

    /// Read the registry, apply `change`, and write it and the metadata back,
    /// all under the project's registry lock, so another process's update
    /// lands wholly before or after this one and is never lost between a read
    /// and a write. Both files are replaced atomically.
    async fn update(
        &self,
        active_vault: &str,
        change: impl FnOnce(&mut Vec<VaultConfig>) + Send + 'static,
    ) -> Result<()> {
        let vaults_file = self.vaults_file.clone();
        let metadata_file = self.metadata_file.clone();
        let lock_file = self.project_cache_dir.join("registry.lock");
        let metadata = CacheMetadata {
            active_vault: active_vault.to_string(),
            last_updated: Self::current_timestamp(),
            version: 1,
            project_id: self.project_id.clone(),
            working_dir: self.working_dir.to_string_lossy().to_string(),
        };

        tokio::task::spawn_blocking(move || {
            let lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&lock_file)
                .map_err(Error::io)?;
            lock.lock().map_err(Error::io)?;

            // A corrupt registry has been set aside by now; start afresh.
            let mut registry = read_registry(&vaults_file)?.unwrap_or_default();
            change(&mut registry);

            let vaults_yaml = yaml_serde::to_string(&registry)
                .map_err(|e| Error::config_error(format!("Failed to serialize vaults: {}", e)))?;
            crate::write_atomic(&vaults_file, vaults_yaml.as_bytes()).map_err(Error::io)?;
            let metadata_json = serde_json::to_string_pretty(&metadata)
                .map_err(|e| Error::config_error(format!("Failed to serialize metadata: {}", e)))?;
            crate::write_atomic(&metadata_file, metadata_json.as_bytes()).map_err(Error::io)
        })
        .await
        .map_err(|e| Error::other(format!("vault cache update failed: {e}")))?
    }

    /// Load vault configurations from cache. A registry that can't be read
    /// is set aside (see `read_registry`) and reported as an error.
    pub async fn load_vaults(&self) -> Result<Vec<VaultConfig>> {
        let vaults_file = self.vaults_file.clone();
        let vaults = tokio::task::spawn_blocking(move || read_registry(&vaults_file))
            .await
            .map_err(|e| Error::other(format!("vault cache read failed: {e}")))??
            .ok_or_else(|| {
                Error::config_error(
                    "Invalid vaults cache format; the unreadable file was kept as a backup \
                     next to it",
                )
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

/// Read the vault registry at `path`: empty if there is none yet, `None` if
/// it can't be parsed. An unparseable file is renamed aside to
/// `vaults.yaml.corrupt-<time>` with an error logged, so a later save starts
/// afresh without destroying the vaults it listed.
fn read_registry(path: &Path) -> Result<Option<Vec<VaultConfig>>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Some(Vec::new())),
        Err(error) => return Err(Error::io(error)),
    };
    let parsed = std::str::from_utf8(&bytes)
        .map_err(|e| e.to_string())
        .and_then(|text| yaml_serde::from_str(text).map_err(|e| e.to_string()));
    match parsed {
        Ok(vaults) => Ok(Some(vaults)),
        Err(reason) => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let backup = path.with_extension(format!("yaml.corrupt-{stamp}"));
            match std::fs::rename(path, &backup) {
                Ok(()) => log::error!(
                    "The vault registry {} could not be read ({reason}); kept it as {} \
                     and started a new one",
                    path.display(),
                    backup.display()
                ),
                Err(error) => log::error!(
                    "The vault registry {} could not be read ({reason}), nor kept aside: {error}",
                    path.display()
                ),
            }
            Ok(None)
        }
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

    /// Two processes for the same project each register a vault; neither
    /// save drops the other's.
    #[tokio::test]
    async fn saves_from_two_processes_keep_each_others_vaults() {
        let temp = TempDir::new().unwrap();
        let first = make_cache(&temp).await;
        let second = make_cache(&temp).await;

        first
            .save_vaults(&[make_vault_config("a", &temp.path().join("a"))], "a")
            .await
            .unwrap();
        second
            .save_vaults(&[make_vault_config("b", &temp.path().join("b"))], "b")
            .await
            .unwrap();

        let mut names: Vec<String> = first
            .load_vaults()
            .await
            .unwrap()
            .into_iter()
            .map(|v| v.name)
            .collect();
        names.sort();
        assert_eq!(names, ["a", "b"]);
        assert_eq!(first.load_metadata().await.unwrap().active_vault, "b");
    }

    /// Removing a vault drops only that one.
    #[tokio::test]
    async fn remove_vault_drops_only_that_vault() {
        let temp = TempDir::new().unwrap();
        let cache = make_cache(&temp).await;
        let vaults = [
            make_vault_config("a", &temp.path().join("a")),
            make_vault_config("b", &temp.path().join("b")),
        ];
        cache.save_vaults(&vaults, "a").await.unwrap();

        cache.remove_vault("a", "b").await.unwrap();
        let names: Vec<String> = cache
            .load_vaults()
            .await
            .unwrap()
            .into_iter()
            .map(|v| v.name)
            .collect();
        assert_eq!(names, ["b"]);
        assert_eq!(cache.load_metadata().await.unwrap().active_vault, "b");
    }

    /// A registry that can't be read is kept aside, not overwritten by the
    /// next save, so nothing it listed is lost for good.
    #[tokio::test]
    async fn a_corrupt_registry_is_kept_as_a_backup() {
        let temp = TempDir::new().unwrap();
        let cache = make_cache(&temp).await;
        let vaults_path = cache.project_cache_dir().join("vaults.yaml");
        tokio::fs::write(&vaults_path, b"{ not: [valid: yaml")
            .await
            .unwrap();

        assert!(cache.load_vaults().await.is_err());
        cache
            .save_vaults(&[make_vault_config("a", &temp.path().join("a"))], "a")
            .await
            .unwrap();

        let backups: Vec<_> = std::fs::read_dir(cache.project_cache_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with("vaults.yaml.corrupt-"))
            .collect();
        assert_eq!(backups.len(), 1, "{backups:?}");
        let kept = std::fs::read(cache.project_cache_dir().join(&backups[0])).unwrap();
        assert_eq!(kept, b"{ not: [valid: yaml");
        assert_eq!(cache.load_vaults().await.unwrap().len(), 1);
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
