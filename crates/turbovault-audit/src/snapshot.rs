//! Content-addressed snapshot storage
//!
//! Stores note content snapshots using SHA-256 hash as filename for natural deduplication.
//! If two operations produce the same content, only one copy is stored on disk.

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use turbovault_core::Result;
use turbovault_core::bytes_to_lower_hex;
use turbovault_core::error::Error;

/// Content-addressed snapshot store
pub struct SnapshotStore {
    snapshot_dir: PathBuf,
}

impl SnapshotStore {
    /// Create a new snapshot store at the given directory
    pub fn new(snapshot_dir: PathBuf) -> Self {
        Self { snapshot_dir }
    }

    /// Store content and return the snapshot ID (SHA-256 hash)
    /// Naturally deduplicates: identical content produces the same hash/filename
    pub async fn store(&self, content: &str) -> Result<String> {
        let id = Self::compute_hash(content);
        let path = self.snapshot_dir.join(&id);

        // Skip if already stored (content-addressed dedup)
        if path.exists() {
            return Ok(id);
        }

        tokio::fs::write(&path, content).await.map_err(Error::io)?;
        Ok(id)
    }

    /// Retrieve content by snapshot ID
    pub async fn retrieve(&self, id: &str) -> Result<String> {
        let path = self.snapshot_dir.join(id);
        if !path.exists() {
            return Err(Error::not_found(format!("Snapshot not found: {}", id)));
        }
        tokio::fs::read_to_string(&path).await.map_err(Error::io)
    }

    /// Check if a snapshot exists
    pub fn exists(&self, id: &str) -> bool {
        self.snapshot_dir.join(id).exists()
    }

    /// Compute SHA-256 hash of content (same as VaultManager's compute_hash)
    pub fn compute_hash(content: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(content.as_bytes());
        bytes_to_lower_hex(hasher.finalize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_store_and_retrieve() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = SnapshotStore::new(temp_dir.path().to_path_buf());

        let content = "# Hello\n\nThis is a test note.\n";
        let id = store.store(content).await.unwrap();

        assert!(!id.is_empty());
        assert!(store.exists(&id));

        let retrieved = store.retrieve(&id).await.unwrap();
        assert_eq!(retrieved, content);
    }

    #[tokio::test]
    async fn test_deduplication() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = SnapshotStore::new(temp_dir.path().to_path_buf());

        let content = "Same content";
        let id1 = store.store(content).await.unwrap();
        let id2 = store.store(content).await.unwrap();

        assert_eq!(id1, id2, "Same content should produce same ID");
    }

    #[tokio::test]
    async fn test_different_content_different_ids() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = SnapshotStore::new(temp_dir.path().to_path_buf());

        let id1 = store.store("Content A").await.unwrap();
        let id2 = store.store("Content B").await.unwrap();

        assert_ne!(id1, id2);
    }

    #[tokio::test]
    async fn test_retrieve_nonexistent() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = SnapshotStore::new(temp_dir.path().to_path_buf());

        let result = store.retrieve("nonexistent_hash").await;
        assert!(result.is_err());
    }
}
