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

/// Which links `get_links` lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum LinkDirection {
    /// Notes (and canvases) that link to the note: its backlinks.
    #[default]
    Incoming,
    /// Notes the note links to.
    Outgoing,
}

/// Which report `graph_report` builds.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GraphReportKind {
    Hubs,
    Centrality,
    DeadEnds,
    IsolatedClusters,
    Cycles,
    BrokenLinks,
}

/// What `graph_report` returns: the kind it was asked for, and one page of
/// that kind's items.
#[derive(serde::Serialize, schemars::JsonSchema)]
struct GraphReport {
    kind: GraphReportKind,
    items: GraphReportItems,
}

/// The items of one report kind. `hubs` and `centrality` are ranked, most
/// connected or most central first; the rest are in path order.
#[derive(serde::Serialize, schemars::JsonSchema)]
#[serde(untagged)]
enum GraphReportItems {
    /// `hubs`.
    Hubs(Vec<HubConnections>),
    /// `centrality`.
    Centrality(Vec<CentralityRank>),
    /// `dead_ends`.
    Paths(Vec<String>),
    /// `isolated_clusters` and `cycles`: each item is a group of paths. A
    /// cycle's paths are in link order.
    Groups(Vec<Vec<String>>),
    /// `broken_links`.
    BrokenLinks(Vec<BrokenLinkInfo>),
}

#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
struct CentralityRank {
    rank: usize,
    file: String,
    score: f64,
    betweenness: f64,
    closeness: f64,
    eigenvector: f64,
    interpretation: String,
}

