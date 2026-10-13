//! Full-text search engine powered by tantivy
//!
//! Provides production-grade full-text search with:
//! - Apache Lucene-inspired indexing and searching
//! - TF-IDF relevance scoring
//! - Field-specific search (content, title, tags)
//! - Fuzzy/approximate queries via regex
//! - Fast searching even on large vaults
//!
//! The index is built from the vault manager's parsed note cache and kept in
//! step with it by [`SearchEngine::apply_changes`]. [`SearchEngine::for_vault`]
//! persists it under TurboVault's cache directory (never inside the vault),
//! with each note's content hash, so a new process re-indexes only the notes
//! whose content changed since the index was last written.

use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, TantivyDocument, doc};
use tracing::instrument;
use turbovault_core::prelude::*;
use turbovault_parser::to_plain_text;
use turbovault_vault::VaultManager;

/// Search result metadata for LLM consumption
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct SearchResultInfo {
    /// File path relative to vault root
    pub path: String,
    /// File title (from frontmatter or first heading)
    pub title: String,
    /// Content preview (first 200 chars of plain text)
    pub preview: String,
    /// Relevance score (0.0 to 1.0, normalized from tantivy's TF-IDF)
    pub score: f64,
    /// Matching snippet with context (plain text)
    pub snippet: String,
    /// Front matter tags
    pub tags: Vec<String>,
    /// Files this note links to
    pub outgoing_links: Vec<String>,
    /// Number of backlinks to this note
    pub backlink_count: usize,
    /// Word count of readable content (excludes markdown syntax)
    pub word_count: usize,
    /// Character count of readable content (excludes markdown syntax)
    pub char_count: usize,
}

/// Search filter options
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    /// Only match specific tags
    pub tags: Option<Vec<String>>,
    /// Only match specific frontmatter keys
    pub frontmatter_filters: Option<Vec<(String, String)>>,
    /// Only match notes linked by these paths
    pub backlinks_from: Option<Vec<String>>,
    /// Exclude specific paths
    pub exclude_paths: Option<Vec<String>>,
}

/// Advanced search builder for LLMs
pub struct SearchQuery {
    query: String,
    filter: SearchFilter,
    limit: usize,
}

impl SearchQuery {
    /// Create new search query
    pub fn new(query: impl Into<String>) -> Self {
        Self {
            query: query.into(),
            filter: SearchFilter::default(),
            limit: 10,
        }
    }

    /// Add tag filter
    pub fn with_tags(mut self, tags: Vec<String>) -> Self {
        self.filter.tags = Some(tags);
        self
    }

    /// Add frontmatter filter (e.g., "type", "project")
    pub fn with_frontmatter(mut self, key: String, value: String) -> Self {
        self.filter
            .frontmatter_filters
            .get_or_insert_with(Vec::new)
            .push((key, value));
        self
    }

    /// Filter by backlinks from specific notes
    pub fn with_backlinks_from(mut self, paths: Vec<String>) -> Self {
        self.filter.backlinks_from = Some(paths);
        self
    }

    /// Exclude certain paths from results
    pub fn exclude(mut self, paths: Vec<String>) -> Self {
        self.filter.exclude_paths = Some(paths);
        self
    }

    /// Set result limit
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }

    /// Get the query parameters
    pub fn build(self) -> (String, SearchFilter, usize) {
        (self.query, self.filter, self.limit)
    }
}

/// Search engine for vault discovery (powered by tantivy)
pub struct SearchEngine {
    pub manager: Arc<VaultManager>,
    index: Index,
    /// One reader for the engine's life, reloaded after every commit, rather
    /// than one opened per query.
    reader: IndexReader,
    /// Tantivy allows one writer per index, so the engine keeps the one it
    /// built with and every change report goes through it in turn.
    writer: tokio::sync::Mutex<IndexWriter>,
    // Pre-resolved field handles — avoids runtime schema lookup and unwrap on every search
    field_path: Field,
    field_title: Field,
    field_content: Field,
    field_tags: Field,
    /// The indexed note's content hash, so a persisted index can tell which
    /// notes changed while no process was running.
    field_checksum: Field,
}

/// Bumped whenever what goes into a document changes (fields, title rules,
/// plain-text conversion), so a persisted index from an older layout is
/// rebuilt rather than trusted.
const SEARCH_INDEX_VERSION: u32 = 1;

/// The file in a persisted index directory saying what it indexes.
const INDEX_MARKER: &str = "turbovault-index.json";

/// Set to `memory` to keep every vault's search index in RAM only, built
/// afresh by each process, instead of persisting it under the cache
/// directory.
pub const SEARCH_INDEX_ENV: &str = "TURBOVAULT_SEARCH_INDEX";

#[derive(Serialize, Deserialize, PartialEq)]
struct IndexMarker {
    version: u32,
    turbovault: String,
    vault: PathBuf,
}

fn schema() -> Schema {
    let mut schema_builder = Schema::builder();
    // turbovault-2ag: `path` is a raw STRING (not tokenized TEXT) so that
    // `apply_changes`' `delete_term(Term::from_field_text(path, rel))`
    // matches the exact path and removes the prior doc on edit. A TEXT
    // field tokenizes "a/b.md" into terms, so delete_term never matched
    // and every edit left a stale duplicate. The query parser only
    // searches title/content/tags, so path is never full-text queried.
    schema_builder.add_text_field("path", STRING | STORED);
    schema_builder.add_text_field("title", TEXT | STORED);
    schema_builder.add_text_field("content", TEXT);
    schema_builder.add_text_field("tags", TEXT | STORED);
    schema_builder.add_text_field("checksum", STRING | STORED);
    schema_builder.build()
}

