//! GraphProvider MCP capabilities.

use std::ops::Deref;

use super::super::*;

#[derive(Clone)]
pub(super) struct GraphProvider(CoreToolHandler);

impl GraphProvider {
    pub(super) fn new(core: CoreToolHandler) -> Self {
        Self(core)
    }
}

impl Deref for GraphProvider {
    type Target = CoreToolHandler;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[turbomcp::server(name = "obsidian-vault", version = "1.5.0")]
impl GraphProvider {
    // ==================== Search & Links ====================

    /// Find all notes that link to this note
    #[tool(
        description = "Find all notes that link TO this note (incoming links)",
        usage = "Use to understand note importance in knowledge graph, discover related content, and analyze impact before deletion. Essential for bidirectional link analysis.",
        performance = "Fast retrieval from pre-built link graph (<50ms typical)",
        related = ["get_forward_links", "get_related_notes", "get_hub_notes"],
        examples = [],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_backlinks(&self, path: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = SearchTools::new(manager);
        let backlinks = tools.find_backlinks(&path).await.map_err(to_mcp_error)?;

        let count = backlinks.len();
        let response =
            StandardResponse::new(vault_name, "get_backlinks", serde_json::json!(backlinks))
                .with_count(count)
                .with_next_step("get_forward_links")
                .with_next_step("get_related_notes");

        let response = if count == 0 {
            response.with_warning("Note has no incoming links".to_string())
        } else {
            response
        };

        response.to_json()
    }

    /// Find all notes that this note links to
    #[tool(
        description = "Find all notes that this note links TO (outgoing links)",
        usage = "Use to understand note dependencies, validate link integrity, and explore connection patterns. Pair with get_backlinks for bidirectional link analysis.",
        performance = "Fast retrieval from pre-built link graph (<50ms typical)",
        related = ["get_backlinks", "get_related_notes", "get_broken_links"],
        examples = [],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_forward_links(&self, path: String) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = SearchTools::new(manager);
        let links = tools
            .find_forward_links(&path)
            .await
            .map_err(to_mcp_error)?;

        let count = links.len();
        let response =
            StandardResponse::new(vault_name, "get_forward_links", serde_json::json!(links))
                .with_count(count)
                .with_next_step("get_backlinks")
                .with_next_step("get_related_notes");

        response.to_json()
    }

    /// Find related notes (by link proximity)
    #[tool(
        description = "Find notes connected within N hops in the link graph (default 2 hops)",
        usage = "Use to discover non-obvious relationships through graph traversal. Ideal for recommendations, cluster analysis, and exploring knowledge neighborhoods. Configurable max_hops parameter.",
        performance = "Graph traversal speed varies by depth: 2 hops <100ms typical, 3+ hops may take longer on large vaults",
        related = ["recommend_related", "get_hub_notes", "suggest_links"],
        examples = [],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_related_notes(
        &self,
        path: String,
        max_hops: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = SearchTools::new(manager);
        let max_hops = max_hops.unwrap_or(2).min(5); // Cap at 5 hops to prevent runaway traversal
        let related = tools
            .find_related_notes(&path, max_hops)
            .await
            .map_err(to_mcp_error)?;

        let count = related.len();
        let response =
            StandardResponse::new(vault_name, "get_related_notes", serde_json::json!(related))
                .with_count(count)
                .with_meta("max_hops", serde_json::json!(max_hops));

        response.to_json()
    }

    // ==================== Analysis ====================

