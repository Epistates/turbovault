//! Graph operations and link analysis tools

use serde::{Deserialize, Serialize};
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

    /// Get detailed broken links information
    pub async fn get_broken_links(&self) -> Result<Vec<BrokenLinkInfo>> {
        let graph_lock = self.manager.link_graph_flushed().await;
        let graph = graph_lock.read().await;
        let unresolved = graph.all_unresolved_links();
        let analyzer = HealthAnalyzer::with_files(&graph, unresolved);

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
    }

    /// Run quick health check
    pub async fn quick_health_check(&self) -> Result<HealthInfo> {
        let graph_lock = self.manager.link_graph_flushed().await;
        let graph = graph_lock.read().await;
        let unresolved = graph.all_unresolved_links();
        let analyzer = HealthAnalyzer::with_files(&graph, unresolved);
        Ok(quick_health(&analyzer, &graph))
    }

    /// Run comprehensive health analysis
    pub async fn full_health_analysis(&self) -> Result<HealthInfo> {
        let graph_lock = self.manager.link_graph_flushed().await;
        let graph = graph_lock.read().await;
        let unresolved = graph.all_unresolved_links();
        let analyzer = HealthAnalyzer::with_files(&graph, unresolved);

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
    }

    /// Get hub notes (highly connected nodes)
    pub async fn get_hub_notes(&self, limit: usize) -> Result<Vec<(String, usize)>> {
        let graph_lock = self.manager.link_graph_flushed().await;
        let graph = graph_lock.read().await;
        Ok(hub_notes(&HealthAnalyzer::new(&graph), limit))
    }

    /// Get dead-end notes (no outgoing links but have incoming)
    pub async fn get_dead_end_notes(&self) -> Result<Vec<String>> {
        let graph_lock = self.manager.link_graph_flushed().await;
        let graph = graph_lock.read().await;
        Ok(dead_end_notes(&HealthAnalyzer::new(&graph)))
    }

    /// The quick health check, the `limit` most connected notes and the
    /// dead ends, read under one hold of the graph, for a vault overview.
    pub async fn overview(
        &self,
        hub_limit: usize,
    ) -> Result<(HealthInfo, Vec<(String, usize)>, Vec<String>)> {
        let graph_lock = self.manager.link_graph_flushed().await;
        let graph = graph_lock.read().await;
        let analyzer = HealthAnalyzer::with_files(&graph, graph.all_unresolved_links());
        Ok((
            quick_health(&analyzer, &graph),
            hub_notes(&analyzer, hub_limit),
            dead_end_notes(&analyzer),
        ))
    }

    /// Detect cycles in the graph
    pub async fn detect_cycles(&self) -> Result<Vec<Vec<String>>> {
        let graph_lock = self.manager.link_graph_flushed().await;
        let graph = graph_lock.read().await;
        let cycles = graph.cycles();

        Ok(cycles
            .into_iter()
            .map(|cycle| {
                cycle
                    .into_iter()
                    .map(|p| turbovault_core::path_to_slash(&p))
                    .collect()
            })
            .collect())
    }

    /// Get connected components
    pub async fn get_connected_components(&self) -> Result<Vec<Vec<String>>> {
        let graph_lock = self.manager.link_graph_flushed().await;
        let graph = graph_lock.read().await;
        let components = graph.connected_components()?;

        Ok(components
            .into_iter()
            .map(|component| {
                component
                    .into_iter()
                    .map(|p| turbovault_core::path_to_slash(&p))
                    .collect()
            })
            .collect())
    }

    /// Get isolated clusters (small disconnected groups)
    pub async fn get_isolated_clusters(&self) -> Result<Vec<Vec<String>>> {
        let graph_lock = self.manager.link_graph_flushed().await;
        let graph = graph_lock.read().await;
        let analyzer = HealthAnalyzer::new(&graph);

        Ok(analyzer
            .isolated_clusters()?
            .into_iter()
            .map(|cluster| {
                cluster
                    .into_iter()
                    .map(|p| turbovault_core::path_to_slash(&p))
                    .collect()
            })
            .collect())
    }
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