/// What indexing needs of one note, worked out off any lock.
struct NoteDocument {
    rel_path: String,
    title: String,
    content: String,
    tags: String,
    checksum: String,
}

impl NoteDocument {
    fn new(rel_path: String, vault_file: &VaultFile) -> Self {
        let title = vault_file
            .frontmatter
            .as_ref()
            .and_then(|fm| fm.data.get("title"))
            .and_then(|v| v.as_str())
            .unwrap_or_else(|| {
                std::path::Path::new(&rel_path)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
            })
            .to_string();
        let tags = vault_file
            .frontmatter
            .as_ref()
            .map(|fm| fm.tags().join(" "))
            .unwrap_or_default();
        Self {
            title,
            // Plain text, so markdown syntax and URLs are not indexed as words.
            content: to_plain_text(&vault_file.content),
            tags,
            checksum: vault_file.metadata.checksum.clone(),
            rel_path,
        }
    }
}

impl SearchEngine {
    /// Create new search engine and index all vault files, in memory.
    pub async fn new(manager: Arc<VaultManager>) -> Result<Self> {
        let index = Index::create_in_ram(schema());
        let engine = Self::with_index(manager, index)?;
        engine.reindex_all().await?;
        Ok(engine)
    }

    /// The search engine for `manager`'s vault, its index persisted in the
    /// cache directory so a restart re-indexes only the notes that changed
    /// meanwhile. Falls back to an in-memory index when that directory can't
    /// be used (including when another process is writing the same vault's
    /// index), or when [`SEARCH_INDEX_ENV`] is `memory`.
    pub async fn for_vault(manager: Arc<VaultManager>) -> Result<Self> {
        if std::env::var(SEARCH_INDEX_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("memory")) {
            return Self::new(manager).await;
        }
        match default_index_dir(manager.vault_path()) {
            Some(dir) => match Self::open(Arc::clone(&manager), &dir).await {
                Ok(engine) => return Ok(engine),
                Err(e) => log::info!(
                    "search index at {} not usable ({e}); indexing in memory",
                    dir.display()
                ),
            },
            None => log::info!("no cache directory for the search index; indexing in memory"),
        }
        Self::new(manager).await
    }

    /// Open the index persisted in `dir`, or create it there, and bring it up
    /// to date with the vault: notes whose content changed since it was last
    /// written are re-indexed, new ones added, and gone ones removed.
    pub async fn open(manager: Arc<VaultManager>, dir: &std::path::Path) -> Result<Self> {
        Self::open_counting(manager, dir)
            .await
            .map(|(engine, _)| engine)
    }

    /// [`Self::open`], with how many notes it had to re-index.
    async fn open_counting(
        manager: Arc<VaultManager>,
        dir: &std::path::Path,
    ) -> Result<(Self, usize)> {
        let marker = IndexMarker {
            version: SEARCH_INDEX_VERSION,
            turbovault: env!("CARGO_PKG_VERSION").to_string(),
            vault: manager.vault_path().clone(),
        };
        let index = {
            let dir = dir.to_path_buf();
            tokio::task::spawn_blocking(move || open_index_dir(&dir, &marker))
                .await
                .map_err(|e| Error::other(format!("search index task failed: {e}")))??
        };
        let engine = Self::with_index(manager, index)?;
        let reindexed = engine.reconcile().await?;
        Ok((engine, reindexed))
    }

