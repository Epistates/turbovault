//! Graph operations and link analysis tools

use turbovault_core::prelude::*;
use turbovault_graph::HealthAnalyzer;
use turbovault_vault::VaultManager;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Graph tools context
pub struct GraphTools {
    pub manager: Arc<VaultManager>,
}

/// Simplified broken link for JSON serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BrokenLinkInfo {
    pub source_file: String,
    pub target: String,
    pub line: usize,
    pub suggestions: Vec<String>,
}

/// Simplified health report for JSON serialization
#[derive(Debug, Clone, Serialize, Deserialize)]
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
        let graph_lock = self.manager.link_graph();
        let graph = graph_lock.read().await;
        let analyzer = HealthAnalyzer::new(&graph);

        let report = analyzer.analyze()?;

        Ok(report
            .broken_links
            .into_iter()
            .map(|bl| BrokenLinkInfo {
                source_file: bl.source_file.to_string_lossy().to_string(),
                target: bl.target,
                line: bl.line,
                suggestions: bl.suggestions,
            })
            .collect())
    }

    /// Run quick health check
    pub async fn quick_health_check(&self) -> Result<HealthInfo> {
        let graph_lock = self.manager.link_graph();
        let graph = graph_lock.read().await;
        let analyzer = HealthAnalyzer::new(&graph);

        let report = analyzer.quick_check()?;

        Ok(HealthInfo {
            total_notes: report.total_notes,
            total_links: report.total_links,
            broken_links_count: report.broken_links.len(),
            orphaned_notes_count: report.orphaned_notes.len(),
            dead_end_notes_count: 0,
            hub_notes_count: 0,
            health_score: report.health_score,
            is_healthy: report.is_healthy(),
        })
    }

    /// Run comprehensive health analysis
    pub async fn full_health_analysis(&self) -> Result<HealthInfo> {
        let graph_lock = self.manager.link_graph();
        let graph = graph_lock.read().await;
        let analyzer = HealthAnalyzer::new(&graph);

        let report = analyzer.analyze()?;

        Ok(HealthInfo {
            total_notes: report.total_notes,
            total_links: report.total_links,
            broken_links_count: report.broken_links.len(),
            orphaned_notes_count: report.orphaned_notes.len(),
            dead_end_notes_count: report.dead_end_notes.len(),
            hub_notes_count: report.hub_notes.len(),
            health_score: report.health_score,
            is_healthy: report.is_healthy(),
        })
    }

    /// Get hub notes (highly connected nodes)
    pub async fn get_hub_notes(&self, limit: usize) -> Result<Vec<(String, usize)>> {
        let graph_lock = self.manager.link_graph();
        let graph = graph_lock.read().await;
        let analyzer = HealthAnalyzer::new(&graph);

        let report = analyzer.analyze()?;

        Ok(report
            .hub_notes
            .into_iter()
            .take(limit)
            .map(|(path, count)| (path.to_string_lossy().to_string(), count))
            .collect())
    }

    /// Get dead-end notes (no outgoing links but have incoming)
    pub async fn get_dead_end_notes(&self) -> Result<Vec<String>> {
        let graph_lock = self.manager.link_graph();
        let graph = graph_lock.read().await;
        let analyzer = HealthAnalyzer::new(&graph);

        let report = analyzer.analyze()?;

        Ok(report
            .dead_end_notes
            .into_iter()
            .map(|p| p.to_string_lossy().to_string())
            .collect())
    }

    /// Detect cycles in the graph
    pub async fn detect_cycles(&self) -> Result<Vec<Vec<String>>> {
        let graph_lock = self.manager.link_graph();
        let graph = graph_lock.read().await;
        let cycles = graph.cycles();

        Ok(cycles
            .into_iter()
            .map(|cycle| {
                cycle
                    .into_iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect()
            })
            .collect())
    }

    /// Get connected components
    pub async fn get_connected_components(&self) -> Result<Vec<Vec<String>>> {
        let graph_lock = self.manager.link_graph();
        let graph = graph_lock.read().await;
        let components = graph.connected_components()?;

        Ok(components
            .into_iter()
            .map(|component| {
                component
                    .into_iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect()
            })
            .collect())
    }

    /// Get isolated clusters (small disconnected groups)
    pub async fn get_isolated_clusters(&self) -> Result<Vec<Vec<String>>> {
        let graph_lock = self.manager.link_graph();
        let graph = graph_lock.read().await;
        let analyzer = HealthAnalyzer::new(&graph);

        let report = analyzer.analyze()?;

        Ok(report
            .isolated_clusters
            .into_iter()
            .map(|cluster| {
                cluster
                    .into_iter()
                    .map(|p| p.to_string_lossy().to_string())
                    .collect()
            })
            .collect())
    }
}
