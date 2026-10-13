//! DiscoveryProvider MCP capabilities.

use std::ops::Deref;

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

/// The results tantivy ranked, cut to the page asked for, as JSON.
fn ranked_page(
    request: &PageRequest,
    results: Vec<turbovault_tools::SearchResultInfo>,
) -> McpResult<(serde_json::Value, Page)> {
    let (results, page) = request.slice_ranked(results);
    let data = serde_json::to_value(&results).map_err(|e| McpError::internal(e.to_string()))?;
    Ok((data, page))
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl DiscoveryProvider {
    // ==================== Search (LLM Discovery) ====================

    /// Search vault by keyword
    #[tool(
        description = "Full-text keyword search across every note, ranked by relevance (BM25). Each result has the path, title, a snippet, tags and links. Use advanced_search to filter by tag, frontmatter or folder.",
        title = "Search Notes",
        tags = ["read", "search"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
    )]
    async fn search(
        &self,
        #[description(
            "Words to search for. Supports tantivy query syntax: `\"exact phrase\"`, `+required`, `-excluded`, `title:word`."
        )]
        query: String,
        #[description("Most results to return. Default 10, maximum 100.")] limit: Option<usize>,
        #[description(
            "How many top results to skip, for the next page. Default 0, at most 1000; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;
        let request = PageRequest::ranked(offset, limit, 10, MAX_SEARCH_RESULTS);
        let results = engine
            .advanced_search(SearchQuery::new(query).limit(request.fetch()))
            .await
            .map_err(to_mcp_error)?;
        let (result_data, page) = ranked_page(&request, results)?;

        let response = StandardResponse::new(vault_name, "search", result_data)
            .with_page(page)
            .with_next_step("advanced_search")
            .with_next_step("recommend_related");

        response.to_json()
    }

    /// Advanced search with filters
    #[tool(
        description = "Keyword search narrowed by tags, frontmatter values and excluded folders, ranked by relevance. Use search for a plain keyword query, or search_by_frontmatter to match one field without a keyword.",
        title = "Advanced Search",
        tags = ["read", "search"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
    )]
    async fn advanced_search(
        &self,
        #[description(
            "Words to search for, in tantivy query syntax as for search. Use `*` to match every note and filter only."
        )]
        query: String,
        #[description(
            "Only notes carrying all of these tags, without `#`, such as `[\"project\", \"active\"]`."
        )]
        tags: Option<Vec<String>>,
        #[description(
            "Only notes whose frontmatter matches every filter. Each filter is `{key, value}` and matches when the value contains `value`."
        )]
        frontmatter_filters: Option<Vec<FrontmatterFilter>>,
        #[description(
            "Leave out notes under these vault-relative folders or paths, such as `[\"archive/\", \"templates/\"]`."
        )]
        exclude_paths: Option<Vec<String>>,
        #[description("Most results to return. Default 10, maximum 100.")] limit: Option<usize>,
        #[description(
            "How many top results to skip, for the next page. Default 0, at most 1000; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;

        let request = PageRequest::ranked(offset, limit, 10, MAX_SEARCH_RESULTS);
        let mut search_query = SearchQuery::new(query).limit(request.fetch());

        if let Some(tags) = tags {
            search_query = search_query.with_tags(tags);
        }
        if let Some(filters) = frontmatter_filters {
            for f in filters {
                search_query = search_query.with_frontmatter(f.key, f.value);
            }
        }
        if let Some(excludes) = exclude_paths {
            search_query = search_query.exclude(excludes);
        }

        // A page of none is known without asking tantivy.
        let results = if request.limit == 0 {
            Vec::new()
        } else {
            engine
                .advanced_search(search_query)
                .await
                .map_err(to_mcp_error)?
        };
        let (result_data, page) = ranked_page(&request, results)?;

        let response = StandardResponse::new(vault_name, "advanced_search", result_data)
            .with_page(page)
            .with_next_step("search");

        response.to_json()
    }

    /// Search by frontmatter key-value pair
    #[tool(
        description = "Find notes whose frontmatter field contains a value, such as `status` = `draft`. For conditions, joins or aggregates over frontmatter use query_frontmatter_sql.",
        title = "Search by Frontmatter",
        tags = ["read", "search"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
    )]
    async fn search_by_frontmatter(
        &self,
        #[description("Frontmatter key to match, such as `status` or `type`.")] key: String,
        #[description(
            "Value to look for in that key; a note matches when its value contains this text."
        )]
        value: String,
        #[description("Most notes to return. Default 50, maximum 100.")] limit: Option<usize>,
        #[description(
            "How many matches to skip, for the next page. Default 0, at most 1000; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;
        let request = PageRequest::ranked(offset, limit, DEFAULT_PAGE_SIZE, MAX_SEARCH_RESULTS);

        let results = engine
            .advanced_search(
                SearchQuery::new("*")
                    .with_frontmatter(key, value)
                    .limit(request.fetch()),
            )
            .await
            .map_err(to_mcp_error)?;
        let (result_data, page) = ranked_page(&request, results)?;

        let response = StandardResponse::new(vault_name, "search_by_frontmatter", result_data)
            .with_page(page)
            .with_next_steps(&["advanced_search", "read_note"]);

        response.to_json()
    }

    /// Find related notes (recommendations engine)
    #[tool(
        description = "Find notes whose content resembles a note's, ranked by similarity, with scores. Use it to suggest reading or linking; get_related_notes follows existing links instead.",
        title = "Recommend Related Notes",
        tags = ["read", "search"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
    )]
    async fn recommend_related(
        &self,
        #[description(
            "Vault-relative path of the note to find relatives of, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description("Most recommendations to return. Default 5, maximum 100.")] limit: Option<
            usize,
        >,
        #[description(
            "How many top recommendations to skip, for the next page. Default 0, at most 1000."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let engine = self.get_search_engine(&vault_name, &manager).await?;
        let request = PageRequest::ranked(offset, limit, 5, MAX_SEARCH_RESULTS);
        let results = engine
            .find_related(&path, request.fetch())
            .await
            .map_err(to_mcp_error)?;
        let (result_data, page) = ranked_page(&request, results)?;

        let response = StandardResponse::new(vault_name, "recommend_related", result_data)
            .with_page(page)
            .with_next_step("get_related_notes");

        response.to_json()
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
