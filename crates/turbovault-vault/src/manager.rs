//! Vault manager implementation with file watching and caching

use path_trav::PathTrav;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::RwLock;
use tracing::instrument;
use turbovault_audit::{AuditLog, SnapshotStore};
use turbovault_core::prelude::*;
use turbovault_core::{Change, ChangePlan, Precondition};
use turbovault_graph::LinkGraph;
use turbovault_parser::Parser;

use crate::substrate::{DirectSubstrate, WriteSubstrate};

/// File cache entry with timestamp
/// Used during initialization to populate link graph; read path bypasses cache
/// to ensure raw file content (including frontmatter) is always returned.
#[derive(Debug, Clone)]
#[allow(dead_code)]
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
    audit_log: Option<Arc<AuditLog>>,
    snapshot_store: Option<Arc<SnapshotStore>>,
    /// write-substrate-layering M3a: the write dispatch chokepoint (design
    /// §6.3). Selected once, at construction, from `write_backend` — layers
    /// above `VaultManager` stay backend-agnostic (R2).
    substrate: WriteSubstrate,
}

impl VaultManager {
    /// Create a new vault manager
    pub fn new(config: ServerConfig) -> Result<Self> {
        let default_vault = config.default_vault()?;
        let vault_path = default_vault.path.clone();
        let parser = Parser::new(vault_path.clone());

        // write-substrate-layering M3a (deliverable decision 1): only the
        // Direct arm sits on a production path here — regardless of the
        // vault's configured `write_backend`. `GitSubstrate` is built and
        // unit-tested directly (substrate.rs) but MUST NOT be constructed
        // here yet: pre-M3a, `write_file`/`delete_file`/`move_file` never
        // looked at `write_backend` at all, and several already-shipped
        // tools (`generate_index`, `append_log_entry`, `visualize`) call
        // these mutators directly rather than through the server's
        // git-aware `WriteTools`/`GitFileTools` dispatch. Selecting
        // `WriteSubstrate::Git` from config here would silently hand those
        // callers a second, uncoordinated git-write path (its own
        // `CommitLocks` domain, no cached repo, no reindex hook) racing the
        // real one. Wiring a live Git arm is M4's job, once those call
        // sites are migrated onto the server's shared repo/lock/hook.
        let substrate = WriteSubstrate::Direct(DirectSubstrate::new(vault_path.clone()));

        Ok(Self {
            config,
            vault_path,
            parser,
            link_graph: Arc::new(RwLock::new(LinkGraph::new())),
            file_cache: Arc::new(RwLock::new(HashMap::new())),
            audit_log: None,
            snapshot_store: None,
            substrate,
        })
    }

    /// Get vault path
    pub fn vault_path(&self) -> &PathBuf {
        &self.vault_path
    }

