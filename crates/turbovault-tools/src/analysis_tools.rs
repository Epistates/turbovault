//! Vault analysis tools for statistics and relationship analysis

use serde_json::json;
use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

/// Analysis tools context
pub struct AnalysisTools {
    pub manager: Arc<VaultManager>,
}

/// Statistics response structure
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VaultStats {
    pub total_files: usize,
    pub total_links: usize,
    pub orphaned_files: usize,
    pub average_links_per_file: f64,
}

impl AnalysisTools {
    /// Create new analysis tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Get vault statistics
    pub async fn get_vault_stats(&self) -> Result<VaultStats> {
        let stats = self.manager.get_stats().await?;

        Ok(VaultStats {
            total_files: stats.total_files,
            total_links: stats.total_links,
            orphaned_files: stats.orphaned_files,
            average_links_per_file: stats.average_links_per_file,
        })
    }

    /// List all orphaned notes (no incoming or outgoing links)
    pub async fn list_orphaned_notes(&self) -> Result<Vec<String>> {
        let orphans = self.manager.get_orphaned_notes().await?;

        Ok(orphans
            .into_iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
            .collect())
    }

    /// Detect cycles (mutual linking patterns)
    pub async fn detect_cycles(&self) -> Result<Vec<Vec<String>>> {
        let graph = self.manager.link_graph();
        let graph_read = graph.read().await;

        let cycles = graph_read
            .cycles()
            .into_iter()
            .map(|cycle| {
                cycle
                    .into_iter()
                    .filter_map(|p| p.to_str().map(|s| s.to_string()))
                    .collect()
            })
            .collect();

        Ok(cycles)
    }

    /// Get link density (total links / possible links)
    pub async fn get_link_density(&self) -> Result<f64> {
        let stats = self.manager.get_stats().await?;

        // Link density = actual_links / possible_links
        // where possible_links = n * (n - 1) for directed graph
        if stats.total_files <= 1 {
            return Ok(0.0);
        }

        let possible_links = (stats.total_files as f64) * ((stats.total_files as f64) - 1.0);
        let density = (stats.total_links as f64) / possible_links;

        Ok(density)
    }

    /// Get connectivity metrics
    pub async fn get_connectivity_metrics(&self) -> Result<serde_json::Value> {
        let stats = self.manager.get_stats().await?;
        let density = self.get_link_density().await?;

        Ok(json!({
            "total_files": stats.total_files,
            "total_links": stats.total_links,
            "orphaned_files": stats.orphaned_files,
            "connected_files": stats.total_files - stats.orphaned_files,
            "average_links_per_file": stats.average_links_per_file,
            "link_density": density,
            "connectivity_rate": if stats.total_files > 0 {
                (stats.total_files - stats.orphaned_files) as f64 / stats.total_files as f64
            } else {
                0.0
            }
        }))
    }
}
