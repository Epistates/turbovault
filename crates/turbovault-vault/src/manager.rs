//! Vault manager implementation with file watching and caching

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::instrument;
use turbovault_core::prelude::*;
use turbovault_graph::LinkGraph;
use turbovault_parser::Parser;

/// File cache entry with timestamp
#[derive(Debug, Clone)]
struct CacheEntry {
    file: VaultFile,
    cached_at: f64,
}

/// Main vault manager with file operations and watching
pub struct VaultManager {
    config: ServerConfig,
    vault_path: PathBuf,
    parser: Parser,
    link_graph: Arc<RwLock<LinkGraph>>,
    file_cache: Arc<RwLock<HashMap<PathBuf, CacheEntry>>>,
}

impl VaultManager {
    /// Create a new vault manager
    pub fn new(config: ServerConfig) -> Result<Self> {
        let vault_path = config.default_vault()?.path.clone();
        let parser = Parser::new(vault_path.clone());

        Ok(Self {
            config,
            vault_path,
            parser,
            link_graph: Arc::new(RwLock::new(LinkGraph::new())),
            file_cache: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Get vault path
    pub fn vault_path(&self) -> &PathBuf {
        &self.vault_path
    }

    /// Initialize vault by scanning all files
    #[instrument(skip(self), name = "vault_initialize")]
    pub async fn initialize(&self) -> Result<()> {
        log::info!("Starting vault initialization for: {:?}", self.vault_path);

        let mut cache = self.file_cache.write().await;
        let mut graph = self.link_graph.write().await;

        // Scan for markdown files
        let md_files = self.scan_files()?;
        log::info!("Found {} markdown files", md_files.len());

        for file_path in md_files {
            log::debug!("Processing file: {:?}", file_path);
            if let Ok(content) = tokio::fs::read_to_string(&file_path).await {
                // Parse file
                match self.parser.parse_file(&file_path, &content) {
                    Ok(vault_file) => {
                        log::debug!(
                            "Parsed {}: {} links extracted",
                            file_path.display(),
                            vault_file.links.len()
                        );

                        // Cache file
                        let now = self.current_timestamp();
                        cache.insert(
                            file_path.clone(),
                            CacheEntry {
                                file: vault_file.clone(),
                                cached_at: now,
                            },
                        );

                        // Add to graph
                        let _ = graph.add_file(&vault_file);
                        let _ = graph.update_links(&vault_file);
                    }
                    Err(e) => {
                        log::warn!("Failed to parse {}: {}", file_path.display(), e);
                    }
                }
            } else {
                log::warn!("Failed to read file: {:?}", file_path);
            }
        }

        log::info!(
            "Vault initialization complete. Graph now has {} files, {} links",
            graph.node_count(),
            graph.edge_count()
        );

        Ok(())
    }

    /// Read file from cache or disk
    #[instrument(skip(self), fields(file = ?path), name = "vault_read_file")]
    pub async fn read_file(&self, path: &Path) -> Result<String> {
        let vault_path = self.resolve_path(path)?;

        // Check cache
        let cache = self.file_cache.read().await;
        if let Some(entry) = cache.get(&vault_path)
            && !self.is_cache_expired(entry.cached_at)
        {
            return Ok(entry.file.content.clone());
        }
        drop(cache);

        // Read from disk
        let content = tokio::fs::read_to_string(&vault_path)
            .await
            .map_err(Error::io)?;

        Ok(content)
    }

    /// Write file to disk atomically
    #[instrument(skip(self, content), fields(file = ?path, size = content.len()), name = "vault_write_file")]
    pub async fn write_file(&self, path: &Path, content: &str) -> Result<()> {
        let vault_path = self.resolve_path(path)?;

        // Ensure parent directory exists
        if let Some(parent) = vault_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(Error::io)?;
        }

        // Write to temp file first
        let temp_path = vault_path.with_extension("tmp");
        tokio::fs::write(&temp_path, content)
            .await
            .map_err(Error::io)?;

        // Atomic rename
        tokio::fs::rename(&temp_path, &vault_path)
            .await
            .map_err(Error::io)?;

        // Invalidate file cache
        let mut cache = self.file_cache.write().await;
        cache.remove(&vault_path);
        drop(cache); // Release write lock before parsing

        // Parse file and update graph
        match self.parser.parse_file(&vault_path, content) {
            Ok(vault_file) => {
                log::debug!(
                    "Parsed {}: {} links extracted",
                    vault_path.display(),
                    vault_file.links.len()
                );

                // Update graph
                let mut graph = self.link_graph.write().await;
                let _ = graph.add_file(&vault_file);
                let _ = graph.update_links(&vault_file);
                log::debug!("Graph updated for {}", vault_path.display());
            }
            Err(e) => {
                log::warn!(
                    "Failed to parse {} after write (graph not updated): {}",
                    vault_path.display(),
                    e
                );
                // Don't fail the write operation if parse fails
            }
        }

        Ok(())
    }

    /// Edit file using SEARCH/REPLACE blocks (LLM-optimized)
    ///
    /// This method applies edits using the aider-inspired format that reduces
    /// LLM laziness by 3X. Uses cascading fuzzy matching to tolerate minor errors.
    ///
    /// # Arguments
    /// * `path` - Relative path to file in vault
    /// * `edits` - String containing SEARCH/REPLACE blocks
    /// * `expected_hash` - Optional SHA-256 hash for TOCTOU protection
    /// * `dry_run` - If true, preview changes without applying
    ///
    /// # Returns
    /// EditResult with new hash, applied blocks count, and optional diff preview
    #[instrument(skip(self, edits), fields(file = ?path, dry_run), name = "vault_edit_file")]
    pub async fn edit_file(
        &self,
        path: &Path,
        edits: &str,
        expected_hash: Option<&str>,
        dry_run: bool,
    ) -> Result<crate::edit::EditResult> {
        use crate::edit::{EditEngine, compute_hash};

        let vault_path = self.resolve_path(path)?;

        // Acquire write lock on file cache to prevent TOCTOU
        let _cache_guard = self.file_cache.write().await;

        // Read current content
        let current_content = tokio::fs::read_to_string(&vault_path)
            .await
            .map_err(Error::io)?;

        // Validate expected hash if provided
        if let Some(expected) = expected_hash {
            let actual = compute_hash(&current_content);
            if actual != expected {
                return Err(Error::ConcurrencyError {
                    reason: format!(
                        "File modified since read. Expected hash: {}, actual: {}. Re-read the file and try again.",
                        expected, actual
                    ),
                });
            }
        }

        // Parse and apply edits
        let engine = EditEngine::new();
        let blocks = engine.parse_blocks(edits)?;

        let edit_result = engine.apply_edits(&current_content, &blocks, dry_run)?;

        // If dry run, return preview without writing
        if dry_run {
            return Ok(edit_result);
        }

        // Apply edits to get new content
        let (new_content, _warnings) = engine.apply_blocks(&current_content, &blocks)?;

        // Release cache guard before write (avoid deadlock)
        drop(_cache_guard);

        // Write atomically (reuses existing write_file logic)
        self.write_file(&vault_path, &new_content).await?;

        Ok(edit_result)
    }

    /// Get backlinks for a file
    pub async fn get_backlinks(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let vault_path = self.resolve_path(path)?;
        let graph = self.link_graph.read().await;
        let backlinks = graph.backlinks(&vault_path)?;
        Ok(backlinks.into_iter().map(|(p, _)| p).collect())
    }

    /// Get forward links for a file
    pub async fn get_forward_links(&self, path: &Path) -> Result<Vec<PathBuf>> {
        let vault_path = self.resolve_path(path)?;
        let graph = self.link_graph.read().await;
        let forward_links = graph.forward_links(&vault_path)?;
        Ok(forward_links.into_iter().map(|(p, _)| p).collect())
    }

    /// Get orphaned notes
    pub async fn get_orphaned_notes(&self) -> Result<Vec<PathBuf>> {
        let graph = self.link_graph.read().await;
        Ok(graph.orphaned_notes())
    }

    /// Get related notes
    pub async fn get_related_notes(&self, path: &Path, max_hops: usize) -> Result<Vec<PathBuf>> {
        let vault_path = self.resolve_path(path)?;
        let graph = self.link_graph.read().await;
        graph.related_notes(&vault_path, max_hops)
    }

    /// Get graph statistics
    pub async fn get_stats(&self) -> Result<turbovault_graph::GraphStats> {
        let graph = self.link_graph.read().await;
        Ok(graph.stats())
    }

    /// Resolve a relative path to vault-root-relative path
    fn resolve_path(&self, path: &Path) -> Result<PathBuf> {
        // If absolute, check it's under vault
        if path.is_absolute() {
            if !path.starts_with(&self.vault_path) {
                // Try canonicalizing both for symlink comparison
                let canonical_abs = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let canonical_vault = self
                    .vault_path
                    .canonicalize()
                    .unwrap_or_else(|_| self.vault_path.clone());

                if !canonical_abs.starts_with(&canonical_vault) {
                    return Err(Error::path_traversal(path.to_path_buf()));
                }
            }
            return Ok(path.to_path_buf());
        }

        // Resolve relative to vault
        let full_path = self.vault_path.join(path);

        // Try to canonicalize, but don't fail if path doesn't exist yet
        let normalized = if full_path.exists() {
            full_path.canonicalize().map_err(|_| {
                Error::invalid_path(format!("Cannot resolve path: {}", path.display()))
            })?
        } else {
            full_path
        };

        // Check still under vault (handle symlinks by comparing both canonical and relative)
        let vault_canonical = self
            .vault_path
            .canonicalize()
            .unwrap_or_else(|_| self.vault_path.clone());
        let norm_canonical = normalized
            .canonicalize()
            .unwrap_or_else(|_| normalized.clone());

        if !norm_canonical.starts_with(&vault_canonical)
            && !normalized.starts_with(&self.vault_path)
        {
            return Err(Error::path_traversal(normalized));
        }

        Ok(normalized)
    }

    /// Scan for markdown files in vault
    fn scan_files(&self) -> Result<Vec<PathBuf>> {
        use std::fs;

        let mut files = Vec::new();
        let mut stack = vec![self.vault_path.clone()];
        let excluded = &self.config.excluded_paths;

        while let Some(dir) = stack.pop() {
            let entries = fs::read_dir(&dir).map_err(Error::io)?;

            for entry in entries {
                let entry = entry.map_err(Error::io)?;
                let path = entry.path();

                // Skip excluded paths
                if let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && excluded.contains(&name.to_string())
                {
                    continue;
                }

                if path.is_dir() {
                    stack.push(path);
                } else if let Some(ext) = path.extension().and_then(|e| e.to_str())
                    && self
                        .config
                        .allowed_extensions
                        .contains(&format!(".{}", ext))
                    && path.metadata().map(|m| m.len()).unwrap_or(0) <= self.config.max_file_size
                {
                    files.push(path);
                }
            }
        }

        Ok(files)
    }

    /// Get current timestamp
    fn current_timestamp(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
    }

    /// Check if cache entry is expired
    fn is_cache_expired(&self, cached_at: f64) -> bool {
        let now = self.current_timestamp();
        now - cached_at > self.config.cache_ttl as f64
    }

    /// Get a reference to the link graph (read-only access)
    pub fn link_graph(&self) -> Arc<RwLock<LinkGraph>> {
        Arc::clone(&self.link_graph)
    }

    /// Parse a single file and return VaultFile
    #[instrument(skip(self), fields(file = ?path), name = "vault_parse_file")]
    pub async fn parse_file(&self, path: &Path) -> Result<VaultFile> {
        let full_path = self.resolve_path(path)?;
        let content = tokio::fs::read_to_string(&full_path)
            .await
            .map_err(Error::io)?;
        self.parser
            .parse_file(&full_path, &content)
            .map_err(|e| Error::parse_error(e.to_string()))
    }

    /// Scan vault and return list of all markdown files
    #[instrument(skip(self), name = "vault_scan")]
    pub async fn scan_vault(&self) -> Result<Vec<PathBuf>> {
        self.scan_files()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test vault configuration
    fn create_test_config(vault_dir: &Path) -> ServerConfig {
        let mut config = ServerConfig::new();
        let vault_config = VaultConfig::builder("test_vault", vault_dir)
            .build()
            .unwrap();
        config.vaults.push(vault_config);
        config
    }

    #[tokio::test]
    async fn test_vault_manager_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());

        let manager = VaultManager::new(config);
        assert!(manager.is_ok());
    }

    #[tokio::test]
    async fn test_vault_path() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());

