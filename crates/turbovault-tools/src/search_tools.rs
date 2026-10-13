//! Search and relationship discovery tools

use std::path::PathBuf;
use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

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
            .map(|p| turbovault_core::path_to_slash(&p))
            .collect())
    }

    /// Find all forward links from a file
    pub async fn find_forward_links(&self, path: &str) -> Result<Vec<String>> {
        let file_path = PathBuf::from(path);
        let forward_links = self.manager.get_forward_links(&file_path).await?;

        Ok(forward_links
            .into_iter()
            .map(|p| turbovault_core::path_to_slash(&p))
            .collect())
    }

    /// Find related notes within N hops
    pub async fn find_related_notes(&self, path: &str, max_hops: usize) -> Result<Vec<String>> {
        let file_path = PathBuf::from(path);
        let related = self.manager.get_related_notes(&file_path, max_hops).await?;

        Ok(related
            .into_iter()
            .map(|p| turbovault_core::path_to_slash(&p))
            .collect())
    }

    /// Search for files by name pattern (simple substring match).
    ///
    /// Only the files the vault scan sees are searched: protected and
    /// excluded directories are skipped and symlinks are not followed, so
    /// this can't list what lies in `.git` or outside the vault.
    pub async fn search_files(&self, pattern: &str) -> Result<Vec<String>> {
        Ok(self
            .manager
            .scan_vault()
            .await?
            .into_iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(pattern))
            })
            .map(|path| self.manager.relative_path(&path))
            .collect())
    }
}