    fn with_index(manager: Arc<VaultManager>, index: Index) -> Result<Self> {
        let schema = index.schema();
        let field = |name: &str| {
            schema
                .get_field(name)
                .map_err(|e| Error::config_error(format!("search index has no {name} field: {e}")))
        };
        let (field_path, field_title, field_content, field_tags, field_checksum) = (
            field("path")?,
            field("title")?,
            field("content")?,
            field("tags")?,
            field("checksum")?,
        );
        let writer = index
            .writer(50_000_000)
            .map_err(|e| Error::config_error(format!("Failed to create index writer: {}", e)))?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| Error::config_error(format!("Failed to create reader: {}", e)))?;
        Ok(Self {
            manager,
            index,
            reader,
            writer: tokio::sync::Mutex::new(writer),
            field_path,
            field_title,
            field_content,
            field_tags,
            field_checksum,
        })
    }

    /// Every note in the cache, as indexing needs it, built in parallel.
    async fn note_documents(&self) -> Result<Vec<NoteDocument>> {
        let notes = self.manager.notes_validated().await;
        let notes: Vec<(String, Arc<VaultFile>)> = notes
            .into_iter()
            .filter(|file| turbovault_core::is_note(&file.path))
            // turbovault-2ag: index docs by VAULT-RELATIVE path so the
            // field_path key matches `apply_changes` (which uses the
            // git-diff relative path). Keying the initial build by the
            // absolute path made `apply_changes`'s relative `delete_term`
            // miss, leaving a stale duplicate doc on every edit.
            .map(|file| (self.manager.relative_path(&file.path), file))
            .collect();
        tokio::task::spawn_blocking(move || {
            notes
                .into_par_iter()
                .map(|(rel_path, file)| NoteDocument::new(rel_path, &file))
                .collect()
        })
        .await
        .map_err(|e| Error::other(format!("search index task failed: {e}")))
    }

    fn document(&self, note: NoteDocument) -> TantivyDocument {
        doc!(
            self.field_path => note.rel_path,
            self.field_title => note.title,
            self.field_content => note.content,
            self.field_tags => note.tags,
            self.field_checksum => note.checksum,
        )
    }

    /// Commit the writer and make what it wrote visible to searches.
    fn commit(&self, writer: &mut IndexWriter) -> Result<()> {
        writer
            .commit()
            .map_err(|e| Error::config_error(format!("Failed to commit index: {}", e)))?;
        self.reader
            .reload()
            .map_err(|e| Error::config_error(format!("Failed to reload index reader: {e}")))
    }

    /// Index every note, replacing whatever the index held.
    async fn reindex_all(&self) -> Result<()> {
        let notes = self.note_documents().await?;
        let mut writer = self.writer.lock().await;
        writer
            .delete_all_documents()
            .map_err(|e| Error::config_error(format!("Failed to clear index: {e}")))?;
        for note in notes {
            writer
                .add_document(self.document(note))
                .map_err(|e| Error::config_error(format!("Failed to index: {e}")))?;
        }
        self.commit(&mut writer)
    }

    /// Bring a persisted index into agreement with the vault, touching only
    /// the notes whose content hash differs from what was indexed. Returns
    /// how many notes it re-indexed or removed.
    async fn reconcile(&self) -> Result<usize> {
        let indexed = self.indexed_checksums()?;
        let notes = self.note_documents().await?;
        let mut writer = self.writer.lock().await;
        let mut remaining = indexed;
        let mut changed = 0usize;
        for note in notes {
            match remaining.remove(&note.rel_path) {
                Some(checksum) if checksum == note.checksum => {}
                _ => {
                    writer.delete_term(tantivy::Term::from_field_text(
                        self.field_path,
                        &note.rel_path,
                    ));
                    writer
                        .add_document(self.document(note))
                        .map_err(|e| Error::config_error(format!("Failed to index: {e}")))?;
                    changed += 1;
                }
            }
        }
        for gone in remaining.keys() {
            writer.delete_term(tantivy::Term::from_field_text(self.field_path, gone));
            changed += 1;
        }
        log::debug!("search index: {changed} note(s) re-indexed since it was last written");
        self.commit(&mut writer)?;
        Ok(changed)
    }

    /// Each indexed path with the content hash it was indexed at.
    fn indexed_checksums(&self) -> Result<HashMap<String, String>> {
        let searcher = self.reader.searcher();
        let mut out = HashMap::new();
        for segment in searcher.segment_readers() {
            let store = segment
                .get_store_reader(1)
                .map_err(|e| Error::config_error(format!("Failed to read index store: {e}")))?;
            for document in store.iter::<TantivyDocument>(segment.alive_bitset()) {
                let document = document
                    .map_err(|e| Error::config_error(format!("Failed to read index store: {e}")))?;
                let text = |field| {
                    document
                        .get_first(field)
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string()
                };
                out.insert(text(self.field_path), text(self.field_checksum));
            }
        }
        Ok(out)
    }

    /// The document for the note at `rel_path` as the note cache holds it
    /// now, or `None` when there is no note there to index: it isn't
    /// markdown, or it is gone (or out of the scan's reach, so not cached).
    async fn document_for(&self, rel_path: &str) -> Option<NoteDocument> {
        // tlx.7: markdown only, the same for a cold build as for a change
        // report, or a path indexed by one vanishes on the other.
        if !turbovault_core::is_note(std::path::Path::new(rel_path)) {
            return None;
        }
        let full_path = self.manager.vault_path().join(rel_path);
        let vault_file = self.manager.cached_note(&full_path).await?;
        Some(NoteDocument::new(rel_path.to_string(), &vault_file))
    }

    /// GWS.14c — apply an incremental change set to the tantivy index.
    ///
    /// Each reported path is re-indexed as the note cache holds it now (the
    /// manager updates the cache before it reports): its old doc is deleted
    /// (a no-op if it had none) and, if the cache has a note there, the new
    /// one added. The `present` half of a report is not trusted over the
    /// cache, which makes a report idempotent and order-independent: one held
    /// back while a cold build ran can be applied after a newer one without
    /// undoing it. Unreadable files are logged and skipped per path, so one
    /// malformed file does not brick the whole drain pass. All changes commit
    /// in one writer changeset, and concurrent calls take turns on the
    /// engine's single writer.
    ///
    /// Replaces the pre-GWS.14c pattern where the server evicted the
    /// cached engine on flush and the next query paid cold-rebuild cost.
    #[instrument(
        skip(self, changes),
        fields(n_changes = changes.len()),
        name = "search_apply_changes"
    )]
    pub async fn apply_changes(&self, changes: Vec<(String, bool)>) -> Result<()> {
        if changes.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer.lock().await;
        for (rel_path, _present) in changes {
            let term = tantivy::Term::from_field_text(self.field_path, &rel_path);
            writer.delete_term(term);
            if let Some(note) = self.document_for(&rel_path).await {
                writer.add_document(self.document(note)).map_err(|e| {
                    Error::config_error(format!("apply_changes: index {rel_path}: {e}"))
                })?;
            }
        }
        self.commit(&mut writer)
    }

    /// Simple keyword search
    #[instrument(skip(self), fields(query = query), name = "search_query")]
    pub async fn search(&self, query: &str) -> Result<Vec<SearchResultInfo>> {
        SearchQuery::new(query).limit(10).build_results(self).await
    }

    /// Advanced search with filters and options
    #[instrument(skip(self, query), name = "search_advanced")]
    pub async fn advanced_search(&self, query: SearchQuery) -> Result<Vec<SearchResultInfo>> {
        query.build_results(self).await
    }

    /// Search by tag
    pub async fn search_by_tags(&self, tags: Vec<String>) -> Result<Vec<SearchResultInfo>> {
        SearchQuery::new("*")
            .with_tags(tags)
            .limit(100)
            .build_results(self)
            .await
    }

    /// Search by frontmatter property
    pub async fn search_by_frontmatter(
        &self,
        key: &str,
        value: &str,
    ) -> Result<Vec<SearchResultInfo>> {
        SearchQuery::new("*")
            .with_frontmatter(key.to_string(), value.to_string())
            .limit(100)
            .build_results(self)
            .await
    }

    /// Find related notes (by link proximity + content similarity)
    #[instrument(skip(self), fields(path = path, limit = limit), name = "search_find_related")]
    pub async fn find_related(&self, path: &str, limit: usize) -> Result<Vec<SearchResultInfo>> {
        // The note's parsed form, from the cache when it's there
        let vault_file = self.note(path).await?;

        // Extract key terms from plain text content (excludes URLs, markdown syntax)
        let plain_content = to_plain_text(&vault_file.content);
        let keywords = extract_keywords(&plain_content);

        // Search for similar notes using tantivy query
        let query = keywords.join(" ");
        let mut results = SearchQuery::new(query)
            .exclude(vec![path.to_string()])
            .limit(limit)
            .build_results(self)
            .await?;

        // Sort by relevance (tantivy already scores, but ensure descending)
        results.sort_by(|a, b| b.score.total_cmp(&a.score));

        Ok(results)
    }

    /// Semantic search recommendations for LLMs
    pub async fn recommend_related(&self, path: &str) -> Result<Vec<SearchResultInfo>> {
        self.find_related(path, 5).await
    }

    /// The parsed note at vault-relative `path`: the cached copy, or parsed
    /// from disk when the cache has none (and erroring as that read does).
    async fn note(&self, path: &str) -> Result<Arc<VaultFile>> {
        let full_path = self.manager.vault_path().join(path);
        match self.manager.cached_note(&full_path).await {
            Some(file) => Ok(file),
            None => Ok(Arc::new(
                self.manager.parse_file(&PathBuf::from(path)).await?,
            )),
        }
    }
}

