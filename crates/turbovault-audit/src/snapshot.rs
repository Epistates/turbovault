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
        let id = content_address(content);
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

    /// The content hash recorded on audit entries and compared by rollback:
    /// [`turbovault_core::compute_hash`], the same hash `read_note` reports and
    /// a Direct write's `expected_hash` is checked against.
    pub fn compute_hash(content: &str) -> String {
        turbovault_core::compute_hash(content)
    }
}

/// A snapshot's file name: SHA-256 of its exact bytes. Unlike
/// [`SnapshotStore::compute_hash`] this does not normalize, because it is an
/// address rather than a comparison: two notes that differ only in Unicode
/// normalization are different bytes, and a rollback has to restore the ones
/// that were there.
fn content_address(content: &str) -> String {
    bytes_to_lower_hex(Sha256::digest(content.as_bytes()))
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

    /// #88: audit hashes used raw bytes while every other hash normalized, so
    /// they disagreed on any decomposed text. They agree now; the snapshot's
    /// file name still addresses the exact bytes.
    #[tokio::test]
    async fn audit_hashes_agree_with_the_write_hash_but_snapshots_keep_exact_bytes() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = SnapshotStore::new(temp_dir.path().to_path_buf());
        let composed = "caf\u{e9}";
        let decomposed = "cafe\u{301}";

        assert_eq!(
            SnapshotStore::compute_hash(decomposed),
            turbovault_core::compute_hash(composed)
        );

        let a = store.store(composed).await.unwrap();
        let b = store.store(decomposed).await.unwrap();
        assert_ne!(a, b, "different bytes need different snapshots");
        assert_eq!(store.retrieve(&b).await.unwrap(), decomposed);
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