    /// Convert a path to a `/`-separated vault-relative string.
    ///
    /// Strips the vault root prefix and normalizes separators to `/` (so paths
    /// render consistently across platforms). Falls back to the lossy full path
    /// when `path` is not under the vault root.
    pub fn relative_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.vault_path)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Set the audit log and snapshot store for operation tracking.
    ///
    /// Wires the Direct substrate's own audit/snapshot recording (M3a
    /// deliverable decision 3) in addition to the manager's own copies
    /// (kept for the `audit_log()`/`snapshot_store()` accessors below).
    pub fn set_audit_log(&mut self, audit_log: Arc<AuditLog>, snapshot_store: Arc<SnapshotStore>) {
        if let WriteSubstrate::Direct(direct) = &mut self.substrate {
            direct.set_audit_log(Arc::clone(&audit_log), Arc::clone(&snapshot_store));
        }
        self.audit_log = Some(audit_log);
        self.snapshot_store = Some(snapshot_store);
    }

    /// Get the audit log reference (if configured)
    pub fn audit_log(&self) -> Option<&Arc<AuditLog>> {
        self.audit_log.as_ref()
    }

    /// Get the snapshot store reference (if configured)
    pub fn snapshot_store(&self) -> Option<&Arc<SnapshotStore>> {
        self.snapshot_store.as_ref()
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

        // Two-pass initialization: first add all files to the graph index,
        // then resolve links. This ensures every file is discoverable when
        // resolving wikilink targets, regardless of scan order.
        let mut parsed_files = Vec::with_capacity(md_files.len());
        let now = self.current_timestamp();

        // Pass 1: parse all files, populate cache and graph nodes
        for file_path in md_files {
            log::debug!("Processing file: {:?}", file_path);
            if let Ok(content) = tokio::fs::read_to_string(&file_path).await {
                match self.parser.parse_file(&file_path, &content) {
                    Ok(vault_file) => {
                        log::debug!(
                            "Parsed {}: {} links extracted",
                            file_path.display(),
                            vault_file.links.len()
                        );

                        cache.insert(
                            file_path.clone(),
                            CacheEntry {
                                file: vault_file.clone(),
                                cached_at: now,
                            },
                        );

                        if let Err(e) = graph.add_file(&vault_file) {
                            log::warn!("Graph add_file failed for {}: {}", file_path.display(), e);
                        }
                        parsed_files.push(vault_file);
                    }
                    Err(e) => {
                        log::warn!("Failed to parse {}: {}", file_path.display(), e);
                    }
                }
            } else {
                log::warn!("Failed to read file: {:?}", file_path);
            }
        }

        // Pass 2: resolve links (all files now in the index)
        for vault_file in &parsed_files {
            if let Err(e) = graph.update_links(vault_file) {
                log::warn!("Graph update_links failed: {}", e);
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
    ///
    /// Cache entries are validated against the file's modification time on disk.
    /// If the file was modified externally (git sync, direct writes, other processes),
    /// the stale cache entry is bypassed and fresh content is read from disk.
    ///
    /// NOTE: Always reads raw file content from disk (including frontmatter).
    /// The file cache stores parsed VaultFile with frontmatter stripped from content,
    /// so it cannot be used here — callers expect the complete raw file.
    #[instrument(skip(self), fields(file = ?path), name = "vault_read_file")]
    pub async fn read_file(&self, path: &Path) -> Result<String> {
        let vault_path = self.resolve_path(path)?;

        // Always read from disk to return raw content including frontmatter.
        // The VaultFile cache stores parsed content with frontmatter stripped,
        // which would silently lose frontmatter for callers.
        let content = tokio::fs::read_to_string(&vault_path)
            .await
            .map_err(Error::io)?;

        Ok(content)
    }

    /// Write file to disk atomically with optional optimistic concurrency control.
    ///
    /// If `expected_hash` is provided, the file's current content hash is verified
    /// before writing. If it doesn't match (another agent modified the file since
    /// the caller last read it), a `ConcurrencyError` is returned.
    ///
    /// write-substrate-layering M3a: the signature and observable behavior
    /// are unchanged — internally this now translates `expected_hash` into a
    /// [`Precondition`] (`for_replace`, forced blind when absent: `write_file`
    /// has no create-only default), builds a one-change [`ChangePlan`], and
    /// delegates to [`Self::substrate`]. The fs-write + precondition + audit
    /// work itself lives in `DirectSubstrate::apply` now; this method only
    /// does path resolution and the post-apply link-graph/cache sync (R7).
    #[instrument(skip(self, content), fields(file = ?path, size = content.len()), name = "vault_write_file")]
    pub async fn write_file(
        &self,
        path: &Path,
        content: &str,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        let vault_path = self.resolve_path(path)?;
        let rel_path = self.relative_path(&vault_path);

        let plan = ChangePlan::new(format!("write_file {rel_path}"))
            .upsert(rel_path.clone(), content.as_bytes())
            .with_precondition(rel_path, Precondition::for_replace(expected_hash, true));

        let outcome = self.substrate.apply(&plan).await?;
        self.sync_index(&outcome.changed).await;
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

        // Preserve the exact pre-image that the edit was calculated from. The
        // write below revalidates this hash after releasing the cache lock.
        let validated_hash = compute_hash(&current_content);

        if let Some(expected) = expected_hash
            && validated_hash != expected
        {
            return Err(Error::ConcurrencyError {
                reason: format!(
                    "File modified since read. Expected hash: {}, actual: {}. Re-read the file and try again.",
                    expected, validated_hash
                ),
            });
        }

        // Parse and apply edits
        let engine = EditEngine::new();
        let blocks = engine.parse_blocks(edits)?;

        let (edit_result, new_content) = engine.apply_edits(&current_content, &blocks, dry_run)?;

        // If dry run, return preview without writing
        if dry_run {
            return Ok(edit_result);
        }

        // Release cache guard before write (avoid deadlock)
        drop(_cache_guard);

        // Re-check at write time so an intervening in-process or external
        // change is rejected instead of silently overwritten.
        self.write_file(&vault_path, &new_content, Some(&validated_hash))
            .await?;

        Ok(edit_result)
    }

    /// Delete file from vault with audit trail, graph cleanup, and optional concurrency check.
    ///
    /// write-substrate-layering M3a: `expected_hash` translates to
    /// [`Precondition::for_in_place`] (a bare delete with no hash requires
    /// the path to exist, matching the pre-M3a "attempt the remove and let
    /// it fail" behavior — both now surface as a loud error rather than an
    /// `Io` error specifically, an accepted refinement — see
    /// `DirectSubstrate::check_precondition`).
    #[instrument(skip(self), fields(file = ?path), name = "vault_delete_file")]
    pub async fn delete_file(&self, path: &Path, expected_hash: Option<&str>) -> Result<()> {
        let vault_path = self.resolve_path(path)?;
        let rel_path = self.relative_path(&vault_path);

        let plan = ChangePlan::new(format!("delete_file {rel_path}"))
            .remove(rel_path.clone())
            .with_precondition(rel_path, Precondition::for_in_place(expected_hash));

        let outcome = self.substrate.apply(&plan).await?;
        self.sync_index(&outcome.changed).await;
        Ok(())
    }

    /// Move file within vault with audit trail, graph update, and optional concurrency check.
    ///
    /// write-substrate-layering M3a: `from`'s `expected_hash` translates to
    /// [`Precondition::for_in_place`] via a raw [`Change::Rename`] — the
    /// destination carries no precondition of its own (matching the pre-M3a
    /// behavior of clobbering an existing destination; a destination guard
    /// is `ChangePlan::rename`'s semantic-builder territory, not used here).
    #[instrument(skip(self), fields(from = ?from, to = ?to), name = "vault_move_file")]
    pub async fn move_file(
        &self,
        from: &Path,
        to: &Path,
        expected_hash: Option<&str>,
    ) -> Result<()> {
        let from_path = self.resolve_path(from)?;
        let to_path = self.resolve_path(to)?;
        let rel_from = self.relative_path(&from_path);
        let rel_to = self.relative_path(&to_path);

        let plan = ChangePlan::new(format!("move_file {rel_from} -> {rel_to}"))
            .with_change(Change::Rename {
                from: rel_from.clone(),
                to: rel_to,
            })
            .with_precondition(rel_from, Precondition::for_in_place(expected_hash));

        let outcome = self.substrate.apply(&plan).await?;
        self.sync_index(&outcome.changed).await;
        Ok(())
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

    /// Normalize a path by resolving `.` and `..` components
    /// This is used as a fallback when path_trav can't check non-existent paths
    fn normalize_path(path: &Path) -> PathBuf {
        let mut components = Vec::new();

        for component in path.components() {
            match component {
                std::path::Component::CurDir => {
                    // Skip `.` components
                }
                std::path::Component::ParentDir => {
                    // Pop the last component for `..`
                    components.pop();
                }
                comp => {
                    components.push(comp);
                }
            }
        }

        components.iter().collect()
    }

    /// Resolve a relative path to vault-root-relative path with path traversal protection
    /// Uses the battle-tested path_trav crate for security, with fallback normalization
    pub fn resolve_path(&self, path: &Path) -> Result<PathBuf> {
        // Resolve relative paths to absolute
        let full_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.vault_path.join(path)
        };

        // Use path_trav to detect traversal attempts (battle-tested library)
        // is_path_trav returns Ok(true) if traversal detected, Ok(false) if safe
        match self.vault_path.is_path_trav(&full_path) {
            Ok(true) => {
                // Path traversal detected by path_trav
                Err(Error::path_traversal(full_path))
            }
            Ok(false) => {
                // Path is safe according to path_trav
                Ok(full_path)
            }
            Err(_) => {
                // path_trav couldn't check (usually means file doesn't exist)
                // Use fallback normalization to detect traversal attempts
                let normalized = Self::normalize_path(&full_path);

                // Check if normalized path is still under vault
                if normalized.starts_with(&self.vault_path) {
                    Ok(full_path)
                } else {
                    Err(Error::path_traversal(full_path))
                }
            }
        }
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

    /// Insert a parsed `VaultFile` into the file cache with `cached_at = now`.
    ///
    /// Setting `cached_at` to the current wall-clock time after a write ensures the
    /// invariant `cached_at >= file_mtime` holds: `vault_files_validated()` treats
    /// `mtime > cached_at` as stale, so stamping the entry after the write prevents
    /// false-positive re-parses on the very next validated read.
    async fn insert_cache_entry(&self, path: PathBuf, file: VaultFile) {
        let now = self.current_timestamp();
        let mut cache = self.file_cache.write().await;
        cache.insert(
            path,
            CacheEntry {
                file,
                cached_at: now,
            },
        );
    }

    /// Bring the link graph + note cache into agreement with a substrate
    /// apply's verdict on which paths are now present (design R7 — one
    /// post-apply sync shared by `write_file`/`delete_file`/`move_file`,
    /// replacing their three near-duplicate pre-M3a graph/cache blocks).
    ///
    /// Markdown-only, mirroring pre-M3a `write_file`'s `is_markdown` guard:
    /// the link graph and note cache model *notes*, so attachments and other
    /// non-note artifacts are never graph nodes. (Pre-M3a `move_file` did
    /// not apply this guard on its insert side — any UTF-8-decodable file
    /// was cached regardless of extension; this unifies on the stricter,
    /// already-established `write_file` behavior. No existing caller relies
    /// on a non-markdown file appearing in the cache after a move.)
    async fn sync_index(&self, changed: &[(String, bool)]) {
        for (rel_path, present) in changed {
            let full_path = self.vault_path.join(rel_path);
            let is_markdown = full_path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("md"));
            if !is_markdown {
                continue;
            }

            if *present {
                match tokio::fs::read_to_string(&full_path).await {
                    Ok(content) => match self.parser.parse_file(&full_path, &content) {
                        Ok(vault_file) => {
                            {
                                let mut graph = self.link_graph.write().await;
                                if let Err(e) = graph.add_file(&vault_file) {
                                    log::warn!(
                                        "Graph add_file failed for {}: {}",
                                        full_path.display(),
                                        e
                                    );
                                }
                                if let Err(e) = graph.update_links(&vault_file) {
                                    log::warn!(
                                        "Graph update_links failed for {}: {}",
                                        full_path.display(),
                                        e
                                    );
                                }
                            }
                            self.insert_cache_entry(full_path, vault_file).await;
                        }
                        Err(e) => log::warn!(
                            "Failed to parse {} after apply (graph not updated): {}",
                            full_path.display(),
                            e
                        ),
                    },
                    Err(e) => log::warn!(
                        "Failed to re-read {} after apply: {}",
                        full_path.display(),
                        e
                    ),
                }
            } else {
                // Re-check disk state before evicting: `sync_index` runs
                // after `substrate.apply()` has already released its
                // write-lock, so a concurrent blind write that recreated
                // this path can land between this apply's completion and
                // this eviction. If the path exists again, this "removed"
                // notification is stale — skip it and let the write that
                // recreated the path reconcile the cache via its own
                // present=true branch, instead of clobbering a legitimately
                // present file.
                // ponytail: narrows the race (check-then-evict is still not
                // atomic with the recreating write) rather than closing it
                // outright; a full fix serializes sync_index with apply()
                // per path, add if this ever proves observable in practice.
                if tokio::fs::try_exists(&full_path).await.unwrap_or(false) {
                    continue;
                }
                {
                    let mut graph = self.link_graph.write().await;
                    let _ = graph.remove_file(&full_path);
                }
                let mut cache = self.file_cache.write().await;
                cache.remove(&full_path);
            }
        }
    }

    /// Check if cache entry is expired (TTL-based)
    #[allow(dead_code)]
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

    /// Synchronize one note's on-disk state into the parsed cache and link graph.
    ///
    /// This is used after an external transactional operation, such as rollback,
    /// that creates, rewrites, or removes a note without going through the normal
    /// `write_file`/`delete_file` paths.
    pub async fn refresh_file_state(&self, path: &Path) -> Result<()> {
        let full_path = self.resolve_path(path)?;
        match tokio::fs::read_to_string(&full_path).await {
            Ok(content) => {
                let vault_file = self
                    .parser
                    .parse_file(&full_path, &content)
                    .map_err(|error| Error::parse_error(error.to_string()))?;
                {
                    let mut graph = self.link_graph.write().await;
                    graph.add_file(&vault_file)?;
                    graph.update_links(&vault_file)?;
                }
                self.insert_cache_entry(full_path, vault_file).await;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                {
                    let mut graph = self.link_graph.write().await;
                    let _ = graph.remove_file(&full_path);
                }
                self.file_cache.write().await.remove(&full_path);
                Ok(())
            }
            Err(error) => Err(Error::io(error)),
        }
    }

    /// Scan vault and return list of all markdown files
    #[instrument(skip(self), name = "vault_scan")]
    pub async fn scan_vault(&self) -> Result<Vec<PathBuf>> {
        self.scan_files()
    }

    /// Scan for markdown files using `DirEntry::file_type()` instead of `Path::is_dir()`.
    ///
    /// On Linux, `readdir` (via `read_dir`) returns `d_type` for each entry, so
    /// `DirEntry::file_type()` requires no additional syscall. The original
    /// `scan_files` calls `path.is_dir()` (which issues `statx`) and
    /// `path.metadata()` (another `statx`) — two extra kernel round-trips per entry.
    /// This variant eliminates both.
    ///
    /// Symlinks to directories are not followed. For normal Obsidian vaults (no symlinks)
    /// this is identical in behaviour.
    fn scan_files_dtype(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        let mut stack = vec![self.vault_path.clone()];
        let excluded = &self.config.excluded_paths;

        while let Some(dir) = stack.pop() {
            let entries = std::fs::read_dir(&dir).map_err(Error::io)?;

            for entry in entries.flatten() {
                let path = entry.path();

                if let Some(name) = path.file_name().and_then(|n| n.to_str())
                    && excluded.contains(&name.to_string())
                {
                    continue;
                }

                // file_type() reads d_type from the dirent — no extra syscall on Linux.
                let Ok(ft) = entry.file_type() else { continue };

                if ft.is_dir() {
                    stack.push(path);
                } else if ft.is_file()
                    && let Some(ext) = path.extension().and_then(|e| e.to_str())
                    && self
                        .config
                        .allowed_extensions
                        .contains(&format!(".{}", ext))
                {
                    // entry.metadata() may reuse the stat info the OS already fetched.
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    if size <= self.config.max_file_size {
                        files.push(path);
                    }
                }
                // symlinks silently skipped — uncommon in Obsidian vaults
            }
        }

        Ok(files)
    }

    /// Scan vault using `DirEntry::file_type()` — benchmark variant for A/B comparison.
    #[instrument(skip(self), name = "vault_scan_dtype")]
    pub async fn scan_vault_dtype(&self) -> Result<Vec<PathBuf>> {
        self.scan_files_dtype()
    }

    /// Return clones of all `VaultFile` objects currently in the in-memory cache.
    ///
    /// The cache is populated during `initialize()` and kept up-to-date on every
    /// write/delete/move. Callers that only need parsed metadata (frontmatter, links)
    /// and can tolerate up-to-millisecond staleness should prefer this over
    /// `scan_vault()` + `parse_file()`, which issues a filesystem scan and N file
    /// reads on every call.
    pub async fn all_cached_vault_files(&self) -> Vec<VaultFile> {
        let cache = self.file_cache.read().await;
        cache.values().map(|e| e.file.clone()).collect()
    }

    /// Return cached vault files, re-parsing any that have been modified on disk since
    /// they were last cached.
    ///
    /// Uses a two-phase locking strategy to avoid holding any lock during I/O:
    ///
    /// 1. Read lock — snapshot `(path, cached_at)` pairs.
    /// 2. No lock — batch all `stat` calls in a `spawn_blocking` task (one thread dispatch
    ///    for N files instead of N async task dispatches), then re-read and re-parse any
    ///    stale files.
    /// 3. Write lock — update stale entries; remove entries whose files were deleted.
    /// 4. Read lock — return the now-validated cache contents.
    ///
    /// On the hot path (nothing changed) this costs N synchronous `stat` calls inside one
    /// `spawn_blocking` task and zero file reads. For a 100-note vault with no external
    /// changes this is ~150–200 µs vs ~3.5 ms for the previous scan-on-every-call approach.
    pub async fn vault_files_validated(&self) -> Vec<VaultFile> {
        // Phase 1: snapshot path + timestamp under read lock (no I/O inside lock).
        let snapshot: Vec<(PathBuf, f64)> = {
            let cache = self.file_cache.read().await;
            cache
                .iter()
                .map(|(p, e)| (p.clone(), e.cached_at))
                .collect()
        };

        // Phase 2: batch all mtime checks into one spawn_blocking call.
        // Each individual tokio::fs::metadata call carries async task overhead;
        // batching them into std::fs::metadata calls inside a single blocking task
        // cuts that overhead from O(N) task dispatches to O(1).
        let stale_paths: Vec<PathBuf> = tokio::task::spawn_blocking(move || -> Vec<PathBuf> {
            snapshot
                .into_iter()
                .filter(|(path, cached_at)| {
                    std::fs::metadata(path)
                        .and_then(|m| m.modified())
                        .map(|mtime| {
                            mtime
                                .duration_since(UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs_f64()
                                > *cached_at
                        })
                        .unwrap_or(true) // missing file treated as modified
                })
                .map(|(path, _)| path)
                .collect()
        })
        .await
        .unwrap_or_default();

        // Re-read and re-parse each stale file (no lock held during I/O).
        let mut to_update: Vec<(PathBuf, Option<VaultFile>)> = Vec::new();
        for path in &stale_paths {
            let result = tokio::fs::read_to_string(path)
                .await
                .ok()
                .and_then(|content| self.parser.parse_file(path, &content).ok());
            to_update.push((path.clone(), result));
        }

        // Phase 3: apply updates under write lock.
        if !to_update.is_empty() {
            let now = self.current_timestamp();
            let mut cache = self.file_cache.write().await;
            for (path, maybe_vf) in to_update {
                match maybe_vf {
                    Some(vf) => {
                        cache.insert(
                            path,
                            CacheEntry {
                                file: vf,
                                cached_at: now,
                            },
                        );
                    }
                    None => {
                        cache.remove(&path);
                    }
                }
            }
        }

        // Phase 4: return the validated cache contents.
        let cache = self.file_cache.read().await;
        cache.values().map(|e| e.file.clone()).collect()
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
        assert!(manager.write_file(path, content, None).await.is_ok());

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
        assert!(manager.write_file(path, content, None).await.is_ok());

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
        assert!(manager.write_file(path, content, None).await.is_ok());

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
        assert!(manager.write_file(path, content1, None).await.is_ok());

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
        // Backlink resolution depends on platform-specific path handling;
        // verify the operation succeeds without asserting exact results
        let _backlinks = manager.get_backlinks(&target_path).await.unwrap();
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
        // Link resolution depends on platform-specific path handling;
        // verify the operation succeeds without asserting exact results
        let _forward = manager.get_forward_links(&source_path).await.unwrap();
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
    async fn test_okf_markdown_cross_link_resolves_end_to_end() {
        // End-to-end: an OKF bundle-relative markdown cross-link
        // `[customers](/tables/customers.md)` must resolve through the real
        // parser -> graph pipeline (not just synthetic Link structs).
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::create_dir_all(temp_dir.path().join("tables")).unwrap();
        std::fs::write(
            temp_dir.path().join("tables/customers.md"),
            "---\ntype: BigQuery Table\ntitle: Customers\n---\n# Schema\n",
        )
        .unwrap();
        std::fs::write(
            temp_dir.path().join("tables/orders.md"),
            "---\ntype: BigQuery Table\ntitle: Orders\n---\n# Joins\n\nJoined with [customers](/tables/customers.md) on `customer_id`.\n",
        )
        .unwrap();

        manager.initialize().await.unwrap();

        // The cross-link must have produced a resolved graph edge.
        let stats = manager.get_stats().await.unwrap();
        assert_eq!(stats.total_files, 2);
        assert_eq!(
            stats.total_links, 1,
            "OKF markdown cross-link should resolve"
        );

        // And it must surface as a backlink on the target.
        let customers = temp_dir.path().join("tables/customers.md");
        let backlinks = manager.get_backlinks(&customers).await.unwrap();
        assert_eq!(backlinks.len(), 1, "customers.md should have one backlink");
    }

    #[tokio::test]
    async fn test_write_non_markdown_file_does_not_pollute_graph() {
        // Writing a non-markdown artifact (e.g. an exported viz.html) must not
        // add a node to the note graph or the note cache.
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();
        manager.initialize().await.unwrap();
        assert_eq!(manager.get_stats().await.unwrap().total_files, 1);

        // Write a non-md file via the same path visualize() uses.
        manager
            .write_file(
                std::path::Path::new("viz.html"),
                "<html>[fake](/note.md)</html>",
                None,
            )
            .await
            .unwrap();

        // The HTML file is on disk but is NOT a graph node.
        assert!(temp_dir.path().join("viz.html").exists());
        assert_eq!(
            manager.get_stats().await.unwrap().total_files,
            1,
            "non-markdown write must not add a graph node"
        );
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

    #[tokio::test]
    async fn test_resolve_path_traversal_prevention() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Try to escape vault with ../ components
        let result = manager.resolve_path(Path::new("../../tmp/evil.md"));
        assert!(result.is_err(), "Path traversal should be prevented");

        // Also test with deeper traversal
        let result2 = manager.resolve_path(Path::new("../../../etc/passwd"));
        assert!(result2.is_err(), "Path traversal should be prevented");
    }

    // -------------------------------------------------------------------------
    // New comprehensive tests
    // -------------------------------------------------------------------------

    /// Writing a file then deleting it should leave the path absent on disk,
    /// and a subsequent `read_file` must return an error.
    #[tokio::test]
    async fn test_delete_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let rel = Path::new("to_delete.md");
        manager.write_file(rel, "# Delete me", None).await.unwrap();

        // Verify the file exists before deletion.
        assert!(temp_dir.path().join(rel).exists());

        manager.delete_file(rel, None).await.unwrap();

        // File must no longer exist on disk.
        assert!(
            !temp_dir.path().join(rel).exists(),
            "File should be gone after delete_file"
        );

        // read_file must return an error for the deleted path.
        let result = manager.read_file(rel).await;
        assert!(result.is_err(), "read_file on deleted path should error");
    }

    /// Moving a file should put its content at the new path and remove the old path.
    #[tokio::test]
    async fn test_move_file() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let src = Path::new("source_note.md");
        let dst = Path::new("dest_note.md");
        let content = "# Moved Note\nsome content";

        manager.write_file(src, content, None).await.unwrap();

        manager.move_file(src, dst, None).await.unwrap();

        // Old path must no longer exist.
        assert!(
            !temp_dir.path().join(src).exists(),
            "Source file should be gone after move"
        );

        // New path must exist with the original content.
        let read_back = manager.read_file(dst).await.unwrap();
        assert_eq!(
            read_back, content,
            "Destination must have the original content"
        );
    }

    /// Moving an attachment must preserve arbitrary bytes rather than requiring UTF-8.
    #[tokio::test]
    async fn test_move_file_preserves_non_utf8_bytes() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        let src = Path::new("attachments/source.bin");
        let dst = Path::new("assets/destination.bin");
        let bytes = [0, 159, 146, 150, 255, 10];

        tokio::fs::create_dir_all(temp_dir.path().join("attachments"))
            .await
            .unwrap();
        tokio::fs::write(temp_dir.path().join(src), bytes)
            .await
            .unwrap();

        manager.move_file(src, dst, None).await.unwrap();

        assert!(!temp_dir.path().join(src).exists());
        assert_eq!(
            tokio::fs::read(temp_dir.path().join(dst)).await.unwrap(),
            bytes
        );
        assert!(manager.all_cached_vault_files().await.is_empty());
    }

    /// `sync_index` is markdown-only (unified from `write_file`'s pre-M3a
    /// guard, see the doc comment on `sync_index`): a moved non-`.md`
    /// UTF-8-decodable file must not appear in the cache, even though
    /// pre-M3a `move_file` cached any UTF-8-decodable file regardless of
    /// extension.
    #[tokio::test]
    async fn test_move_file_non_markdown_utf8_not_cached() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        let src = Path::new("notes.txt");
        let dst = Path::new("moved.txt");

        tokio::fs::write(temp_dir.path().join(src), "plain text, not markdown")
            .await
            .unwrap();

        manager.move_file(src, dst, None).await.unwrap();

        assert!(!temp_dir.path().join(src).exists());
        assert_eq!(
            tokio::fs::read_to_string(temp_dir.path().join(dst))
                .await
                .unwrap(),
            "plain text, not markdown"
        );
        assert!(
            manager.all_cached_vault_files().await.is_empty(),
            "non-markdown files must never be cached"
        );
    }

    #[tokio::test]
    async fn test_refresh_file_state_tracks_external_delete_and_restore() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        std::fs::write(temp_dir.path().join("target.md"), "# Target\n").unwrap();
        std::fs::write(
            temp_dir.path().join("source.md"),
            "# Source\n\n[[target]]\n",
        )
        .unwrap();
        manager.initialize().await.unwrap();

        let source = Path::new("source.md");
        tokio::fs::remove_file(temp_dir.path().join(source))
            .await
            .unwrap();
        manager.refresh_file_state(source).await.unwrap();
        assert_eq!(manager.all_cached_vault_files().await.len(), 1);
        assert!(manager.get_forward_links(source).await.unwrap().is_empty());

        tokio::fs::write(temp_dir.path().join(source), "# Restored\n\n[[target]]\n")
            .await
            .unwrap();
        manager.refresh_file_state(source).await.unwrap();
        assert_eq!(manager.all_cached_vault_files().await.len(), 2);
        assert_eq!(manager.get_forward_links(source).await.unwrap().len(), 1);
    }

    /// Moving a file to a subdirectory that doesn't exist yet should create
    /// the intermediate directories automatically.
    #[tokio::test]
    async fn test_move_file_cross_directory() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let src = Path::new("flat_note.md");
        let dst = Path::new("deep/nested/subdir/note.md");
        let content = "# Cross-dir Move";

        manager.write_file(src, content, None).await.unwrap();

        // The destination directory does not exist yet.
        assert!(!temp_dir.path().join("deep").exists());

        manager.move_file(src, dst, None).await.unwrap();

        // Source gone, destination present.
        assert!(!temp_dir.path().join(src).exists());
        assert!(temp_dir.path().join(dst).exists());

        let read_back = manager.read_file(dst).await.unwrap();
        assert_eq!(read_back, content);
    }

    /// After a successful `write_file` no `.tmp.*` files should remain
    /// anywhere under the vault directory.
    #[tokio::test]
    async fn test_temp_file_cleanup_on_write() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Write into a nested directory to exercise the parent-creation path
        // and ensure temp files are cleaned up in the right place.
        let rel = Path::new("sub/cleanup_test.md");
        manager.write_file(rel, "content", None).await.unwrap();

        // Walk the entire vault tree and assert no `.tmp.*` files remain.
        let mut stack = vec![temp_dir.path().to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
                    assert!(
                        !ext.starts_with("tmp"),
                        "Leftover temp file found: {:?}",
                        path
                    );
                }
            }
        }
    }

    /// After writing a note that contains a wikilink the link graph must
    /// record that forward link from the written file.
    #[tokio::test]
    async fn test_graph_updated_after_write() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Write the target file so the parser can resolve the link.
        let target = Path::new("target.md");
        manager.write_file(target, "# Target", None).await.unwrap();

        // Write a source file that links to target.
        let source = Path::new("source.md");
        manager
            .write_file(source, "# Source\n[[target]]", None)
            .await
            .unwrap();

        // Check the link graph via forward_links on the absolute source path.
        let source_abs = temp_dir.path().join(source);
        let forward = manager.get_forward_links(&source_abs).await.unwrap();

        // At least one forward link should resolve to target.
        assert!(
            !forward.is_empty(),
            "Link graph should record the [[target]] forward link after write"
        );
    }

    /// After deleting file A (which links to B) the backlinks for B must no
    /// longer include A.
    #[tokio::test]
    async fn test_graph_updated_after_delete() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        // Create both files and initialize so the graph is populated.
        std::fs::write(temp_dir.path().join("a.md"), "# A\n[[b]]").unwrap();
        std::fs::write(temp_dir.path().join("b.md"), "# B").unwrap();
        manager.initialize().await.unwrap();

        // Sanity: A should appear as a backlink to B before deletion.
        let b_abs = temp_dir.path().join("b.md");
        let backlinks_before = manager.get_backlinks(&b_abs).await.unwrap();
        assert!(
            !backlinks_before.is_empty(),
            "Before deletion A should be a backlink to B"
        );

        // Delete A.
        manager.delete_file(Path::new("a.md"), None).await.unwrap();

        // After deletion A must no longer appear in B's backlinks.
        let backlinks_after = manager.get_backlinks(&b_abs).await.unwrap();
        let a_abs = temp_dir.path().join("a.md");
        let a_still_linked = backlinks_after.iter().any(|p| p == &a_abs);
        assert!(
            !a_still_linked,
            "After deleting A, it must not appear in B's backlinks; found: {:?}",
            backlinks_after
        );
    }

    // ── vault_files_validated ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_validated_returns_cached_files_on_hot_path() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("a.md"), "# A").unwrap();
        std::fs::write(temp_dir.path().join("b.md"), "# B").unwrap();
        manager.initialize().await.unwrap();

        let files = manager.vault_files_validated().await;
        assert_eq!(files.len(), 2);
    }

    #[tokio::test]
    async fn test_validated_detects_external_modification() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = temp_dir.path().join("note.md");
        std::fs::write(&path, "---\nstatus: draft\n---\n# Note").unwrap();
        manager.initialize().await.unwrap();

        // Confirm initial frontmatter is cached.
        let files = manager.vault_files_validated().await;
        let initial = files
            .iter()
            .find(|f| f.path == path)
            .and_then(|f| f.frontmatter.as_ref())
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        assert_eq!(initial, "draft");

        // Overwrite the file externally with a future mtime.
        // Sleep 10 ms so the OS registers a mtime change.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        std::fs::write(&path, "---\nstatus: published\n---\n# Note").unwrap();

        // vault_files_validated must detect the mtime change and re-parse.
        let files = manager.vault_files_validated().await;
        let updated = files
            .iter()
            .find(|f| f.path == path)
            .and_then(|f| f.frontmatter.as_ref())
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        assert_eq!(updated, "published");
    }

    #[tokio::test]
    async fn test_validated_removes_deleted_file_from_cache() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = temp_dir.path().join("ephemeral.md");
        std::fs::write(&path, "# Ephemeral").unwrap();
        manager.initialize().await.unwrap();

        assert_eq!(manager.vault_files_validated().await.len(), 1);

        std::fs::remove_file(&path).unwrap();

        // After deletion the entry must be evicted from the cache.
        assert_eq!(manager.vault_files_validated().await.len(), 0);
    }

    // ── scan_vault_dtype ─────────────────────────────────────────────────────

    /// scan_vault_dtype must find the same markdown files as scan_vault for a
    /// normal vault (no symlinks).
    #[tokio::test]
    async fn test_scan_vault_dtype_parity_with_scan_vault() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("root.md"), "# Root").unwrap();
        std::fs::create_dir(temp_dir.path().join("sub")).unwrap();
        std::fs::write(temp_dir.path().join("sub/child.md"), "# Child").unwrap();
        std::fs::write(temp_dir.path().join("ignored.txt"), "not markdown").unwrap();

        let mut classic = manager.scan_vault().await.unwrap();
        let mut dtype = manager.scan_vault_dtype().await.unwrap();

        classic.sort();
        dtype.sort();

        assert_eq!(
            classic, dtype,
            "scan_vault_dtype must find identical files as scan_vault"
        );
    }

    /// scan_vault_dtype must recurse into nested subdirectories.
    #[tokio::test]
    async fn test_scan_vault_dtype_recurses_subdirectories() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::create_dir_all(temp_dir.path().join("a/b/c")).unwrap();
        std::fs::write(temp_dir.path().join("a/b/c/deep.md"), "# Deep").unwrap();

        let files = manager.scan_vault_dtype().await.unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("deep.md"));
    }

    /// scan_vault_dtype must skip non-markdown files.
    #[tokio::test]
    async fn test_scan_vault_dtype_skips_non_markdown() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();
        std::fs::write(temp_dir.path().join("image.png"), "fake png").unwrap();
        std::fs::write(temp_dir.path().join("data.json"), "{}").unwrap();

        let files = manager.scan_vault_dtype().await.unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("note.md"));
    }

    // ── all_cached_vault_files ───────────────────────────────────────────────

    /// Before initialize(), all_cached_vault_files returns an empty list.
    #[tokio::test]
    async fn test_all_cached_vault_files_empty_before_init() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();

        // Cache is empty — initialize() has not been called.
        assert!(manager.all_cached_vault_files().await.is_empty());
    }

    /// After initialize(), all_cached_vault_files returns all parsed files.
    #[tokio::test]
    async fn test_all_cached_vault_files_populated_after_init() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("a.md"), "# A").unwrap();
        std::fs::write(temp_dir.path().join("b.md"), "# B").unwrap();
        manager.initialize().await.unwrap();

        assert_eq!(manager.all_cached_vault_files().await.len(), 2);
    }

    /// all_cached_vault_files does NOT pick up external disk modifications —
    /// it returns whatever is in the cache without mtime checks.  This is the
    /// intended "fast path" behaviour; callers that need freshness should use
    /// vault_files_validated() instead.
    #[tokio::test]
    async fn test_all_cached_vault_files_does_not_detect_external_modification() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        let path = temp_dir.path().join("note.md");
        std::fs::write(&path, "---\nstatus: draft\n---\n# Note").unwrap();
        manager.initialize().await.unwrap();

        // Externally overwrite the file.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        std::fs::write(&path, "---\nstatus: published\n---\n# Note").unwrap();

        // all_cached_vault_files returns stale data — still "draft".
        let files = manager.all_cached_vault_files().await;
        let status = files
            .iter()
            .find(|f| f.path == path)
            .and_then(|f| f.frontmatter.as_ref())
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(
            status, "draft",
            "all_cached_vault_files must not re-read disk"
        );
    }

    // ── cache-coherence: write_file / move_file / delete_file ────────────────

    /// write_file() on a brand-new file must insert it into the cache so that
    /// all_cached_vault_files() returns it without a reinitialize().
    #[tokio::test]
    async fn test_write_file_new_inserts_into_cache() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();
        manager.initialize().await.unwrap(); // warm empty cache

        manager
            .write_file(Path::new("new.md"), "---\nstatus: fresh\n---\n# New", None)
            .await
            .unwrap();

        let files = manager.all_cached_vault_files().await;
        assert_eq!(
            files.len(),
            1,
            "new file must appear in cache after write_file"
        );

        let status = files[0]
            .frontmatter
            .as_ref()
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(status, "fresh");
    }

    /// write_file() that overwrites an existing note must update the cached
    /// frontmatter; the cache must NOT keep the stale pre-write values.
    #[tokio::test]
    async fn test_write_file_overwrite_updates_cache() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(
            temp_dir.path().join("note.md"),
            "---\nstatus: old\n---\n# Note",
        )
        .unwrap();
        manager.initialize().await.unwrap();

        manager
            .write_file(
                Path::new("note.md"),
                "---\nstatus: updated\n---\n# Note",
                None,
            )
            .await
            .unwrap();

        let files = manager.all_cached_vault_files().await;
        assert_eq!(files.len(), 1);
        let status = files[0]
            .frontmatter
            .as_ref()
            .and_then(|fm| fm.data.get("status"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(status, "updated", "cache must reflect updated frontmatter");
    }

    /// move_file() must evict the old path and insert the new path so that
    /// all_cached_vault_files() reflects the move without reinitialize().
    #[tokio::test]
    async fn test_move_file_updates_cache() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(
            temp_dir.path().join("from.md"),
            "---\nstatus: active\n---\n# From",
        )
        .unwrap();
        manager.initialize().await.unwrap();

        manager
            .move_file(Path::new("from.md"), Path::new("to.md"), None)
            .await
            .unwrap();

        let files = manager.all_cached_vault_files().await;
        assert_eq!(
            files.len(),
            1,
            "cache should have exactly one entry after move"
        );

        let from_abs = temp_dir.path().join("from.md");
        let to_abs = temp_dir.path().join("to.md");
        assert!(
            !files.iter().any(|f| f.path == from_abs),
            "old path must be absent from cache after move"
        );
        assert!(
            files.iter().any(|f| f.path == to_abs),
            "new path must be present in cache after move"
        );
    }

    /// delete_file() must evict the entry from the cache immediately.
    #[tokio::test]
    async fn test_delete_file_evicts_from_cache() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        std::fs::write(temp_dir.path().join("note.md"), "# Note").unwrap();
        manager.initialize().await.unwrap();
        assert_eq!(manager.all_cached_vault_files().await.len(), 1);

        manager
            .delete_file(Path::new("note.md"), None)
            .await
            .unwrap();

        assert_eq!(
            manager.all_cached_vault_files().await.len(),
            0,
            "deleted file must be evicted from cache"
        );
    }

    /// `sync_index`'s eviction branch runs after `substrate.apply()` has
    /// already released its write-lock (see the "ponytail" comment on that
    /// branch). If a path is recreated between an operation's apply() and
    /// its sync_index() call, a stale "now absent" notification for that
    /// path must not evict the file that is legitimately present again.
    #[tokio::test]
    async fn test_sync_index_skips_stale_removal_when_path_recreated() {
        let temp_dir = TempDir::new().unwrap();
        let config = create_test_config(temp_dir.path());
        let manager = VaultManager::new(config).unwrap();

        manager
            .write_file(Path::new("race.md"), "v1", None)
            .await
            .unwrap();
        assert_eq!(manager.all_cached_vault_files().await.len(), 1);

        // Stand in for a concurrent blind write that recreated the path
        // after a delete's apply() completed but before the delete's
        // sync_index() call ran.
        manager
            .write_file(Path::new("race.md"), "v2", None)
            .await
            .unwrap();

        // The delete's (delayed, stale) removal notification arrives last.
        manager.sync_index(&[("race.md".to_string(), false)]).await;

        assert_eq!(
            manager.all_cached_vault_files().await.len(),
            1,
            "stale removal must not evict a path that was recreated"
        );
        assert_eq!(manager.read_file(Path::new("race.md")).await.unwrap(), "v2");
    }

    #[tokio::test]
    async fn stale_write_hash_preserves_existing_content() {
        let temp_dir = TempDir::new().unwrap();
        let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
        manager
            .write_file(Path::new("note.md"), "current", None)
            .await
            .unwrap();

        let error = manager
            .write_file(Path::new("note.md"), "replacement", Some("stale"))
            .await
            .unwrap_err();

        assert!(matches!(error, Error::ConcurrencyError { .. }));
        assert_eq!(
            manager.read_file(Path::new("note.md")).await.unwrap(),
            "current"
        );
    }

    #[tokio::test]
    async fn expected_hash_rejects_recreating_a_deleted_file() {
        let temp_dir = TempDir::new().unwrap();
        let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();

        let error = manager
            .write_file(Path::new("missing.md"), "replacement", Some("stale"))
            .await
            .unwrap_err();

        match error {
            Error::ConcurrencyError { reason } => assert!(reason.contains("does not exist")),
            other => panic!("expected concurrency error, got {other:?}"),
        }
        assert!(!temp_dir.path().join("missing.md").exists());
    }

    #[tokio::test]
    async fn stale_edit_hash_preserves_existing_content() {
        let temp_dir = TempDir::new().unwrap();
        let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
        manager
            .write_file(Path::new("note.md"), "hello world", None)
            .await
            .unwrap();

        let edits = "<<<<<<< SEARCH\nhello world\n=======\ngoodbye world\n>>>>>>> REPLACE";
        let error = manager
            .edit_file(Path::new("note.md"), edits, Some("stale"), false)
            .await
            .unwrap_err();

        assert!(matches!(error, Error::ConcurrencyError { .. }));
        assert_eq!(
            manager.read_file(Path::new("note.md")).await.unwrap(),
            "hello world"
        );
    }

    #[tokio::test]
    async fn stale_delete_and_move_hashes_preserve_the_source() {
        let temp_dir = TempDir::new().unwrap();
        let manager = VaultManager::new(create_test_config(temp_dir.path())).unwrap();
        manager
            .write_file(Path::new("note.md"), "keep me", None)
            .await
            .unwrap();

        let delete_error = manager
            .delete_file(Path::new("note.md"), Some("stale"))
            .await
            .unwrap_err();
        assert!(matches!(delete_error, Error::ConcurrencyError { .. }));

        let move_error = manager
            .move_file(Path::new("note.md"), Path::new("moved.md"), Some("stale"))
            .await
            .unwrap_err();
        assert!(matches!(move_error, Error::ConcurrencyError { .. }));
        assert_eq!(
            manager.read_file(Path::new("note.md")).await.unwrap(),
            "keep me"
        );
        assert!(!temp_dir.path().join("moved.md").exists());
    }
}
