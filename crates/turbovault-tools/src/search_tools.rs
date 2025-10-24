//! Search and relationship discovery tools

use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;
use std::path::PathBuf;
use std::sync::Arc;

/// Search tools context
pub struct SearchTools {
    pub manager: Arc<VaultManager>,
}

impl SearchTools {
    /// Create new search tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Find all backlinks to a file
    pub async fn find_backlinks(&self, path: &str) -> Result<Vec<String>> {
        let file_path = PathBuf::from(path);
        let backlinks = self.manager.get_backlinks(&file_path).await?;

        Ok(backlinks
            .into_iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
            .collect())
    }

    /// Find all forward links from a file
    pub async fn find_forward_links(&self, path: &str) -> Result<Vec<String>> {
        let file_path = PathBuf::from(path);
        let forward_links = self.manager.get_forward_links(&file_path).await?;

        Ok(forward_links
            .into_iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
            .collect())
    }

    /// Find related notes within N hops
    pub async fn find_related_notes(&self, path: &str, max_hops: usize) -> Result<Vec<String>> {
        let file_path = PathBuf::from(path);
        let related = self.manager.get_related_notes(&file_path, max_hops).await?;

        Ok(related
            .into_iter()
            .filter_map(|p| p.to_str().map(|s| s.to_string()))
            .collect())
    }

    /// Search for files by name pattern (simple substring match)
    pub async fn search_files(&self, pattern: &str) -> Result<Vec<String>> {
        let vault_path = self.manager.vault_path();
        let mut results = Vec::new();

        // Walk vault directory
        let mut stack = vec![vault_path.clone()];
        while let Some(dir) = stack.pop() {
            if let Ok(entries) = std::fs::read_dir(&dir) {
                for entry in entries.flatten() {
                    let path = entry.path();

                    if path.is_dir() {
                        stack.push(path);
                    } else if let Some(name) = path.file_name().and_then(|n| n.to_str())
                        && name.contains(pattern)
                        && let Ok(rel_path) = path.strip_prefix(vault_path)
                        && let Some(rel_str) = rel_path.to_str()
                    {
                        results.push(rel_str.to_string());
                    }
                }
            }
        }

        Ok(results)
    }
}