    /// Find hub notes (highly connected)
    #[tool(
        description = "Find the top N most connected notes in the vault (default 10). Returns notes ranked by total link count (incoming + outgoing). Hub notes are central to knowledge graph structure and often represent key concepts or index pages.",
        usage = "Identify knowledge centers, validate vault organization, discover MOCs (Maps of Content)",
        performance = "<50ms typical, scales linearly with vault size",
        related = ["get_centrality_ranking", "get_dead_end_notes", "explain_vault"],
        examples = [],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_hub_notes(&self, top_n: Option<usize>) -> McpResult<serde_json::Value> {
        let top_n = top_n.unwrap_or(10);
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let hubs = tools.get_hub_notes(top_n).await.map_err(to_mcp_error)?;

        let count = hubs.len();
        let response = StandardResponse::new(
            vault_name,
            "get_hub_notes",
            serde_json::to_value(&hubs).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_count(count)
        .with_next_step("get_related_notes");

        response.to_json()
    }

    /// Find dead-end notes (incoming but no outgoing)
    #[tool(
        description = "Find notes with incoming links but NO outgoing links (knowledge dead-ends). Returns list of paths with backlink counts. Dead-ends may indicate incomplete notes, missing connections, or final destination topics.",
        usage = "Identify incomplete notes needing expansion, discover topics lacking context, prioritize linking work",
        performance = "<100ms typical, graph traversal O(N)",
        related = ["suggest_links", "get_hub_notes", "get_isolated_clusters"],
        examples = [],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_dead_end_notes(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let dead_ends = tools.get_dead_end_notes().await.map_err(to_mcp_error)?;

        let count = dead_ends.len();
        let response = StandardResponse::new(
            vault_name,
            "get_dead_end_notes",
            serde_json::json!(dead_ends),
        )
        .with_count(count);

        response.to_json()
    }

    /// Find isolated clusters in vault
    #[tool(
        description = "Find disconnected groups of notes (subgraphs with no connections to main graph). Returns clusters as arrays of paths. Isolated clusters may represent separate projects, orphaned content, or incomplete knowledge areas.",
        usage = "Improve vault connectivity, discover orphaned content, validate vault structure",
        performance = "<200ms typical, uses union-find algorithm O(N)",
        related = ["suggest_links", "get_dead_end_notes", "full_health_analysis"],
        examples = [],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn get_isolated_clusters(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let clusters = tools.get_isolated_clusters().await.map_err(to_mcp_error)?;

        let count = clusters.len();
        let response = StandardResponse::new(
            vault_name,
            "get_isolated_clusters",
            serde_json::json!(clusters),
        )
        .with_count(count);

        response.to_json()
    }

    // ==================== Health & Validation ====================

    /// Quick health check (0-100 score)
    #[tool(
        description = "Perform fast health assessment of active vault returning 0-100 score",
        usage = "Use as first diagnostic before deeper analysis. Score <60 suggests issues needing attention",
        performance = "Fast - optimized for speed with <100ms typical response using heuristics not exhaustive analysis",
        related = ["full_health_analysis", "get_broken_links", "detect_cycles"],
        examples = ["quick vault check", "is my vault healthy?", "vault health score"],
        tags = ["read", "health"],
        read_only = true,
    )]
    async fn quick_health_check(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let health = tools.quick_health_check().await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(
            vault_name,
            "quick_health_check",
            serde_json::to_value(&health).map_err(|e| McpError::internal(e.to_string()))?,
        )
        .with_next_step("full_health_analysis")
        .with_next_step(if health.is_healthy {
            "recommend_related"
        } else {
            "get_broken_links"
        });

        response.to_json()
    }

    /// Full health analysis with detailed report
    #[tool(
        description = "Comprehensive vault health report with detailed metrics including broken links, orphan analysis, link density, cluster analysis, and recommendations",
        usage = "Use when quick_health_check reveals issues or before major vault refactoring. Provides actionable insights for vault improvement",
        performance = "Slow - may take several seconds on large vaults. Significantly slower than quick_health_check due to exhaustive analysis",
        related = ["quick_health_check", "export_health_report", "explain_vault"],
        examples = ["detailed health analysis", "comprehensive vault check", "what are all my vault issues?"],
        tags = ["read", "health"],
        read_only = true,
    )]
    async fn full_health_analysis(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let health = tools.full_health_analysis().await.map_err(to_mcp_error)?;

        let mut response = StandardResponse::new(
            vault_name,
            "full_health_analysis",
            serde_json::to_value(&health).map_err(|e| McpError::internal(e.to_string()))?,
        );

        // Add metadata about analysis
        response = response.with_meta("analysis_type", serde_json::json!("comprehensive"));

        // Suggest next actions based on health status
        if health.broken_links_count > 0 {
            response = response.with_next_step("get_broken_links");
        }
        if health.orphaned_notes_count > 0 {
            response = response.with_next_step("suggest_links");
        }

        response.to_json()
    }

    /// Get all broken links in vault
    #[tool(
        description = "Find all links pointing to non-existent notes with source path, target path, link text, and line number for each broken link",
        usage = "Use to identify notes to create or links to fix. Broken links harm navigation and indicate incomplete knowledge graph",
        performance = "Moderate - scans all notes and validates link targets, scales with vault size",
        related = ["suggest_links", "full_health_analysis", "export_broken_links"],
        examples = ["find broken links", "which links are broken?", "show missing note targets"],
        tags = ["read", "health"],
        read_only = true,
    )]
    async fn get_broken_links(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let broken = tools.get_broken_links().await.map_err(to_mcp_error)?;

        let count = broken.len();
        let response =
            StandardResponse::new(vault_name, "get_broken_links", serde_json::json!(broken))
                .with_count(count);

        let response = if count > 0 {
            response
                .with_warning(format!("Found {} broken links", count))
                .with_next_step("export_broken_links")
        } else {
            response
        };

        response.to_json()
    }

    /// Detect cycles in link graph
    #[tool(
        description = "Detect circular reference chains in the link graph returning all cycles as arrays of paths",
        usage = "Use for graph topology analysis. Cycles aren't necessarily bad (many knowledge domains are naturally circular) but may indicate redundant structure or need for hub notes",
        performance = "Moderate - performs graph traversal to detect cycles, scales with vault complexity and link density",
        related = ["get_hub_notes", "full_health_analysis", "get_related_notes"],
        examples = ["find circular links", "detect reference cycles", "A→B→C→A patterns"],
        tags = ["read", "graph"],
        read_only = true,
    )]
    async fn detect_cycles(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let cycles = tools.detect_cycles().await.map_err(to_mcp_error)?;

        let count = cycles.len();
        let response =
            StandardResponse::new(vault_name, "detect_cycles", serde_json::json!(cycles))
                .with_count(count);

        let response = if count > 0 {
            response
                .with_warning("Cycles detected in link graph".to_string())
                .with_next_step("get_broken_links")
        } else {
            response
        };

        response.to_json()
    }

    /// **HOLISTIC VAULT OVERVIEW** - Complete gestalt view for LLMs (FIX 7: Single call replaces 5+ separate calls)
    /// Provides all essential vault structure info at once: organization, health, hubs, orphans, recommendations
    #[tool(
        description = "Generate holistic vault overview in a single comprehensive call",
        usage = "Use as comprehensive diagnostic or for presenting complete vault state. Replaces 5+ separate calls (scan + health + hubs + orphans + stats)",
        performance = "SLOW (1-5 seconds on large vaults) - aggregates multiple analyses. Use quick_health_check for fast diagnostics",
        related = ["get_vault_context", "full_health_analysis", "get_hub_notes", "quick_health_check"],
        examples = ["Get complete vault status before refactoring", "Present vault health to user", "Generate comprehensive diagnostic report"],
        tags = ["read"],
        read_only = true,
    )]
    async fn explain_vault(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let graph_tools = GraphTools::new(manager.clone());
        let analysis_tools = AnalysisTools::new(manager.clone());

        // Get all data efficiently (parallelizable)
        let files = manager.scan_vault().await.map_err(to_mcp_error)?;
        let health = graph_tools
            .quick_health_check()
            .await
            .map_err(to_mcp_error)?;
        let hubs = graph_tools.get_hub_notes(10).await.map_err(to_mcp_error)?;
        let dead_ends = graph_tools
            .get_dead_end_notes()
            .await
            .map_err(to_mcp_error)?;
        let stats = analysis_tools
            .get_vault_stats()
            .await
            .map_err(to_mcp_error)?;

        // Organize files by folder
        let mut folders: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for file in &files {
            if file
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("md"))
            {
                let file_str = manager.relative_path(file);
                let folder = std::path::Path::new(&file_str)
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map_or_else(
                        || "root".to_string(),
                        |parent| parent.to_string_lossy().replace('\\', "/"),
                    );
                folders.entry(folder).or_default().push(file_str);
            }
        }

        // Create holistic overview
        let overview = serde_json::json!({
            "vault_name": vault_name,
            "quick_facts": {
                "total_files": stats.total_files,
                "total_links": stats.total_links,
                "orphaned": stats.orphaned_files,
                "health_score": health.health_score,
                "is_healthy": health.is_healthy
            },
            "structure": {
                "folders": folders.keys().collect::<Vec<_>>(),
                "file_count_by_folder": folders.iter()
                    .map(|(k, v)| (k.clone(), v.len()))
                    .collect::<std::collections::HashMap<_, _>>(),
            },
            "key_insights": {
                "hub_notes": hubs.iter().take(5).map(|(path, count)| serde_json::json!({
                    "path": manager.relative_path(std::path::Path::new(path)),
                    "connections": count
                })).collect::<Vec<_>>(),
                "dead_ends": dead_ends.iter().take(5)
                    .map(|path| manager.relative_path(std::path::Path::new(path)))
                    .collect::<Vec<_>>(),
                "average_links_per_file": stats.average_links_per_file,
            },
            "recommendations": {
                "action_1": if stats.orphaned_files > 0 {
                    format!("Link {} orphaned notes to main index or other hub notes", stats.orphaned_files)
                } else {
                    "Vault is well-connected".to_string()
                },
                "action_2": if health.broken_links_count > 0 {
                    format!("Fix {} broken links (use get_broken_links for details)", health.broken_links_count)
                } else {
                    "No broken links".to_string()
                },
                "action_3": if hubs.len() > 3 {
                    "Create hub pages for your top 3-5 topics".to_string()
                } else {
                    "Consider creating more cross-linking between topics".to_string()
                }
            }
        });

        let response = StandardResponse::new(vault_name, "explain_vault", overview)
            .with_meta(
                "view_type".to_string(),
                serde_json::json!("holistic_gestalt"),
            )
            .with_meta(
                "alternatives".to_string(),
                serde_json::json!([
                    "search() - Find notes by keyword",
                    "get_hub_notes() - See most connected notes",
                    "full_health_analysis() - Detailed health report",
                    "query_metadata() - Search by frontmatter"
                ]),
            )
            .with_next_steps(&[
                if stats.orphaned_files > 0 {
                    "get_dead_end_notes"
                } else {
                    "search"
                },
                if health.broken_links_count > 0 {
                    "get_broken_links"
                } else {
                    "get_hub_notes"
                },
            ]);

        response.to_json()
    }
}