/// How thorough `vault_health` is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub(crate) enum HealthDepth {
    /// The score and the counts of notes, links, broken links and orphans.
    #[default]
    Quick,
    /// The quick counts plus dead-end and hub counts; slower on a large vault.
    Full,
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
    // ==================== Links ====================

    /// List a note's links in one direction.
    #[tool(
        description = "List a note's links as vault-relative paths. `incoming` (the default) is its backlinks: the notes and canvases that link to it; check them before renaming or deleting a note. `outgoing` is the notes it links to; broken links are not included (graph_report with kind `broken_links` lists those).",
        title = "Get Links",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<Vec<String>>,
    )]
    async fn get_links(
        &self,
        #[description(
            "Vault-relative path of the note, including the extension, such as `projects/Plan.md`."
        )]
        path: String,
        #[description(
            "`incoming` (default) for the notes and canvases linking to the note, `outgoing` for the notes it links to."
        )]
        direction: Option<LinkDirection>,
        #[description("Most links to return. Default 50, maximum 500.")] limit: Option<usize>,
        #[description(
            "How many links to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let direction = direction.unwrap_or_default();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = SearchTools::new(manager.clone());
        let links = match direction {
            LinkDirection::Incoming => {
                let mut backlinks = tools.find_backlinks(&path).await.map_err(to_mcp_error)?;
                // Canvases are not in the link graph; the ones that point at
                // the note are its backlinks too.
                backlinks.extend(
                    turbovault_tools::CanvasTools::new(manager.clone())
                        .canvases_referencing(&path)
                        .await
                        .map_err(to_mcp_error)?,
                );
                backlinks
            }
            LinkDirection::Outgoing => tools
                .find_forward_links(&path)
                .await
                .map_err(to_mcp_error)?,
        };
        let request = PageRequest::new(offset, limit, DEFAULT_PAGE_SIZE, MAX_REPORT_ITEMS);
        let (links, page) = request.slice(relative_paths(&manager, links));

        let mut response = StandardResponse::new(vault_name, "get_links", links)
            .with_meta(
                "direction",
                serde_json::json!(match direction {
                    LinkDirection::Incoming => "incoming",
                    LinkDirection::Outgoing => "outgoing",
                }),
            )
            .with_next_step("get_related_notes");
        if page.total == Some(0) && direction == LinkDirection::Incoming {
            response = response.with_warning("Note has no incoming links".to_string());
        }
        response.with_page(page).to_json()
    }

    /// Find related notes (by link proximity)
    #[tool(
        description = "List the notes within a number of link hops of a note, following links in either direction. Use it to gather a note's neighborhood; search with `related_to` finds notes with similar content instead.",
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

    // ==================== Reports ====================

    /// One list about the vault's link graph.
    #[tool(
        description = "List one thing about the vault's link graph, chosen by `kind`. `hubs`: notes ranked by link count, a good place to start reading. `centrality`: notes ranked by how central they are (betweenness, closeness, eigenvector). `dead_ends`: notes that are linked to but link nowhere. `isolated_clusters`: groups of notes linked only among themselves. `cycles`: chains of notes linking back to where they started. `broken_links`: links to notes that don't exist, with the linking note, the target as written, its line and similarly named notes. vault_health gives the counts behind all of these.",
        title = "Graph Report",
        tags = ["read", "graph"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = Paged<GraphReport>,
    )]
    async fn graph_report(
        &self,
        #[description(
            "Which list: `hubs`, `centrality`, `dead_ends`, `isolated_clusters`, `cycles` or `broken_links`."
        )]
        kind: GraphReportKind,
        #[description("Most items to return. Default 10 for `hubs`, 50 otherwise; maximum 500.")]
        limit: Option<usize>,
        #[description(
            "How many items to skip, for the next page. Default 0; use `meta.pagination.next_offset` from the previous call."
        )]
        offset: Option<usize>,
    ) -> McpResult<serde_json::Value> {
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager.clone());
        let default = if kind == GraphReportKind::Hubs {
            10
        } else {
            DEFAULT_PAGE_SIZE
        };
        let request = PageRequest::new(offset, limit, default, MAX_REPORT_ITEMS);
        let mut warnings = Vec::new();
        let mut next_steps: Vec<&str> = Vec::new();

        let (items, page) = match kind {
            GraphReportKind::Hubs => {
                let mut hubs = tools
                    .get_hub_notes(usize::MAX)
                    .await
                    .map_err(to_mcp_error)?
                    .into_iter()
                    .map(|(path, connections)| HubConnections {
                        path: manager.relative_path(Path::new(&path)),
                        connections,
                    })
                    .collect::<Vec<_>>();
                hubs.sort_by(|a, b| {
                    b.connections
                        .cmp(&a.connections)
                        .then_with(|| a.path.cmp(&b.path))
                });
                next_steps.push("get_related_notes");
                let (hubs, page) = request.slice(hubs);
                (GraphReportItems::Hubs(hubs), page)
            }
            GraphReportKind::Centrality => {
                let ranking = RelationshipTools::new(manager.clone())
                    .get_centrality_ranking()
                    .await
                    .map_err(to_mcp_error)?;
                let rankings: Vec<CentralityRank> = ranking
                    .get("rankings")
                    .cloned()
                    .map(serde_json::from_value)
                    .transpose()
                    .map_err(|error| McpError::internal(error.to_string()))?
                    .unwrap_or_default();
                let (rankings, page) = request.slice(rankings);
                (GraphReportItems::Centrality(rankings), page)
            }
            GraphReportKind::DeadEnds => {
                let dead_ends = tools.get_dead_end_notes().await.map_err(to_mcp_error)?;
                let (dead_ends, page) = request.slice(relative_paths(&manager, dead_ends));
                (GraphReportItems::Paths(dead_ends), page)
            }
            GraphReportKind::IsolatedClusters => {
                let mut clusters = tools
                    .get_isolated_clusters()
                    .await
                    .map_err(to_mcp_error)?
                    .into_iter()
                    .map(|cluster| relative_paths(&manager, cluster))
                    .collect::<Vec<_>>();
                clusters.sort();
                let (clusters, page) = request.slice(clusters);
                (GraphReportItems::Groups(clusters), page)
            }
            GraphReportKind::Cycles => {
                // A cycle's paths stay in link order; only the list of
                // cycles is sorted, so pages are stable.
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
                let (cycles, page) = request.slice(cycles);
                if page.total.is_some_and(|total| total > 0) {
                    warnings.push("Cycles detected in link graph".to_string());
                }
                (GraphReportItems::Groups(cycles), page)
            }
            GraphReportKind::BrokenLinks => {
                let mut broken = tools.get_broken_links().await.map_err(to_mcp_error)?;
                for link in &mut broken {
                    link.source_file = manager.relative_path(Path::new(&link.source_file));
                }
                broken.sort_by(|a, b| {
                    (&a.source_file, a.line, &a.target).cmp(&(&b.source_file, b.line, &b.target))
                });
                let (broken, page) = request.slice(broken);
                if let Some(total) = page.total.filter(|total| *total > 0) {
                    warnings.push(format!("Found {total} broken links"));
                    next_steps.push("export");
                }
                (GraphReportItems::BrokenLinks(broken), page)
            }
        };

        let mut response =
            StandardResponse::new(vault_name, "graph_report", GraphReport { kind, items })
                .with_next_steps(&next_steps);
        for warning in warnings {
            response = response.with_warning(warning);
        }
        response.with_page(page).to_json()
    }

    /// The vault's link health.
    #[tool(
        description = "Score the vault's link health from 0 to 100, with counts of notes, links, broken links and orphans. `depth: full` adds dead-end and hub counts and is slower on a large vault. Start here; graph_report lists the notes behind each count.",
        title = "Vault Health",
        tags = ["read", "health"],
        read_only = true,
        destructive = false,
        idempotent = true,
        open_world = false,
        output_schema = StandardResponse<HealthInfo>,
    )]
    async fn vault_health(
        &self,
        #[description(
            "`quick` (default) for the score and core counts, `full` to add dead-end and hub counts."
        )]
        depth: Option<HealthDepth>,
        ctx: &RequestContext,
    ) -> McpResult<serde_json::Value> {
        let depth = depth.unwrap_or_default();
        let (vault_name, manager) = self.get_vault_pair().await?;
        let tools = GraphTools::new(manager);
        let health = match depth {
            HealthDepth::Quick => tools.quick_health_check().await.map_err(to_mcp_error)?,
            HealthDepth::Full => {
                let mut stages = Stages::new(ctx, 1);
                stages.next("Analyzing the link graph").await?;
                let health = tools.full_health_analysis().await.map_err(to_mcp_error)?;
                stages.finish().await;
                health
            }
        };

        let mut response = StandardResponse::new(vault_name, "vault_health", health.clone())
            .with_meta(
                "depth",
                serde_json::json!(match depth {
                    HealthDepth::Quick => "quick",
                    HealthDepth::Full => "full",
                }),
            );
        if health.broken_links_count > 0 || health.orphaned_notes_count > 0 {
            response = response.with_next_step("graph_report");
        }
        if health.orphaned_notes_count > 0 {
            response = response.with_next_step("suggest_links");
        }
        response.to_json()
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
        ctx: &RequestContext,
    ) -> McpResult<serde_json::Value> {
        let concise = ResponseFormat::is_concise(response_format);
        let mut stages = Stages::new(ctx, 3);
        let (vault_name, manager) = self.get_vault_pair().await?;
        let graph_tools = GraphTools::new(manager.clone());
        let analysis_tools = AnalysisTools::new(manager.clone());

        stages.next("Listing the vault's notes").await?;
        let files = manager.scan_vault().await.map_err(to_mcp_error)?;
        stages.next("Analyzing links, hubs and dead ends").await?;
        let (health, hubs, dead_ends) = graph_tools.overview(10).await.map_err(to_mcp_error)?;
        stages.next("Counting notes and links").await?;
        let stats = analysis_tools
            .get_vault_stats()
            .await
            .map_err(to_mcp_error)?;
        stages.finish().await;

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
                        "Fix {} broken links (graph_report with kind `broken_links` lists them)",
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
                    "search() - Find notes by keyword, tag or frontmatter",
                    "graph_report(kind=hubs) - See most connected notes",
                    "vault_health(depth=full) - Detailed health counts",
                ]),
            );
        }
        let response = response.with_next_steps(&["graph_report", "search"]);

        response.to_json()
    }
}
