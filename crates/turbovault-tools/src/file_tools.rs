//! File operation tools for the Obsidian MCP server

use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;
use std::path::PathBuf;
use std::sync::Arc;

/// File tools context
#[derive(Clone)]
pub struct FileTools {
    pub manager: Arc<VaultManager>,
}

impl FileTools {
    /// Create new file tools
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Read a file from the vault
    pub async fn read_file(&self, path: &str) -> Result<String> {
        let file_path = PathBuf::from(path);
        self.manager.read_file(&file_path).await
    }

    /// Write a file to the vault (creates directories as needed)
    pub async fn write_file(&self, path: &str, content: &str) -> Result<()> {
        let file_path = PathBuf::from(path);
        self.manager.write_file(&file_path, content).await
    }

    /// Edit file using SEARCH/REPLACE blocks (LLM-optimized)
    ///
    /// Uses aider-inspired git merge conflict syntax that reduces LLM laziness by 3X.
    /// Provides fuzzy matching to tolerate minor formatting errors from LLMs.
    ///
    /// # Arguments
    /// * `path` - Relative path to file in vault
    /// * `edits` - String containing SEARCH/REPLACE blocks in git merge conflict format
    /// * `expected_hash` - Optional SHA-256 hash from previous read_file call
    /// * `dry_run` - If true, preview changes without applying
    ///
    /// # Returns
    /// EditResult with success status, hashes, and optional diff preview
    pub async fn edit_file(
        &self,
        path: &str,
        edits: &str,
        expected_hash: Option<&str>,
        dry_run: bool,
    ) -> Result<turbovault_vault::EditResult> {
        let file_path = PathBuf::from(path);
        self.manager
            .edit_file(&file_path, edits, expected_hash, dry_run)
            .await
    }

    /// Delete a file from the vault
    pub async fn delete_file(&self, path: &str) -> Result<()> {
        let file_path = self.manager.vault_path().join(path);

        // Verify it's under vault
        if !file_path.starts_with(self.manager.vault_path()) {
            return Err(Error::path_traversal(file_path));
        }

        tokio::fs::remove_file(&file_path)
            .await
            .map_err(Error::io)?;

        Ok(())
    }

    /// Move a file within the vault
    pub async fn move_file(&self, from: &str, to: &str) -> Result<()> {
        let from_path = self.manager.vault_path().join(from);
        let to_path = self.manager.vault_path().join(to);

        // Verify both paths are under vault
        if !from_path.starts_with(self.manager.vault_path()) {
            return Err(Error::path_traversal(from_path));
        }
        if !to_path.starts_with(self.manager.vault_path()) {
            return Err(Error::path_traversal(to_path));
        }

        // Create parent directory if needed
        if let Some(parent) = to_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(Error::io)?;
        }

        // Perform rename
        tokio::fs::rename(&from_path, &to_path)
            .await
            .map_err(Error::io)?;

        Ok(())
    }

    /// Copy a file within the vault
    pub async fn copy_file(&self, from: &str, to: &str) -> Result<()> {
        let from_path = self.manager.vault_path().join(from);
        let to_path = self.manager.vault_path().join(to);

        // Verify both paths are under vault
        if !from_path.starts_with(self.manager.vault_path()) {
            return Err(Error::path_traversal(from_path));
        }
        if !to_path.starts_with(self.manager.vault_path()) {
            return Err(Error::path_traversal(to_path));
        }

        // Create parent directory if needed
        if let Some(parent) = to_path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(Error::io)?;
        }

        // Perform copy
        tokio::fs::copy(&from_path, &to_path)
            .await
            .map_err(Error::io)?;

        Ok(())
    }
}
