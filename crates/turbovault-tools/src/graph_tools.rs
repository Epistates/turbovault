//! Graph operations and link analysis tools
//!
//! Every analysis runs over the link graph on the blocking pool, after the
//! freshness gate, so walking a large vault never ties up an async worker.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_graph::{AnalysisConfig, HealthAnalyzer, HealthCounts, HealthReport, LinkGraph};
use turbovault_vault::VaultManager;

/// Graph tools context
pub struct GraphTools {
    pub manager: Arc<VaultManager>,
}

/// Simplified broken link for JSON serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct BrokenLinkInfo {
    pub source_file: String,
    pub target: String,
    pub line: usize,
    pub suggestions: Vec<String>,
}

/// Simplified health report for JSON serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct HealthInfo {
    pub total_notes: usize,
    pub total_links: usize,
    pub broken_links_count: usize,
    pub orphaned_notes_count: usize,
    pub dead_end_notes_count: usize,
    pub hub_notes_count: usize,
    pub health_score: u8,
    pub is_healthy: bool,
}

impl GraphTools {
    /// Create new graph tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Run `analysis` over the link graph (after the freshness gate) on the
    /// blocking pool.
    async fn with_graph<T, F>(&self, analysis: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&LinkGraph) -> Result<T> + Send + 'static,
    {
        let graph = self.manager.link_graph_flushed().await;
        tokio::task::spawn_blocking(move || analysis(&graph.blocking_read()))
            .await
            .map_err(|e| Error::other(format!("graph analysis task failed: {e}")))?
    }

    /// Get detailed broken links information
    pub async fn get_broken_links(&self) -> Result<Vec<BrokenLinkInfo>> {
        self.with_graph(|graph| {
            let analyzer = HealthAnalyzer::with_files(graph, graph.all_unresolved_links());
            Ok(analyzer
                .broken_links()?
                .into_iter()
                .map(|bl| BrokenLinkInfo {
                    source_file: turbovault_core::path_to_slash(&bl.source_file),
                    target: bl.target,
                    line: bl.line,
                    suggestions: bl.suggestions,
                })
                .collect())
        })
        .await
    }

    /// Run quick health check
    pub async fn quick_health_check(&self) -> Result<HealthInfo> {
        self.with_graph(|graph| {
            let analyzer = HealthAnalyzer::with_files(graph, graph.all_unresolved_links());
            Ok(quick_health(&analyzer, graph))
        })
        .await
    }

    /// Run comprehensive health analysis
    pub async fn full_health_analysis(&self) -> Result<HealthInfo> {
        self.with_graph(|graph| {
            let analyzer = HealthAnalyzer::with_files(graph, graph.all_unresolved_links());

            // Counts only: the full analysis would also list every issue and
            // suggest fixes for every broken link, none of which this reports.
            let counts = analyzer.counts()?;
            let health_score = HealthReport::score(&counts);
            Ok(HealthInfo {
                total_notes: counts.total_notes,
                total_links: counts.total_links,
                broken_links_count: counts.broken_links,
                orphaned_notes_count: counts.orphaned_notes,
                dead_end_notes_count: counts.dead_end_notes,
                hub_notes_count: analyzer
                    .hub_notes(AnalysisConfig::default().hub_notes_limit)
                    .len(),
                health_score,
                is_healthy: health_score >= 80,
            })
        })
        .await
    }

    /// Get hub notes (highly connected nodes)
    pub async fn get_hub_notes(&self, limit: usize) -> Result<Vec<(String, usize)>> {
        self.with_graph(move |graph| Ok(hub_notes(&HealthAnalyzer::new(graph), limit)))
            .await
    }

    /// Get dead-end notes (no outgoing links but have incoming)
    pub async fn get_dead_end_notes(&self) -> Result<Vec<String>> {
        self.with_graph(|graph| Ok(dead_end_notes(&HealthAnalyzer::new(graph))))
            .await
    }

    /// The quick health check, the `limit` most connected notes and the
    /// dead ends, read under one hold of the graph, for a vault overview.
    pub async fn overview(
        &self,
        hub_limit: usize,
    ) -> Result<(HealthInfo, Vec<(String, usize)>, Vec<String>)> {
        self.with_graph(move |graph| {
            let analyzer = HealthAnalyzer::with_files(graph, graph.all_unresolved_links());
            Ok((
                quick_health(&analyzer, graph),
                hub_notes(&analyzer, hub_limit),
                dead_end_notes(&analyzer),
            ))
        })
        .await
    }

    /// Detect cycles in the graph
    pub async fn detect_cycles(&self) -> Result<Vec<Vec<String>>> {
        self.with_graph(|graph| Ok(slash_paths(graph.cycles())))
            .await
    }

    /// Get connected components
    pub async fn get_connected_components(&self) -> Result<Vec<Vec<String>>> {
        self.with_graph(|graph| Ok(slash_paths(graph.connected_components()?)))
            .await
    }

    /// Get isolated clusters (small disconnected groups)
    pub async fn get_isolated_clusters(&self) -> Result<Vec<Vec<String>>> {
        self.with_graph(|graph| Ok(slash_paths(HealthAnalyzer::new(graph).isolated_clusters()?)))
            .await
    }
}

/// Groups of paths, each written with `/` separators.
fn slash_paths(groups: Vec<Vec<PathBuf>>) -> Vec<Vec<String>> {
    groups
        .into_iter()
        .map(|group| {
            group
                .into_iter()
                .map(|p| turbovault_core::path_to_slash(&p))
                .collect()
        })
        .collect()
}

/// The quick health check: broken links and orphans, scored on those alone.
fn quick_health(analyzer: &HealthAnalyzer<'_>, graph: &LinkGraph) -> HealthInfo {
    let counts = HealthCounts {
        total_notes: graph.node_count(),
        total_links: graph.edge_count(),
        broken_links: analyzer.broken_link_count(),
        orphaned_notes: graph.orphaned_notes().len(),
        ..HealthCounts::default()
    };
    let health_score = HealthReport::score(&counts);
    HealthInfo {
        total_notes: counts.total_notes,
        total_links: counts.total_links,
        broken_links_count: counts.broken_links,
        orphaned_notes_count: counts.orphaned_notes,
        dead_end_notes_count: 0,
        hub_notes_count: 0,
        health_score,
        is_healthy: health_score >= 80,
    }
}

fn hub_notes(analyzer: &HealthAnalyzer<'_>, limit: usize) -> Vec<(String, usize)> {
    analyzer
        .hub_notes(limit)
        .into_iter()
        .map(|(path, count)| (turbovault_core::path_to_slash(&path), count))
        .collect()
}

fn dead_end_notes(analyzer: &HealthAnalyzer<'_>) -> Vec<String> {
    analyzer
        .dead_end_notes()
        .into_iter()
        .map(|p| turbovault_core::path_to_slash(&p))
        .collect()
}