/// Where `vault`'s search index is persisted: a directory under the cache
/// root named for the vault's path. Index directories whose vault no longer
/// exists are removed on the way.
pub fn default_index_dir(vault: &std::path::Path) -> Option<PathBuf> {
    let root = turbovault_core::cache::VaultCache::cache_root()
        .ok()?
        .join("search");
    let key = turbovault_core::compute_hash(&vault.to_string_lossy());
    let dir = root.join(&key[..16]);
    remove_orphaned_indexes(&root, &dir);
    Some(dir)
}

/// Remove index directories under `root` whose vault is gone, sparing
/// `keep`. Best effort: anything unreadable is left alone.
fn remove_orphaned_indexes(root: &std::path::Path, keep: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if dir == keep {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(dir.join(INDEX_MARKER)) else {
            continue;
        };
        if let Ok(marker) = serde_json::from_str::<IndexMarker>(&text)
            && !marker.vault.exists()
        {
            let _ = std::fs::remove_dir_all(&dir);
        }
    }
}

/// Open the index in `dir` if it was written for `marker` (this vault, this
/// layout, this TurboVault), or start a new one there.
fn open_index_dir(dir: &std::path::Path, marker: &IndexMarker) -> Result<Index> {
    let io = |e: std::io::Error| Error::io_at(dir.display().to_string(), e);
    let marker_path = dir.join(INDEX_MARKER);
    let current = std::fs::read_to_string(&marker_path)
        .ok()
        .and_then(|text| serde_json::from_str::<IndexMarker>(&text).ok())
        .is_some_and(|found| &found == marker);
    if current
        && let Ok(index) = Index::open_in_dir(dir)
        && index.schema() == schema()
    {
        return Ok(index);
    }

    // Anything else in the directory is an older or foreign index.
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(io)?;
    }
    create_private_dir(dir).map_err(io)?;
    let index = Index::create_in_dir(dir, schema())
        .map_err(|e| Error::config_error(format!("Failed to create search index: {e}")))?;
    let text = serde_json::to_string(marker).map_err(|e| Error::other(e.to_string()))?;
    std::fs::write(&marker_path, text).map_err(io)?;
    Ok(index)
}

/// Create `dir` and its parents, readable by this user only: the index
/// holds the vault's text.
fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

