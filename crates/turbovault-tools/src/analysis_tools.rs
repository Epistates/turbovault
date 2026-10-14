//! Vault statistics. Graph analyses (orphans, cycles, hubs, health) live in
//! [`crate::graph_tools::GraphTools`].

use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

/// Analysis tools context
pub struct AnalysisTools {
    /// The vault the statistics are taken from.
    pub manager: Arc<VaultManager>,
}

/// Statistics response structure
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct VaultStats {
    /// Notes in the vault.
    pub total_files: usize,
    /// Links between notes.
    pub total_links: usize,
    /// Notes with no links in or out.
    pub orphaned_files: usize,
    /// `total_links / total_files`.
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
}