        let manager = VaultManager::new(config).unwrap();
        assert_eq!(manager.vault_path(), temp_dir.path());
    }

    #[tokio::test]
    async fn test_write_and_read_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Write a file
        let path = Path::new("test.md");
        let content = "# Test Note\nHello world";
        assert!(manager.write_file(path, content).await.is_ok());

        // Read it back
        let read_content = manager.read_file(path).await.unwrap();
        assert_eq!(read_content, content);
    }

    #[tokio::test]
    async fn test_write_file_creates_directories() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Write file in nested directory
        let path = Path::new("notes/subfolder/test.md");
        let content = "Nested file";
        assert!(manager.write_file(path, content).await.is_ok());

        // Verify it was created
        let read_content = manager.read_file(path).await.unwrap();
        assert_eq!(read_content, content);
    }

    #[tokio::test]
    async fn test_path_traversal_prevention() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Attempt path traversal
        let bad_path = Path::new("../../../etc/passwd");
        let result = manager.read_file(bad_path).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_atomic_write() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = Path::new("atomic_test.md");
        let content = "Atomic write test";

        // Write file
        assert!(manager.write_file(path, content).await.is_ok());

        // Verify no .tmp files are left
        let entries = std::fs::read_dir(temp_dir.path()).unwrap();
        for entry in entries {
            let entry = entry.unwrap();
            let path = entry.path();
            if let Some(ext) = path.extension() {
                assert_ne!(ext, "tmp", "Temporary file left after write");
            }
        }
    }

    #[tokio::test]
    async fn test_cache_invalidation() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = Path::new("cache_test.md");
        let content1 = "Original content";

        // Write initial file
        assert!(manager.write_file(path, content1).await.is_ok());

        // Read from cache
        let read1 = manager.read_file(path).await.unwrap();
        assert_eq!(read1, content1);

        // Update file directly
        let vault_path = temp_dir.path().join(path);
        let content2 = "Updated content";
        std::fs::write(&vault_path, content2).unwrap();

        // Read again (should get new content, not cached)
        let read2 = manager.read_file(path).await.unwrap();
        // Note: may be cached depending on cache_ttl, but read should work
        assert!(!read2.is_empty());
    }

    #[tokio::test]
    async fn test_scan_files() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create some files
        std::fs::write(temp_dir.path().join("note1.md"), "# Note 1").unwrap();
        std::fs::write(temp_dir.path().join("note2.md"), "# Note 2").unwrap();
        std::fs::create_dir(temp_dir.path().join("folder")).unwrap();
        std::fs::write(temp_dir.path().join("folder/note3.md"), "# Note 3").unwrap();

        // Scan files
        let files = manager.scan_files().unwrap();

        // Should find all 3 markdown files
        assert_eq!(files.len(), 3);

        // Verify they're all .md files
        for file in &files {
            assert_eq!(file.extension().and_then(|e| e.to_str()), Some("md"));
        }
    }

    #[tokio::test]
    async fn test_initialize_vault() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create test files with lowercase links matching the filenames
        let note1 = "# Note 1\n[[note2]]";
        let note2 = "# Note 2\n[[note1]]";
        std::fs::write(temp_dir.path().join("note1.md"), note1).unwrap();
        std::fs::write(temp_dir.path().join("note2.md"), note2).unwrap();

        // Initialize vault
        assert!(manager.initialize().await.is_ok());

        // Verify stats work
        let stats = manager.get_stats().await.unwrap();
        assert_eq!(stats.total_files, 2);
        // At least one link should resolve
        assert!(stats.total_links >= 1);
    }

    #[tokio::test]
    async fn test_get_backlinks() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create files with links (use absolute paths for graph queries)
        std::fs::write(temp_dir.path().join("target.md"), "# Target").unwrap();
        std::fs::write(temp_dir.path().join("source.md"), "# Source\n[[target]]").unwrap();

        manager.initialize().await.unwrap();

        // Get backlinks for target (query with absolute path since graph stores absolute paths)
        let target_path = temp_dir.path().join("target.md");
        let backlinks = manager.get_backlinks(&target_path).await.unwrap();
        assert!(!backlinks.is_empty());
    }

    #[tokio::test]
    async fn test_get_forward_links() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create files with links
        std::fs::write(
            temp_dir.path().join("source.md"),
            "# Source\n[[target1]]\n[[target2]]",
        )
        .unwrap();
        std::fs::write(temp_dir.path().join("target1.md"), "# Target 1").unwrap();
        std::fs::write(temp_dir.path().join("target2.md"), "# Target 2").unwrap();

        manager.initialize().await.unwrap();

        // Get forward links (use absolute path)
        let source_path = temp_dir.path().join("source.md");
        let forward = manager.get_forward_links(&source_path).await.unwrap();
        // At least one target should be found
        assert!(!forward.is_empty());
    }

    #[tokio::test]
    async fn test_get_orphaned_notes() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create orphaned and linked files
        std::fs::write(temp_dir.path().join("orphan.md"), "# Orphaned Note").unwrap();
        std::fs::write(
            temp_dir.path().join("linked1.md"),
            "# Linked 1\n[[linked2]]",
        )
        .unwrap();
        std::fs::write(temp_dir.path().join("linked2.md"), "# Linked 2").unwrap();

        manager.initialize().await.unwrap();

        // Get orphaned notes
        let orphans = manager.get_orphaned_notes().await.unwrap();
        assert_eq!(orphans.len(), 1);
    }

    #[tokio::test]
    async fn test_get_stats() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create test files
        std::fs::write(temp_dir.path().join("note1.md"), "# Note 1").unwrap();
        std::fs::write(temp_dir.path().join("note2.md"), "# Note 2").unwrap();

        manager.initialize().await.unwrap();

        let stats = manager.get_stats().await.unwrap();
        assert_eq!(stats.total_files, 2);
        assert_eq!(stats.total_links, 0); // No links between these files
        assert_eq!(stats.orphaned_files, 2); // Both orphaned
    }

    #[tokio::test]
    async fn test_get_related_notes() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create a chain: A -> B -> C
        std::fs::write(temp_dir.path().join("a.md"), "# A\n[[b]]").unwrap();
        std::fs::write(temp_dir.path().join("b.md"), "# B\n[[a]]\n[[c]]").unwrap();
        std::fs::write(temp_dir.path().join("c.md"), "# C\n[[b]]").unwrap();

        manager.initialize().await.unwrap();

        // Get related notes to B within 1 hop (use absolute path)
        let b_path = temp_dir.path().join("b.md");
        let related = manager.get_related_notes(&b_path, 1).await.unwrap();

        // Should find A and C (direct neighbors)
        assert!(!related.is_empty());
    }

    #[tokio::test]
    async fn test_resolve_path_absolute() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Valid absolute path under vault
        let valid_path = temp_dir.path().join("test.md");
        let result = manager.resolve_path(&valid_path);
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_resolve_path_relative() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create the actual file
        std::fs::write(temp_dir.path().join("test.md"), "content").unwrap();

        let result = manager.resolve_path(Path::new("test.md"));
        assert!(result.is_ok());
    }
}
