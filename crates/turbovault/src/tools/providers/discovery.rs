//! DiscoveryProvider MCP capabilities.

use std::collections::BTreeMap;
use std::ops::Deref;

use turbovault_tools::SearchResultInfo;

use super::super::*;

#[derive(Clone)]
pub(super) struct DiscoveryProvider(CoreToolHandler);

impl DiscoveryProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for DiscoveryProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// One ranked note, as the search tools return it. Concise leaves out the
/// fields marked below; the rest are always there.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct SearchHit {
    path: String,
    title: String,
    /// The first 200 characters of the note. Omitted when concise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preview: Option<String>,
    score: f64,
    snippet: String,
    tags: Vec<String>,
    /// Omitted when concise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outgoing_links: Option<Vec<String>>,
    backlink_count: usize,
    /// Omitted when concise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    word_count: Option<usize>,
    /// Omitted when concise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    char_count: Option<usize>,
}

impl SearchHit {
    fn new(result: SearchResultInfo, concise: bool) -> Self {
        let detail = !concise;
        Self {
            path: result.path,
            title: result.title,
            preview: detail.then_some(result.preview),
            score: result.score,
            snippet: result.snippet,
            tags: result.tags,
            outgoing_links: detail.then_some(result.outgoing_links),
            backlink_count: result.backlink_count,
            word_count: detail.then_some(result.word_count),
            char_count: detail.then_some(result.char_count),
        }
    }
}

/// The results tantivy ranked, cut to the page asked for.
fn ranked_page(
    request: &PageRequest,
    results: Vec<SearchResultInfo>,
    response_format: Option<ResponseFormat>,
) -> (Vec<SearchHit>, Page) {
    let concise = ResponseFormat::is_concise(response_format);
    let (results, page) = request.slice_ranked(results);
    let hits = results
        .into_iter()
        .map(|result| SearchHit::new(result, concise))
        .collect();
    (hits, page)
}