impl SearchQuery {
    /// Build and execute search results using tantivy
    async fn build_results(self, engine: &SearchEngine) -> Result<Vec<SearchResultInfo>> {
        let (query_str, filter, limit) = self.build();

        // Gate first: whatever the gate finds changed reaches the index
        // before the searcher below is taken from it.
        let graph = engine.manager.link_graph_flushed().await;
        let searcher = engine.reader.searcher();

        // Parse query using tantivy's QueryParser with fuzzy search enabled
        let mut query_parser = QueryParser::for_index(
            &engine.index,
            vec![engine.field_title, engine.field_content, engine.field_tags],
        );

        // Enable fuzzy search with Levenshtein distance of 1 for typo tolerance
        // This makes searches forgiving of single-character mistakes
        query_parser.set_field_fuzzy(
            engine.field_title,
            true,  // enable_fuzzy
            1,     // distance (1-2 char typos)
            false, // prefix_only
        );
        query_parser.set_field_fuzzy(engine.field_content, true, 1, false);
        query_parser.set_field_fuzzy(engine.field_tags, true, 1, false);

        let query = query_parser
            .parse_query(&query_str)
            .map_err(|e| Error::validation_error(format!("Failed to parse query: {}", e)))?;

        // Execute search
        let top_docs = searcher
            .search(
                &query,
                &TopDocs::with_limit(limit * 2).order_by_score(), // Get extra docs for filtering
            )
            .map_err(|e| Error::config_error(format!("Search failed: {}", e)))?;

        let mut results = Vec::new();

        for (score, doc_address) in top_docs {
            // Retrieve the stored document from the index
            let tantivy_doc: TantivyDocument = searcher
                .doc(doc_address)
                .map_err(|e| Error::config_error(format!("Failed to retrieve doc: {}", e)))?;

            // Extract field values directly — avoids JSON serialize/deserialize round-trip
            let path = tantivy_doc
                .get_first(engine.field_path)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let title = tantivy_doc
                .get_first(engine.field_title)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let tags_str = tantivy_doc
                .get_first(engine.field_tags)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let file_tags: Vec<String> =
                tags_str.split_whitespace().map(|s| s.to_string()).collect();

            // Apply filter filters
            if let Some(tags) = &filter.tags
                && !file_tags.iter().any(|t| tags.contains(t))
            {
                continue;
            }

            // Apply exclusion filter
            if let Some(exclude) = &filter.exclude_paths
                && exclude.iter().any(|p| path.ends_with(p))
            {
                continue;
            }

            // The note as parsed, from the cache; a hit whose note can't be
            // had is skipped.
            let Ok(vault_file) = engine.note(&path).await else {
                continue;
            };

            // Apply frontmatter filters
            if let Some(fm_filters) = &filter.frontmatter_filters {
                let mut matches_all = true;
                if let Some(fm) = &vault_file.frontmatter {
                    for (key, value) in fm_filters {
                        if let Some(fm_value) = fm.data.get(key) {
                            let fm_str = fm_value.to_string();
                            if !fm_str.contains(value) {
                                matches_all = false;
                                break;
                            }
                        } else {
                            matches_all = false;
                            break;
                        }
                    }
                } else {
                    matches_all = false;
                }
                if !matches_all {
                    continue;
                }
            }

            // Extract plain text for preview, snippet, and metrics
            let plain_content = to_plain_text(&vault_file.content);

            // Generate preview from plain text (first line, up to 200 chars)
            let preview = plain_content
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(200)
                .collect::<String>();

            // Extract snippet from plain text (no markdown syntax in results)
            let snippet = extract_snippet(&plain_content, &query_str);

            // Calculate content metrics from plain text
            let word_count = plain_content.split_whitespace().count();
            let char_count = plain_content.chars().count();

            // Get outgoing links
            let outgoing_links: Vec<String> =
                vault_file.links.iter().map(|l| l.target.clone()).collect();

            // Normalize Tantivy's BM25 score to 0.0-1.0 range
            // Typical BM25 scores range 0-10+, so we use sigmoid-like normalization
            let score_f64 = score as f64;
            let normalized_score = (1.0 / (1.0 + (-score_f64 / 2.0).exp())).clamp(0.0, 1.0);

            results.push(SearchResultInfo {
                path,
                title,
                preview,
                score: normalized_score,
                snippet,
                tags: file_tags,
                outgoing_links,
                // Counted below, under one short hold of the graph.
                backlink_count: 0,
                word_count,
                char_count,
            });

            if results.len() >= limit {
                break;
            }
        }

        // The graph is keyed by absolute path; counting by the vault-relative
        // path found nothing, so every result said it had no backlinks.
        let graph_read = graph.read().await;
        for result in &mut results {
            let full_path = engine.manager.vault_path().join(&result.path);
            result.backlink_count = graph_read.backlink_source_count(&full_path);
        }

        Ok(results)
    }
}

/// Extract keywords from content for recommendations
fn extract_keywords(content: &str) -> Vec<String> {
    content
        .split_whitespace()
        .filter(|word| word.len() > 3)
        .filter(|word| !is_stopword(word))
        .map(|w| w.to_lowercase())
        .take(10)
        .collect()
}

/// Check if word is a common stopword
pub(crate) fn is_stopword(word: &str) -> bool {
    matches!(
        word.to_lowercase().as_str(),
        "the"
            | "a"
            | "an"
            | "and"
            | "or"
            | "but"
            | "in"
            | "on"
            | "at"
            | "to"
            | "for"
            | "of"
            | "with"
            | "from"
            | "by"
            | "about"
            | "is"
            | "are"
            | "was"
            | "were"
            | "be"
            | "been"
            | "being"
            | "have"
            | "has"
            | "had"
            | "do"
            | "does"
            | "did"
            | "will"
            | "would"
            | "could"
            | "should"
            | "may"
            | "might"
            | "must"
            | "can"
    )
}

/// Extract snippet from content around matching terms
/// `text` lower-cased character by character, with the byte offset in `text`
/// that each byte of the result came from.
fn lower_with_origins(text: &str) -> (String, Vec<usize>) {
    let mut lower = String::with_capacity(text.len());
    let mut origins = Vec::with_capacity(text.len());
    for (offset, ch) in text.char_indices() {
        lower.extend(ch.to_lowercase());
        origins.resize(lower.len(), offset);
    }
    (lower, origins)
}

