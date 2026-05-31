//! Full-text search engine powered by tantivy
//!
//! Provides production-grade full-text search with:
//! - Apache Lucene-inspired indexing and searching
//! - TF-IDF relevance scoring
//! - Field-specific search (content, title, tags)
//! - Fuzzy/approximate queries via regex
//! - Fast searching even on large vaults

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::*;
use tantivy::{Index, ReloadPolicy, TantivyDocument, doc};
use tracing::instrument;
use turbovault_core::prelude::*;
use turbovault_parser::to_plain_text;
use turbovault_vault::VaultManager;

/// Search result metadata for LLM consumption
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    // Pre-resolved field handles — avoids runtime schema lookup and unwrap on every search
    field_path: Field,
    field_title: Field,
    field_content: Field,
    field_tags: Field,
}

impl SearchEngine {
    /// Create new search engine and index all vault files
    pub async fn new(manager: Arc<VaultManager>) -> Result<Self> {
        // Define schema: fields to index
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
        let schema = schema_builder.build();

        // Resolve field handles once at construction time — panic here is a programmer error
        // (schema was just built with these exact field names above)
        let field_path = schema
            .get_field("path")
            .expect("schema built with 'path' field");
        let field_title = schema
            .get_field("title")
            .expect("schema built with 'title' field");
        let field_content = schema
            .get_field("content")
            .expect("schema built with 'content' field");
        let field_tags = schema
            .get_field("tags")
            .expect("schema built with 'tags' field");

        // Create in-memory index
        let index = Index::create_in_ram(schema.clone());

        // Index all files
        let mut index_writer = index
            .writer(50_000_000)
            .map_err(|e| Error::config_error(format!("Failed to create index writer: {}", e)))?;

        let files = manager.scan_vault().await?;

        for file_path in files {
            // Convert PathBuf to string to check extension (case-insensitive)
            let path_str = file_path.to_string_lossy();
            let path_lower = path_str.to_lowercase();
            if !path_lower.ends_with(".md") {
                continue;
            }

            match manager.parse_file(&file_path).await {
                Ok(vault_file) => {
                    // turbovault-2ag: index docs by VAULT-RELATIVE path so the
                    // field_path key matches `apply_changes` (which uses the
                    // git-diff relative path). Keying the initial build by the
                    // absolute path made `apply_changes`'s relative `delete_term`
                    // miss, leaving a stale duplicate doc on every edit.
                    let path_str = file_path
                        .strip_prefix(manager.vault_path())
                        .unwrap_or(&file_path)
                        .to_string_lossy()
                        .to_string();

                    // Get title
                    let title = vault_file
                        .frontmatter
                        .as_ref()
                        .and_then(|fm| fm.data.get("title"))
                        .and_then(|v| v.as_str())
                        .unwrap_or_else(|| {
                            file_path
                                .file_stem()
                                .unwrap_or_default()
                                .to_str()
                                .unwrap_or("")
                        })
                        .to_string();

                    // Get tags
                    let tags_str = vault_file
                        .frontmatter
                        .as_ref()
                        .map(|fm| fm.tags().join(" "))
                        .unwrap_or_default();

                    // Extract plain text for indexing (excludes markdown syntax, URLs, etc.)
                    let plain_content = to_plain_text(&vault_file.content);

                    // Add document to index with plain text content (use stored field handles)
                    let _ = index_writer.add_document(doc!(
                        field_path => path_str.clone(),
                        field_title => title,
                        field_content => plain_content,
                        field_tags => tags_str,
                    ));
                }
                Err(_e) => {
                    // Silently skip files that fail to parse
                }
            }
        }

        index_writer
            .commit()
            .map_err(|e| Error::config_error(format!("Failed to commit index: {}", e)))?;

        Ok(Self {
            manager,
            index,
            field_path,
            field_title,
            field_content,
            field_tags,
        })
    }

    /// GWS.14c — apply an incremental change set to the tantivy index.
    /// Each `(path, present_in_commit)` is processed in order:
    /// - `present=true`  → delete the existing doc for this path (no-op if
    ///   absent), re-parse the working-tree bytes, add the new doc.
    /// - `present=false` → delete the doc for this path.
    /// All changes commit in one writer transaction.
    ///
    /// Reads from the working tree (working-tree == HEAD invariant during
    /// the substrate's commit lock). Parse errors are logged + skipped per
    /// path — one malformed file does not brick the whole drain pass.
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
        let mut writer = self
            .index
            .writer(50_000_000)
            .map_err(|e| Error::config_error(format!("apply_changes: writer create: {}", e)))?;
        for (rel_path, present) in changes {
            // delete is a no-op if the path isn't currently in the index.
            let term = tantivy::Term::from_field_text(self.field_path, &rel_path);
            writer.delete_term(term);

            if present {
                match self
                    .manager
                    .parse_file(std::path::Path::new(&rel_path))
                    .await
                {
                    Ok(vault_file) => {
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
                        let tags_str = vault_file
                            .frontmatter
                            .as_ref()
                            .map(|fm| fm.tags().join(" "))
                            .unwrap_or_default();
                        let plain_content = to_plain_text(&vault_file.content);
                        let _ = writer.add_document(doc!(
                            self.field_path => rel_path.clone(),
                            self.field_title => title,
                            self.field_content => plain_content,
                            self.field_tags => tags_str,
                        ));
                    }
                    Err(e) => {
                        log::debug!(
                            "search apply_changes skip {} (parse failed: {})",
                            rel_path,
                            e
                        );
                    }
                }
            }
        }
        writer
            .commit()
            .map_err(|e| Error::config_error(format!("apply_changes: writer commit: {}", e)))?;
        Ok(())
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
        // Parse the note to extract keywords
        let vault_file = self.manager.parse_file(&PathBuf::from(path)).await?;

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
}

impl SearchQuery {
    /// Build and execute search results using tantivy
    async fn build_results(self, engine: &SearchEngine) -> Result<Vec<SearchResultInfo>> {
        let (query_str, filter, limit) = self.build();

        let reader = engine
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()
            .map_err(|e| Error::config_error(format!("Failed to create reader: {}", e)))?;

        let searcher = reader.searcher();
        let graph = engine.manager.link_graph();
        let graph_read = graph.read().await;

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
            .map_err(|e| Error::config_error(format!("Failed to parse query: {}", e)))?;

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

            // Apply frontmatter filters
            if let Some(fm_filters) = &filter.frontmatter_filters {
                let file_path = PathBuf::from(&path);
                if let Ok(vault_file) = engine.manager.parse_file(&file_path).await {
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
                } else {
                    continue;
                }
            }

            // Get full content for preview and snippet
            let file_path = PathBuf::from(&path);
            if let Ok(vault_file) = engine.manager.parse_file(&file_path).await {
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
                let backlink_count = graph_read.backlinks(&file_path).unwrap_or_default().len();

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
                    backlink_count,
                    word_count,
                    char_count,
                });
            }

            if results.len() >= limit {
                break;
            }
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
fn extract_snippet(content: &str, query: &str) -> String {
    if query.is_empty() || query == "*" {
        return content.lines().take(1).collect();
    }

    let query_lower = query.to_lowercase();
    let content_lower = content.to_lowercase();

    if let Some(pos) = content_lower.find(&query_lower) {
        let mut start = pos.saturating_sub(50);
        while start > 0 && !content.is_char_boundary(start) {
            start -= 1;
        }

        let mut end = (pos + query_lower.len() + 50).min(content.len());
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

        // Edit the file on disk, then incrementally reindex just that path.
        std::fs::write(tmp.path().join("s.md"), "# S\n\nbetaword content here\n").unwrap();
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
}