/// What `inspect_frontmatter` returns. Only its schema is used: the SQL
/// engine builds the value.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct FrontmatterInspection {
    file_count: usize,
    column_count: usize,
    /// Each column (frontmatter key, plus `path`) by name.
    schema: BTreeMap<String, FrontmatterColumn>,
    tables: SqlTables,
    sample_paths: Vec<String>,
    usage: String,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct FrontmatterColumn {
    /// `string`, `number`, `boolean`, `array`, `object`, `null` or `mixed`.
    #[serde(rename = "type")]
    type_: String,
    nullable: bool,
    count: usize,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct SqlTables {
    files: String,
    tags: String,
    links: String,
}

/// What `query_frontmatter_sql` returns. Only its schema is used: the SQL
/// engine builds the value.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct SqlQueryResult {
    file_count: usize,
    tag_count: usize,
    link_count: usize,
    /// The statement's result: rows for a SELECT (with `truncated` and
    /// `total_rows` when cut), or one result per statement for several.
    result: serde_json::Value,
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl DiscoveryProvider {
    // ==================== Search (LLM Discovery) ====================

    /// Search the vault by keyword, tag, frontmatter or content resemblance.
    #[tool(
        description = "Find notes, ranked by relevance (BM25). Give `query` for keywords, narrow with `tags`, `frontmatter_filters` and `exclude_paths`, or give only filters to list every note that matches them. With `related_to` instead, find notes whose content resembles that note's. Each result has the path, title, a snippet, tags and links. For conditions, joins or aggregates over frontmatter use query_frontmatter_sql; for meaning rather than words, find_similar.",
        title = "Search Notes",
        tags = ["read", "search"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<SearchHit>>,
    )]
    // Each argument is one of the tool's MCP parameters; the macro reads the
    // input schema from this signature.
    #[allow(clippy::too_many_arguments)]
    async fn search(
        &self,
        #[description(
            "Words to search for, in tantivy query syntax: `\"exact phrase\"`, `+required`, `-excluded`, `title:word`. Omit (or `*`) to match every note and rank by the filters alone."
        )]
        query: Option<String>,
        #[description(
            "Only notes carrying all of these tags, without `#`, such as `[\"project\", \"active\"]`."
        )]
        tags: Option<Vec<String>>,
        #[description(
            "Only notes whose frontmatter matches every filter. Each filter is `{key, value}` and matches when the value contains `value`, such as `{\"key\": \"status\", \"value\": \"draft\"}`."
        )]
        frontmatter_filters: Option<Vec<FrontmatterFilter>>,
        #[description(
            "Leave out notes under these vault-relative folders or paths, such as `[\"archive/\", \"templates/\"]`."
        )]
        exclude_paths: Option<Vec<String>>,
        #[description(
            "Vault-relative path of a note: find notes whose content resembles it, instead of searching by keyword. Not combined with the other filters."
        )]
        related_to: Option<String>,
        #[description("Most results to return. Default 10, maximum 100.")] limit: Option<usize>,
        #[description(
            "How many top results to skip, for the next page. Default 0, at most 1000; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
        #[description(
            "`detailed` (default) or `concise`. Concise leaves out each result's `preview`, `outgoing_links`, `word_count` and `char_count`, keeping the path, title, score, snippet, tags and backlink count."
        )]
        response_format: Option<ResponseFormat>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;
        let request = PageRequest::ranked(offset, limit, 10, MAX_SEARCH_RESULTS);

        let results = if let Some(path) = related_to {
            let filtered = query.is_some()
                || tags.is_some()
                || frontmatter_filters.is_some()
                || exclude_paths.is_some();
            if filtered {
                return Err(McpError::invalid_params(
                    "related_to finds look-alikes of one note and takes no query or filters; \
                     search with those, or with related_to, in separate calls"
                        .to_string(),
                ));
            }
            engine
                .find_related(&path, request.fetch())
                .await
                .map_err(to_mcp_error)?
        } else {
            let query = query
                .filter(|query| !query.trim().is_empty())
                .unwrap_or_else(|| "*".to_string());
            let mut search_query = SearchQuery::new(query).limit(request.fetch());
            if let Some(tags) = tags {
                search_query = search_query.with_tags(tags);
            }
            for filter in frontmatter_filters.into_iter().flatten() {
                search_query = search_query.with_frontmatter(filter.key, filter.value);
            }
            if let Some(excludes) = exclude_paths {
                search_query = search_query.exclude(excludes);
            }
            // A page of none is known without asking tantivy.
            if request.limit == 0 {
                Vec::new()
            } else {
                engine
                    .advanced_search(search_query)
                    .await
                    .map_err(to_mcp_error)?
            }
        };
        let (result_data, page) = ranked_page(&request, results, response_format);

        StandardResponse::new(vault_name, "search", result_data)
            .with_page(page)
            .with_next_steps(&["read_note", "find_similar"])
            .to_json()
    }

    // ==================== SQL Frontmatter Queries (feature = "sql") ====================

    /// Inspect frontmatter schema
    #[tool(
        description = "Describe the frontmatter across all notes as SQL columns: each key's name, type, how many notes have it, and whether it can be empty. Call it before query_frontmatter_sql to learn what columns exist.",
        title = "Inspect Frontmatter",
        tags = ["read", "frontmatter"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<FrontmatterInspection>,
    )]
    async fn inspect_frontmatter(&self) -> McpResult<serde_json::Value> {
        #[cfg(feature = "sql")]
        {
            let (vault_name, manager) = self.get_vault_pair().await?;
            let engine = FrontmatterSqlEngine::new(manager);
            let result = engine.inspect().await.map_err(to_mcp_error)?;

            let response = StandardResponse::new(vault_name, "inspect_frontmatter", result)
                .with_next_step("query_frontmatter_sql");

            response.to_json()
        }
        #[cfg(not(feature = "sql"))]
        {
            Err(McpError::unavailable(
                "SQL feature not enabled. Rebuild TurboVault with: cargo build --features sql"
                    .to_string(),
            ))
        }
    }

    /// Execute SQL query against frontmatter
    #[tool(
        description = "Run a read-only SQL SELECT against a 'files' table built from all vault note frontmatter, plus 'tags' (path, tag) and 'links' (source, target, link_type, is_valid). Each note becomes a row with 'path' + all frontmatter keys as columns. Supports WHERE, JOIN, GROUP BY, ORDER BY, LIMIT, subqueries, and aggregations. Only SELECT runs; results stop at 1000 rows (marked truncated, with total_rows) and a query has 10 seconds",
        title = "Query Frontmatter with SQL",
        tags = ["read", "frontmatter", "sql"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<SqlQueryResult>,
    )]
    async fn query_frontmatter_sql(
        &self,
        #[description(
            "One SQL SELECT, such as `SELECT path, status FROM files WHERE status = 'draft' ORDER BY path`. Quote frontmatter keys with spaces or dashes in double quotes."
        )]
        sql: String,
    ) -> McpResult<serde_json::Value> {
        #[cfg(feature = "sql")]
        {
            let (vault_name, manager) = self.get_vault_pair().await?;
            let engine = FrontmatterSqlEngine::new(manager);
            let result = engine.query(&sql).await.map_err(to_mcp_error)?;

            let response = StandardResponse::new(vault_name, "query_frontmatter_sql", result)
                .with_next_steps(&["inspect_frontmatter", "read_note"]);

            response.to_json()
        }
        #[cfg(not(feature = "sql"))]
        {
            let _ = sql;
            Err(McpError::unavailable(
                "SQL feature not enabled. Rebuild TurboVault with: cargo build --features sql"
                    .to_string(),
            ))
        }
    }
}
