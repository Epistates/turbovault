//! GraphProvider MCP capabilities.

use std::collections::BTreeMap;
use std::ops::Deref;

use turbovault_tools::{BrokenLinkInfo, HealthInfo};

use super::super::*;

/// Response payload for `explain_vault`.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct ExplainVaultPayload {
    vault_name: String,
    quick_facts: QuickFacts,
    /// Folders and their note counts. Omitted when concise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    structure: Option<VaultStructure>,
    key_insights: KeyInsights,
    recommendations: Recommendations,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct QuickFacts {
    total_files: usize,
    total_links: usize,
    orphaned: usize,
    health_score: u8,
    is_healthy: bool,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct VaultStructure {
    /// Folders holding notes, `root` for the vault root.
    folders: Vec<String>,
    file_count_by_folder: BTreeMap<String, usize>,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct KeyInsights {
    hub_notes: Vec<HubConnections>,
    dead_ends: Vec<String>,
    average_links_per_file: f64,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct HubConnections {
    path: String,
    connections: usize,
}

#[derive(serde::Serialize, schemars::JsonSchema)]
struct Recommendations {
    action_1: String,
    action_2: String,
    action_3: String,
}

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

/// `paths` (graph node paths) as vault-relative paths, sorted. The graph
/// hands back sets in no fixed order, and a list is only safe to page
/// through if every call orders it the same way.
fn relative_paths(manager: &VaultManager, paths: Vec<String>) -> Vec<String> {
    let mut paths: Vec<String> = paths
        .iter()
        .map(|path| manager.relative_path(Path::new(path)))
        .collect();
    paths.sort();
    paths
}

#[turbomcp::server(name = "obsidian-vault", version = env!("CARGO_PKG_VERSION"))]
impl GraphProvider {
    // ==================== Search & Links ====================

    /// Find all notes that link to this note
    #[tool(
        description = "List the notes that link to a note (its backlinks), and the canvases that point at it, as vault-relative paths. Use before renaming or deleting a note to see what depends on it; get_forward_links gives the other direction.",
        title = "Get Backlinks",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<String>>,
    )]
    async fn get_backlinks(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description("Most backlinks to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many backlinks to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = SearchTools::new(manager.clone());
        let mut backlinks = tools.find_backlinks(&path).await.map_err(to_mcp_error)?;
        // Canvases are not in the link graph; the ones that point at the
        // note are its backlinks too.
        backlinks.extend(
            turbovault_tools::CanvasTools::new(manager.clone())
                .canvases_referencing(&path)
                .await
                .map_err(to_mcp_error)?,
        );
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (backlinks, page) = request.slice(relative_paths(&manager, backlinks));

        let response = StandardResponse::new(vault_name, "get_backlinks", backlinks)
            .with_next_step("get_forward_links")
            .with_next_step("get_related_notes");

        let response = if page.total == Some(0) {
            response.with_warning("Note has no incoming links".to_string())
        } else {
            response
        };

        response.with_page(page).to_json()
    }

    /// Find all notes that this note links to
    #[tool(
        description = "List the notes a note links to (its outgoing links that resolve to notes), as vault-relative paths. Broken links are not included; get_broken_links lists those.",
        title = "Get Forward Links",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<String>>,
    )]
    async fn get_forward_links(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description("Most links to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many links to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = SearchTools::new(manager.clone());
        let links = tools
            .find_forward_links(&path)
            .await
            .map_err(to_mcp_error)?;
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (links, page) = request.slice(relative_paths(&manager, links));

        let response = StandardResponse::new(vault_name, "get_forward_links", links)
            .with_page(page)
            .with_next_step("get_backlinks")
            .with_next_step("get_related_notes");

        response.to_json()
    }

    /// Find related notes (by link proximity)
    #[tool(
        description = "List the notes within a number of link hops of a note, following links in either direction. Use it to gather a note's neighborhood; recommend_related finds notes with similar content instead.",
        title = "Get Related Notes",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<String>>,
    )]
    async fn get_related_notes(
        &self,
        #[description(
            "Vault-relative path of the note to start from, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description(
            "How many link hops to follow. Default 2, maximum 5; each hop can multiply the result."
        )]
        max_hops: Option<usize>,
        #[description("Most notes to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many notes to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = SearchTools::new(manager.clone());
        let max_hops = max_hops.unwrap_or(2).min(5); // Cap at 5 hops to prevent runaway traversal
        let related = tools
            .find_related_notes(&path, max_hops)
            .await
            .map_err(to_mcp_error)?;
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (related, page) = request.slice(relative_paths(&manager, related));

        let response = StandardResponse::new(vault_name, "get_related_notes", related)
            .with_page(page)
            .with_meta("max_hops", serde_json::json!(max_hops));

        response.to_json()
    }

    // ==================== Analysis ====================

    /// Find hub notes (highly connected)
    #[tool(
        description = "Rank notes by how many links they have, incoming plus outgoing, most connected first. Each item is `[path, link_count]`. Hubs are usually index pages or central concepts, a good place to start reading.",
        title = "Get Hub Notes",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<(String, usize)>>,
    )]
    async fn get_hub_notes(
        &self,
        #[description("How many hubs to return. Default 10, maximum 500.")] top_n: Option<usize>,
        #[description(
            "How many top hubs to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let request = PageRequest::new(offset, top_n, 10, MAX_REPORT_ITEMS);
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager.clone());
        let mut hubs = tools
            .get_hub_notes(usize::MAX)
            .await
            .map_err(to_mcp_error)?
            .into_iter()
            .map(|(path, count)| (manager.relative_path(Path::new(&path)), count))
            .collect::<Vec<_>>();
        hubs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let (hubs, page) = request.slice(hubs);

        let response = StandardResponse::new(vault_name, "get_hub_notes", hubs)
            .with_page(page)
            .with_next_step("get_related_notes");

        response.to_json()
    }

    /// Find dead-end notes (incoming but no outgoing)
    #[tool(
        description = "List notes that other notes link to but that link to nothing themselves. These are often unfinished notes or final topics that could point onward.",
        title = "Get Dead-End Notes",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<String>>,
    )]
    async fn get_dead_end_notes(
        &self,
        #[description("Most notes to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many notes to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager.clone());
        let dead_ends = tools.get_dead_end_notes().await.map_err(to_mcp_error)?;
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (dead_ends, page) = request.slice(relative_paths(&manager, dead_ends));

        let response =
            StandardResponse::new(vault_name, "get_dead_end_notes", dead_ends).with_page(page);

        response.to_json()
    }

    /// Find isolated clusters in vault
    #[tool(
        description = "List groups of notes that link among themselves but have no link to the rest of the vault. Each item is an array of paths. They often mark separate projects or content that was never connected.",
        title = "Get Isolated Clusters",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<Vec<String>>>,
    )]
    async fn get_isolated_clusters(
        &self,
        #[description("Most clusters to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many clusters to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager.clone());
        let mut clusters = tools
            .get_isolated_clusters()
            .await
            .map_err(to_mcp_error)?
            .into_iter()
            .map(|cluster| relative_paths(&manager, cluster))
            .collect::<Vec<_>>();
        clusters.sort();
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (clusters, page) = request.slice(clusters);

        let response =
            StandardResponse::new(vault_name, "get_isolated_clusters", clusters).with_page(page);

        response.to_json()
    }

    // ==================== Health & Validation ====================

    /// Quick health check (0-100 score)
    #[tool(
        description = "Score the vault's link health from 0 to 100, with counts of notes, links, broken links and orphans. Fast; use it first, then full_health_analysis or get_broken_links for detail.",
        title = "Quick Health Check",
        tags = ["read", "health"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<HealthInfo>,
    )]
    async fn quick_health_check(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let health = tools.quick_health_check().await.map_err(to_mcp_error)?;

        let response = StandardResponse::new(vault_name, "quick_health_check", health.clone())
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
        description = "Full link-health report: the quick check's counts plus dead-end and hub counts. Slower than quick_health_check on a large vault; the lists themselves come from get_broken_links, get_dead_end_notes and get_hub_notes.",
        title = "Full Health Analysis",
        tags = ["read", "health"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<HealthInfo>,
    )]
    async fn full_health_analysis(&self) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let health = tools.full_health_analysis().await.map_err(to_mcp_error)?;

        let mut response =
            StandardResponse::new(vault_name, "full_health_analysis", health.clone());

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
        description = "List links that point to notes that don't exist: the linking note, the target as written, its line, and existing notes with similar names. Fix one by editing the link or creating the missing note.",
        title = "Get Broken Links",
        tags = ["read", "health"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<BrokenLinkInfo>>,
    )]
    async fn get_broken_links(
        &self,
        #[description("Most broken links to return. Default 50, maximum 500.")] limit: Option<
            usize,
        >,
        #[description(
            "How many broken links to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager.clone());
        let mut broken = tools.get_broken_links().await.map_err(to_mcp_error)?;
        for link in &mut broken {
            link.source_file = manager.relative_path(Path::new(&link.source_file));
        }
        broken.sort_by(|a, b| {
            (&a.source_file, a.line, &a.target).cmp(&(&b.source_file, b.line, &b.target))
        });
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (broken, page) = request.slice(broken);

        let response = StandardResponse::new(vault_name, "get_broken_links", broken);

        let response = match page.total {
            Some(total) if total > 0 => response
                .with_warning(format!("Found {} broken links", total))
                .with_next_step("export_broken_links"),
            _ => response,
        };

        response.with_page(page).to_json()
    }

    /// Detect cycles in link graph
    #[tool(
        description = "List chains of notes that link around in a circle back to where they started. Each item is an array of paths. Cycles are normal in a wiki; this is for spotting loops in hierarchies that should not have them.",
        title = "Detect Cycles",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<Vec<String>>>,
    )]
    async fn detect_cycles(
        &self,
        #[description("Most cycles to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many cycles to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager.clone());
        // A cycle's paths stay in link order; only the list of cycles is
        // sorted, so pages are stable.
        let mut cycles = tools
            .detect_cycles()
            .await
            .map_err(to_mcp_error)?
            .into_iter()
            .map(|cycle| {
                cycle
                    .iter()
                    .map(|path| manager.relative_path(Path::new(path)))
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        cycles.sort();
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (cycles, page) = request.slice(cycles);

        let response = StandardResponse::new(vault_name, "detect_cycles", cycles);

        let response = if page.total.is_some_and(|total| total > 0) {
            response
                .with_warning("Cycles detected in link graph".to_string())
                .with_next_step("get_broken_links")
        } else {
            response
        };

        response.with_page(page).to_json()
    }

    /// **HOLISTIC VAULT OVERVIEW** - Complete gestalt view for LLMs (FIX 7: Single call replaces 5+ separate calls)
    /// Provides all essential vault structure info at once: organization, health, hubs, orphans, recommendations
    #[tool(
        description = "One-call overview of the vault: note and link counts, health score, folders with note counts, top hubs and dead ends, and suggested next actions. Use it to orient in an unfamiliar vault; get_vault_context is lighter.",
        title = "Explain Vault",
        tags = ["read"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<ExplainVaultPayload>,
    )]
    async fn explain_vault(
        &self,
        #[description(
            "`detailed` (default) or `concise`. Concise leaves out `structure` (every folder and its note count, which grows with the vault) and `meta.alternatives`."
        )]
        response_format: Option<ResponseFormat>,
    ) -> McpResult<serde_json::Value> {
        let concise = ResponseFormat::is_concise(response_format);
        let (vault_name, manager) = self.get_vault_pair().await?;
        let graph_tools = GraphTools::new(manager.clone());
        let analysis_tools = AnalysisTools::new(manager.clone());

        // Get all data efficiently (parallelizable)
        let files = manager.scan_vault().await.map_err(to_mcp_error)?;
        let (health, hubs, dead_ends) = graph_tools.overview(10).await.map_err(to_mcp_error)?;
        let stats = analysis_tools
            .get_vault_stats()
            .await
            .map_err(to_mcp_error)?;

        // Organize files by folder
        let mut folders: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for file in &files {
            if turbovault_core::is_note(file) {
                let file_str = manager.relative_path(file);
                // `file_str` is already `/`-separated (manager.relative_path),
                // so `parent` is too. Windows path parsing accepts `/` as a
                // separator, and `Path::parent` slices rather than
                // re-renders, so no further conversion is needed here.
                let folder = std::path::Path::new(&file_str)
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .map_or_else(
                        || "root".to_string(),
                        |parent| parent.to_string_lossy().into_owned(),
                    );
                folders.entry(folder).or_default().push(file_str);
            }
        }

        // Create holistic overview
        let overview = ExplainVaultPayload {
            vault_name: vault_name.clone(),
            quick_facts: QuickFacts {
                total_files: stats.total_files,
                total_links: stats.total_links,
                orphaned: stats.orphaned_files,
                health_score: health.health_score,
                is_healthy: health.is_healthy,
            },
            structure: (!concise).then(|| VaultStructure {
                folders: folders.keys().cloned().collect(),
                file_count_by_folder: folders
                    .iter()
                    .map(|(folder, files)| (folder.clone(), files.len()))
                    .collect(),
            }),
            key_insights: KeyInsights {
                hub_notes: hubs
                    .iter()
                    .take(5)
                    .map(|(path, count)| HubConnections {
                        path: manager.relative_path(std::path::Path::new(path)),
                        connections: *count,
                    })
                    .collect(),
                dead_ends: dead_ends
                    .iter()
                    .take(5)
                    .map(|path| manager.relative_path(std::path::Path::new(path)))
                    .collect(),
                average_links_per_file: stats.average_links_per_file,
            },
            recommendations: Recommendations {
                action_1: if stats.orphaned_files > 0 {
                    format!(
                        "Link {} orphaned notes to main index or other hub notes",
                        stats.orphaned_files
                    )
                } else {
                    "Vault is well-connected".to_string()
                },
                action_2: if health.broken_links_count > 0 {
                    format!(
                        "Fix {} broken links (use get_broken_links for details)",
                        health.broken_links_count
                    )
                } else {
                    "No broken links".to_string()
                },
                action_3: if hubs.len() > 3 {
                    "Create hub pages for your top 3-5 topics".to_string()
                } else {
                    "Consider creating more cross-linking between topics".to_string()
                },
            },
        };

        let mut response = StandardResponse::new(vault_name, "explain_vault", overview).with_meta(
            "view_type".to_string(),
            serde_json::json!("holistic_gestalt"),
        );
        if !concise {
            response = response.with_meta(
                "alternatives".to_string(),
                serde_json::json!([
                    "search() - Find notes by keyword",
                    "get_hub_notes() - See most connected notes",
                    "full_health_analysis() - Detailed health report",
                    "query_metadata() - Search by frontmatter"
                ]),
            );
        }
        let response = response.with_next_steps(&[
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