fn extract_snippet(content: &str, query: &str) -> String {
    if query.is_empty() || query == "*" {
        return content.lines().take(1).collect();
    }

    let (query_lower, _) = lower_with_origins(query);
    let (content_lower, origins) = lower_with_origins(content);

    if let Some(found) = content_lower.find(&query_lower) {
        // Where the match sits in `content` itself, which a character that
        // changes length when lowered puts somewhere else.
        let pos = origins[found];
        let match_end = origins
            .get(found + query_lower.len())
            .copied()
            .unwrap_or(content.len());

        let mut start = pos.saturating_sub(50);
        while start > 0 && !content.is_char_boundary(start) {
            start -= 1;
        }

        let mut end = (match_end + 50).min(content.len());
        while end < content.len() && !content.is_char_boundary(end) {
            end += 1;
        }

        let snippet = &content[start..end];
        format!("...{}...", snippet.trim())
    } else {
        content.lines().take(1).next().unwrap_or("").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_keywords() {
        let content = "The quick brown fox jumps over the lazy dog";
        let keywords = extract_keywords(content);
        assert!(!keywords.is_empty());
        assert!(keywords.iter().any(|k| k == "quick" || k == "brown"));
    }

    #[test]
    fn test_is_stopword() {
        assert!(is_stopword("the"));
        assert!(is_stopword("and"));
        assert!(!is_stopword("rust"));
    }

    #[test]
    fn test_extract_snippet() {
        let content = "The quick brown fox jumps over the lazy dog";
        let snippet = extract_snippet(content, "fox");
        assert!(snippet.contains("fox"));
    }

    /// Lower-casing can change a character's length (`İ` is two bytes, its
    /// lower case three), so a match found in a lower-cased copy has to be
    /// mapped back before it can cut the original.
    #[test]
    fn test_extract_snippet_after_characters_that_grow_when_lowered() {
        let content = format!("{} harbor {}", "İ".repeat(60), "x".repeat(80));
        let snippet = extract_snippet(&content, "HARBOR");
        assert!(snippet.contains("harbor"), "{snippet}");
    }

    #[test]
    fn test_extract_snippet_no_match() {
        let content = "The quick brown fox";
        let snippet = extract_snippet(content, "xyz");
        assert!(!snippet.contains("xyz"));
    }

    #[test]
    fn test_extract_snippet_wildcard() {
        let content = "First line\nSecond line";
        let snippet = extract_snippet(content, "*");
        assert!(snippet.contains("First"));
    }

    #[test]
    fn test_extract_keywords_filters_short_words() {
        let content = "a b c defgh ijklmn";
        let keywords = extract_keywords(content);
        assert!(!keywords.iter().any(|k| k.len() <= 3));
    }

    // ==================== INTEGRATION TESTS ====================
    // These tests verify the search engine works end-to-end

    /// Test: File path extension checking works correctly
    #[test]
    fn test_file_path_extension_check() {
        let paths = vec![
            "/vault/index.md",
            "/vault/test.MD",
            "/vault/readme.txt",
            "/vault/file.md.bak",
            "relative/path/note.md",
        ];

        for path_str in paths {
            let ends_with_md = path_str.to_lowercase().ends_with(".md");
            eprintln!("[TEST] Path: {}, ends_with .md: {}", path_str, ends_with_md);
        }

        // Verify the logic
        assert!("/vault/index.md".ends_with(".md"));
        assert!("/vault/test.md".ends_with(".md"));
        assert!(!"/vault/readme.txt".ends_with(".md"));
        assert!(!"/vault/file.md.bak".ends_with(".md"));
        assert!("relative/path/note.md".ends_with(".md"));
    }

    /// turbovault-2ag: an incremental reindex of an edited file must REPLACE
    /// its doc, not leave a stale duplicate. Regression for two bugs: the
    /// initial build keyed docs by absolute path while `apply_changes` keyed
    /// by vault-relative path, and the `path` field was tokenized TEXT so
    /// `delete_term` never matched. Either alone leaves the old doc behind.
    #[tokio::test]
    async fn apply_changes_replaces_edited_doc_without_duplicate() {
        use tempfile::TempDir;
        use turbovault_core::config::{ServerConfig, VaultConfig};

        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("s.md"), "# S\n\nalphaword content here\n").unwrap();

        let mut cfg = ServerConfig::new();
        cfg.vaults
            .push(VaultConfig::builder("v", tmp.path()).build().unwrap());
        let manager = Arc::new(VaultManager::new(cfg).unwrap());

        let engine = SearchEngine::new(Arc::clone(&manager)).await.unwrap();
        assert_eq!(
            engine.search("alphaword").await.unwrap().len(),
            1,
            "alphaword indexed by the initial build"
        );

        // Edit the file on disk, let the manager see it as its sweep would,
        // then incrementally reindex just that path.
        std::fs::write(tmp.path().join("s.md"), "# S\n\nbetaword content here\n").unwrap();
        manager.reconcile_now().await;
        engine
            .apply_changes(vec![("s.md".to_string(), true)])
            .await
            .unwrap();

        assert_eq!(
            engine.search("betaword").await.unwrap().len(),
            1,
            "new term findable after incremental reindex"
        );
        assert_eq!(
            engine.search("alphaword").await.unwrap().len(),
            0,
            "old doc replaced, not duplicated"
        );
    }

    async fn engine_over(files: &[(&str, &str)]) -> (tempfile::TempDir, SearchEngine) {
        use turbovault_core::config::{ServerConfig, VaultConfig};

        let tmp = tempfile::TempDir::new().unwrap();
        for (path, content) in files {
            std::fs::write(tmp.path().join(path), content).unwrap();
        }
        let mut cfg = ServerConfig::new();
        cfg.vaults
            .push(VaultConfig::builder("v", tmp.path()).build().unwrap());
        let manager = Arc::new(VaultManager::new(cfg).unwrap());
        let engine = SearchEngine::new(manager).await.unwrap();
        (tmp, engine)
    }

    /// Tantivy allows one writer per index. Two change reports applied at
    /// once (a write and the freshness sweep) must both land, not fail on
    /// the writer lock and cost the server its engine.
    #[tokio::test]
    async fn concurrent_apply_changes_both_land() {
        let (tmp, engine) = engine_over(&[("a.md", "# A\n"), ("b.md", "# B\n")]).await;
        std::fs::write(tmp.path().join("a.md"), "# A\n\ndeltaword\n").unwrap();
        std::fs::write(tmp.path().join("b.md"), "# B\n\nepsilonword\n").unwrap();
        engine.manager.reconcile_now().await;

        let (a, b) = tokio::join!(
            engine.apply_changes(vec![("a.md".to_string(), true)]),
            engine.apply_changes(vec![("b.md".to_string(), true)]),
        );
        a.unwrap();
        b.unwrap();

        assert_eq!(engine.search("deltaword").await.unwrap().len(), 1);
        assert_eq!(engine.search("epsilonword").await.unwrap().len(), 1);
    }

    /// A change report is applied as the vault is now (as the manager, which
    /// sends every report, has recorded it), so two reports for one path give
    /// the same index whichever lands last. A report kept while a cold build
    /// ran can be applied after a newer one without undoing it.
    #[tokio::test]
    async fn apply_changes_follows_the_vault_not_the_report() {
        let (tmp, engine) = engine_over(&[("a.md", "# A\n\nzetaword\n")]).await;

        // Deleted, then created again before the older report was applied.
        engine
            .apply_changes(vec![("a.md".to_string(), true)])
            .await
            .unwrap();
        engine
            .apply_changes(vec![("a.md".to_string(), false)])
            .await
            .unwrap();
        assert_eq!(
            engine.search("zetaword").await.unwrap().len(),
            1,
            "a.md is on disk, so a stale deletion report must not drop it"
        );

        std::fs::remove_file(tmp.path().join("a.md")).unwrap();
        engine.manager.reconcile_now().await;
        engine
            .apply_changes(vec![("a.md".to_string(), true)])
            .await
            .unwrap();
        assert_eq!(engine.search("zetaword").await.unwrap().len(), 0);
    }

    /// tlx.7: apply_changes must mirror SearchEngine::new's markdown-only
    /// filter. A committed non-.md file that parse_file would accept must NOT
    /// be indexed incrementally — otherwise it is searchable until the next
    /// cold rebuild (which only indexes .md) silently drops it.
    #[tokio::test]
    async fn apply_changes_skips_non_markdown_paths() {
        use tempfile::TempDir;
        use turbovault_core::config::{ServerConfig, VaultConfig};

        let tmp = TempDir::new().unwrap();
        // A real, parseable file on disk, so it's the .md guard — not a parse
        // failure — that keeps it out of the index.
        std::fs::write(tmp.path().join("note.txt"), "# T\n\ngammaword content\n").unwrap();

        let mut cfg = ServerConfig::new();
        cfg.vaults
            .push(VaultConfig::builder("v", tmp.path()).build().unwrap());
        let manager = Arc::new(VaultManager::new(cfg).unwrap());

        let engine = SearchEngine::new(Arc::clone(&manager)).await.unwrap();
        engine
            .apply_changes(vec![("note.txt".to_string(), true)])
            .await
            .unwrap();

        assert_eq!(
            engine.search("gammaword").await.unwrap().len(),
            0,
            "non-.md path not indexed incrementally, matching cold-rebuild behavior"
        );
    }

    /// Test: Stopword filtering works for keyword extraction
    #[test]
    fn test_stopword_filtering_comprehensive() {
        let stopwords = vec!["the", "and", "or", "is", "are"];
        let content_words = vec!["testing", "capabilities", "search", "index"];

        for word in stopwords {
            assert!(is_stopword(word), "Should recognize '{}' as stopword", word);
        }

        for word in content_words {
            assert!(
                !is_stopword(word),
                "Should NOT recognize '{}' as stopword",
                word
            );
        }
    }

    /// Test: Snippet extraction handles edge cases
    #[test]
    fn test_snippet_extraction_edge_cases() {
        // Empty content
        let snippet = extract_snippet("", "search");
        assert!(snippet.is_empty() || !snippet.contains("search"));

        // Content shorter than context window
        let short = "short";
        let snippet = extract_snippet(short, "short");
        assert!(snippet.contains("short"));

        // Multiple occurrences - should find first
        let multi = "test test test another test";
        let snippet = extract_snippet(multi, "test");
        assert!(snippet.contains("test"));
    }

    /// Test: Fuzzy search query building (basic)
    #[test]
    fn test_fuzzy_search_query_building() {
        // This test verifies the QueryParser can be created and configured
        use tantivy::schema::*;

        let mut schema_builder = Schema::builder();
        schema_builder.add_text_field("title", TEXT);
        schema_builder.add_text_field("content", TEXT);
        let schema = schema_builder.build();

        // Create query parser
        let mut query_parser = tantivy::query::QueryParser::for_index(
            &tantivy::Index::create_in_ram(schema.clone()),
            vec![schema.get_field("title").unwrap()],
        );

        // Enable fuzzy search
        query_parser.set_field_fuzzy(
            schema.get_field("title").unwrap(),
            true,  // enable
            1,     // distance
            false, // prefix_only
        );

        eprintln!("[TEST] QueryParser configured successfully with fuzzy search");
    }

    /// Test: Score normalization stays in 0.0-1.0 range
    #[test]
    fn test_score_normalization_bounds() {
        let scores: Vec<f64> = vec![-10.0, -1.0, 0.0, 1.0, 5.0, 10.0, 100.0];

        for raw_score in scores {
            let normalized: f64 = (1.0 / (1.0 + (-raw_score / 2.0).exp())).clamp(0.0, 1.0);
            assert!(
                (0.0..=1.0).contains(&normalized),
                "Score {} normalized to {}, should be 0.0-1.0",
                raw_score,
                normalized
            );
            eprintln!("[SCORE] Raw: {}, Normalized: {}", raw_score, normalized);
        }
    }

    /// TEST: Integration - file extension logic in isolation
    #[test]
    fn test_file_filtering_logic() {
        // Test BOTH case-sensitive and case-insensitive approaches
        let test_paths = vec![
            ("index.md", true),
            ("test.MD", true), // should support uppercase too!
            ("README.txt", false),
            (".md", true),
            ("file.md.backup", false),
        ];

        eprintln!("\n[INTEGRATION TEST] File filtering logic (case-insensitive):");
        for (path, should_index) in test_paths {
            let path_str = path.to_string();
            // Use to_lowercase() for case-insensitive comparison (like real code should do)
            let passes_filter = path_str.to_lowercase().ends_with(".md");
            eprintln!(
                "[CHECK] Path: {}, ends_with .md (case-insensitive): {}, expected: {}",
                path, passes_filter, should_index
            );

            if should_index {
                assert!(
                    passes_filter,
                    "Path {} should pass filter (case-insensitive)",
                    path
                );
            } else {
                assert!(
                    !passes_filter,
                    "Path {} should NOT pass filter (case-insensitive)",
                    path
                );
            }
        }
    }

    async fn vault_with(notes: &[(&str, &str)]) -> (tempfile::TempDir, Arc<VaultManager>) {
        let temp = tempfile::TempDir::new().unwrap();
        for (path, content) in notes {
            std::fs::write(temp.path().join(path), content).unwrap();
        }
        let mut config = turbovault_core::ConfigProfile::Development.create_config();
        config.vaults.push(
            turbovault_core::VaultConfig::builder("search", temp.path())
                .build()
                .unwrap(),
        );
        let manager = VaultManager::new(config).unwrap();
        manager.initialize().await.unwrap();
        (temp, Arc::new(manager))
    }

    fn paths(results: &[SearchResultInfo]) -> Vec<String> {
        let mut paths: Vec<String> = results.iter().map(|r| r.path.clone()).collect();
        paths.sort();
        paths
    }

    /// A persisted index picks up what changed while no process held it,
    /// and re-indexes only that.
    #[tokio::test]
    async fn a_reopened_index_reindexes_only_what_changed() {
        let (vault, manager) = vault_with(&[
            ("kept.md", "# Kept\n\nthe walrus stays\n"),
            ("edited.md", "# Edited\n\nthe walrus leaves\n"),
            ("deleted.md", "# Deleted\n\nthe walrus vanishes\n"),
        ])
        .await;
        let index_dir = tempfile::TempDir::new().unwrap();
        let dir = index_dir.path().join("index");

        let (engine, reindexed) = SearchEngine::open_counting(Arc::clone(&manager), &dir)
            .await
            .unwrap();
        assert_eq!(reindexed, 3, "a new index indexes every note");
        assert_eq!(paths(&engine.search("walrus").await.unwrap()).len(), 3);
        drop(engine);

        std::fs::write(
            vault.path().join("edited.md"),
            "# Edited\n\nthe narwhal arrives\n",
        )
        .unwrap();
        std::fs::remove_file(vault.path().join("deleted.md")).unwrap();
        std::fs::write(
            vault.path().join("added.md"),
            "# Added\n\nthe walrus returns\n",
        )
        .unwrap();
        manager.reconcile_now().await;

        let (engine, reindexed) = SearchEngine::open_counting(Arc::clone(&manager), &dir)
            .await
            .unwrap();
        assert_eq!(reindexed, 3, "edited, deleted and added; kept is untouched");
        assert_eq!(
            paths(&engine.search("walrus").await.unwrap()),
            ["added.md", "kept.md"]
        );
        assert_eq!(
            paths(&engine.search("narwhal").await.unwrap()),
            ["edited.md"]
        );
    }

    /// Only one process writes a persisted index; a second is refused, and
    /// `for_vault` falls back to memory rather than failing.
    #[tokio::test]
    async fn a_second_writer_is_refused() {
        let (_vault, manager) = vault_with(&[("a.md", "# A\n\nwalrus\n")]).await;
        let index_dir = tempfile::TempDir::new().unwrap();
        let dir = index_dir.path().join("index");
        let _first = SearchEngine::open(Arc::clone(&manager), &dir)
            .await
            .unwrap();
        assert!(
            SearchEngine::open(Arc::clone(&manager), &dir)
                .await
                .is_err()
        );
    }

    /// An index written for another layout or vault is rebuilt.
    #[tokio::test]
    async fn a_foreign_index_is_rebuilt() {
        let (_vault, manager) = vault_with(&[("a.md", "# A\n\nwalrus\n")]).await;
        let index_dir = tempfile::TempDir::new().unwrap();
        let dir = index_dir.path().join("index");
        drop(
            SearchEngine::open(Arc::clone(&manager), &dir)
                .await
                .unwrap(),
        );
        std::fs::write(
            dir.join(INDEX_MARKER),
            r#"{"version":0,"turbovault":"0.0.0","vault":"/elsewhere"}"#,
        )
        .unwrap();
        let (engine, reindexed) = SearchEngine::open_counting(Arc::clone(&manager), &dir)
            .await
            .unwrap();
        assert_eq!(reindexed, 1);
        assert_eq!(paths(&engine.search("walrus").await.unwrap()), ["a.md"]);
    }

    /// Results count the notes linking to them. The graph is keyed by
    /// absolute path, and looking it up by the vault-relative path always
    /// found none.
    #[tokio::test]
    async fn results_count_their_backlinks() {
        let (_vault, manager) = vault_with(&[
            ("target.md", "# Target\n\nwalrus\n"),
            ("one.md", "[[target]] and [[target]] again\n"),
            ("two.md", "see [[target]]\n"),
        ])
        .await;
        let engine = SearchEngine::new(manager).await.unwrap();
        let results = engine.search("walrus").await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].backlink_count, 2);
    }
}
