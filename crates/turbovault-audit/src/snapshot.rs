//! Content-addressed snapshot storage
//!
//! Stores note content snapshots using SHA-256 hash as filename for natural deduplication.
//! If two operations produce the same content, only one copy is stored on disk.

use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};
use turbovault_core::bytes_to_lower_hex;
use turbovault_core::error::Error;
use turbovault_core::{Result, SnapshotRetention};

/// How many new snapshots a store writes between retention passes.
const PRUNE_EVERY: usize = 32;

/// Content-addressed snapshot store
pub struct SnapshotStore {
    snapshot_dir: PathBuf,
    retention: SnapshotRetention,
    written: AtomicUsize,
}

impl SnapshotStore {
    /// Create a new snapshot store at the given directory. It keeps every
    /// snapshot; see [`Self::with_retention`].
    pub fn new(snapshot_dir: PathBuf) -> Self {
        Self {
            snapshot_dir,
            retention: SnapshotRetention::default(),
            written: AtomicUsize::new(0),
        }
    }

    /// Delete snapshots past `retention`'s limits, checked every few new
    /// snapshots and whenever [`Self::prune`] is called.
    pub fn with_retention(mut self, retention: SnapshotRetention) -> Self {
        self.retention = retention;
        self
    }

    /// Store content and return the snapshot ID (SHA-256 hash)
    /// Naturally deduplicates: identical content produces the same hash/filename
    pub async fn store(&self, content: &str) -> Result<String> {
        let id = content_address(content);
        let path = self.snapshot_dir.join(&id);

        // Skip if already stored (content-addressed dedup), marking it used
        // now so retention keeps it as long as it keeps being recorded.
        if path.exists() {
            if let Ok(file) = std::fs::File::options().write(true).open(&path) {
                let _ = file.set_modified(SystemTime::now());
            }
            return Ok(id);
        }

        tokio::fs::write(&path, content).await.map_err(Error::io)?;
        if self.retention.is_limited()
            && (self.written.fetch_add(1, Ordering::Relaxed) + 1).is_multiple_of(PRUNE_EVERY)
            && let Err(error) = self.prune().await
        {
            log::warn!("Snapshot retention pass failed: {}", error);
        }
        Ok(id)
    }

    /// Delete the snapshots past the store's retention limits: those not
    /// used for `max_age_days`, then the least recently used beyond
    /// `max_count`. Returns how many were deleted. With no limits set it
    /// deletes nothing.
    pub async fn prune(&self) -> Result<usize> {
        if !self.retention.is_limited() {
            return Ok(0);
        }
        let dir = self.snapshot_dir.clone();
        let retention = self.retention.clone();
        tokio::task::spawn_blocking(move || {
            let mut snapshots = Vec::new();
            for entry in std::fs::read_dir(&dir).map_err(Error::io)? {
                let entry = entry.map_err(Error::io)?;
                let metadata = entry.metadata().map_err(Error::io)?;
                if metadata.is_file() {
                    let used = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    snapshots.push((used, entry.path()));
                }
            }
            // Most recently used first.
            snapshots.sort_by_key(|(used, _)| std::cmp::Reverse(*used));
            let cutoff = retention
                .max_age_days
                .map(|days| SystemTime::now() - Duration::from_secs(u64::from(days) * 86_400));
            let keep = retention.max_count.unwrap_or(usize::MAX);

            let mut deleted = 0;
            for (rank, (used, path)) in snapshots.into_iter().enumerate() {
                if rank >= keep || cutoff.is_some_and(|cutoff| used < cutoff) {
                    std::fs::remove_file(&path).map_err(Error::io)?;
                    deleted += 1;
                }
            }
            Ok(deleted)
        })
        .await
        .map_err(|e| Error::Other(format!("snapshot retention task failed: {e}")))?
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

    fn age(store: &SnapshotStore, id: &str, days: u64) {
        let file = std::fs::File::options()
            .write(true)
            .open(store.snapshot_dir.join(id))
            .unwrap();
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
        file.set_modified(then).unwrap();
    }

    #[tokio::test]
    async fn prune_keeps_the_most_recently_used_up_to_max_count() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store =
            SnapshotStore::new(temp_dir.path().to_path_buf()).with_retention(SnapshotRetention {
                max_count: Some(2),
                max_age_days: None,
            });
        let a = store.store("a").await.unwrap();
        let b = store.store("b").await.unwrap();
        let c = store.store("c").await.unwrap();
        age(&store, &a, 3);
        age(&store, &b, 2);
        age(&store, &c, 1);

        assert_eq!(store.prune().await.unwrap(), 1);
        assert!(!store.exists(&a));
        assert!(store.exists(&b) && store.exists(&c));
    }

    /// A snapshot stored again counts as used again, so a note's current
    /// content outlives the age limit as long as it keeps being recorded.
    #[tokio::test]
    async fn prune_drops_snapshots_unused_past_max_age() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store =
            SnapshotStore::new(temp_dir.path().to_path_buf()).with_retention(SnapshotRetention {
                max_count: None,
                max_age_days: Some(7),
            });
        let old = store.store("old").await.unwrap();
        let reused = store.store("reused").await.unwrap();
        age(&store, &old, 10);
        age(&store, &reused, 10);
        store.store("reused").await.unwrap();

        assert_eq!(store.prune().await.unwrap(), 1);
        assert!(!store.exists(&old));
        assert!(store.exists(&reused));
    }

    #[tokio::test]
    async fn prune_keeps_everything_by_default() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = SnapshotStore::new(temp_dir.path().to_path_buf());
        let id = store.store("kept").await.unwrap();
        age(&store, &id, 3650);
        assert_eq!(store.prune().await.unwrap(), 0);
        assert!(store.exists(&id));
    }

    #[tokio::test]
    async fn test_retrieve_nonexistent() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let store = SnapshotStore::new(temp_dir.path().to_path_buf());

        let result = store.retrieve("nonexistent_hash").await;
        assert!(result.is_err());
    }
}
