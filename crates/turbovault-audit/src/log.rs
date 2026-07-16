//! Append-only JSONL audit log for vault operations

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;
use turbovault_core::Result;
use turbovault_core::error::Error;
use uuid::Uuid;

/// Type of operation tracked in the audit log
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OperationType {
    Create,
    Update,
    Delete,
    Move,
    Rollback,
}

impl std::fmt::Display for OperationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Create => write!(f, "CREATE"),
            Self::Update => write!(f, "UPDATE"),
            Self::Delete => write!(f, "DELETE"),
            Self::Move => write!(f, "MOVE"),
            Self::Rollback => write!(f, "ROLLBACK"),
        }
    }
}

/// A single audit log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub id: String,
    pub timestamp: String,
    pub operation: OperationType,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before_snapshot_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after_snapshot_id: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl AuditEntry {
    /// Create a new audit entry with auto-generated ID and timestamp
    pub fn new(operation: OperationType, path: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            operation,
            path: path.into(),
            new_path: None,
            before_hash: None,
            after_hash: None,
            before_snapshot_id: None,
            after_snapshot_id: None,
            metadata: serde_json::json!({}),
        }
    }

    pub fn with_new_path(mut self, path: impl Into<String>) -> Self {
        self.new_path = Some(path.into());
        self
    }

    pub fn with_before(mut self, hash: impl Into<String>, snapshot_id: impl Into<String>) -> Self {
        self.before_hash = Some(hash.into());
        self.before_snapshot_id = Some(snapshot_id.into());
        self
    }

    pub fn with_after(mut self, hash: impl Into<String>, snapshot_id: impl Into<String>) -> Self {
        self.after_hash = Some(hash.into());
        self.after_snapshot_id = Some(snapshot_id.into());
        self
    }

    pub fn with_metadata(mut self, metadata: serde_json::Value) -> Self {
        self.metadata = metadata;
        self
    }
}

/// Filter for querying audit entries
#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub path: Option<String>,
    pub operation: Option<OperationType>,
    pub since: Option<chrono::DateTime<Utc>>,
    pub until: Option<chrono::DateTime<Utc>>,
    pub limit: usize,
}

impl AuditFilter {
    pub fn new() -> Self {
        Self {
            limit: 50,
            ..Default::default()
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn with_operation(mut self, op: OperationType) -> Self {
        self.operation = Some(op);
        self
    }

    pub fn with_since(mut self, since: chrono::DateTime<Utc>) -> Self {
        self.since = Some(since);
        self
    }

    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// Audit trail statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditStats {
    pub total_operations: usize,
    pub operations_by_type: HashMap<String, usize>,
    pub total_snapshot_size_bytes: u64,
    pub oldest_entry: Option<String>,
    pub newest_entry: Option<String>,
}

/// Audit log manager — append-only JSONL storage
pub struct AuditLog {
    #[allow(dead_code)]
    audit_dir: PathBuf,
    log_file: PathBuf,
    snapshot_dir: PathBuf,
    write_lock: tokio::sync::Mutex<()>,
}

impl AuditLog {
    /// Create a new audit log for a vault
    pub async fn new(vault_path: &Path) -> Result<Self> {
        let audit_dir = vault_path.join(".turbovault").join("audit");
        let log_file = audit_dir.join("operations.jsonl");
        let snapshot_dir = audit_dir.join("snapshots");

        // Create directories
        tokio::fs::create_dir_all(&audit_dir)
            .await
            .map_err(Error::io)?;
        tokio::fs::create_dir_all(&snapshot_dir)
            .await
            .map_err(Error::io)?;

        Ok(Self {
            audit_dir,
            log_file,
            snapshot_dir,
            write_lock: tokio::sync::Mutex::new(()),
        })
    }

    /// Get the snapshot directory path
    pub fn snapshot_dir(&self) -> &Path {
        &self.snapshot_dir
    }

    /// Record an audit entry (append to JSONL)
    pub async fn record(&self, entry: &AuditEntry) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut json = serde_json::to_string(entry)
            .map_err(|e| Error::Other(format!("Failed to serialize audit entry: {}", e)))?;
        json.push('\n');

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file)
            .await
            .map_err(Error::io)?;

        file.write_all(json.as_bytes()).await.map_err(Error::io)?;
        file.flush().await.map_err(Error::io)?;

        Ok(())
    }

    /// Query audit entries with optional filters
    pub async fn query(&self, filter: &AuditFilter) -> Result<Vec<AuditEntry>> {
        if filter.limit == 0 {
            return Ok(vec![]);
        }
        if !self.log_file.exists() {
            return Ok(vec![]);
        }

        let content = tokio::fs::read_to_string(&self.log_file)
            .await
            .map_err(Error::io)?;

        let mut entries: Vec<AuditEntry> = Vec::new();

        for line in content.lines().rev() {
            // Reverse order (newest first)
            if line.trim().is_empty() {
                continue;
            }

            let entry: AuditEntry = match serde_json::from_str(line) {
                Ok(e) => e,
                Err(e) => {
                    log::warn!("Skipping malformed audit entry: {}", e);
                    continue;
                }
            };

            // Apply filters
            if let Some(ref path_filter) = filter.path
                && !entry.path.contains(path_filter)
            {
                continue;
            }

            if let Some(ref op_filter) = filter.operation
                && &entry.operation != op_filter
            {
                continue;
            }

            if let Some(ref since) = filter.since
                && let Ok(entry_time) = chrono::DateTime::parse_from_rfc3339(&entry.timestamp)
                && entry_time < *since
            {
                continue;
            }

            if let Some(ref until) = filter.until
                && let Ok(entry_time) = chrono::DateTime::parse_from_rfc3339(&entry.timestamp)
                && entry_time > *until
            {
                continue;
            }

            entries.push(entry);

            if entries.len() >= filter.limit {
                break;
            }
        }

        Ok(entries)
    }

    /// Get a specific entry by ID
    pub async fn get_entry(&self, id: &str) -> Result<Option<AuditEntry>> {
        if !self.log_file.exists() {
            return Ok(None);
        }

        let content = tokio::fs::read_to_string(&self.log_file)
            .await
            .map_err(Error::io)?;

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(entry) = serde_json::from_str::<AuditEntry>(line)
                && entry.id == id
            {
                return Ok(Some(entry));
            }
        }

        Ok(None)
    }

    /// Compute audit statistics
    pub async fn stats(&self) -> Result<AuditStats> {
        let mut total_operations = 0usize;
        let mut operations_by_type: HashMap<String, usize> = HashMap::new();
        let mut oldest_entry: Option<String> = None;
        let mut newest_entry: Option<String> = None;

        if self.log_file.exists() {
            let content = tokio::fs::read_to_string(&self.log_file)
                .await
                .map_err(Error::io)?;

            for line in content.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                if let Ok(entry) = serde_json::from_str::<AuditEntry>(line) {
                    total_operations += 1;
                    *operations_by_type
                        .entry(entry.operation.to_string())
                        .or_insert(0) += 1;

                    if oldest_entry.is_none() {
                        oldest_entry = Some(entry.timestamp.clone());
                    }
                    newest_entry = Some(entry.timestamp);
                }
            }
        }

        // Calculate snapshot storage size
        let total_snapshot_size_bytes = compute_dir_size(&self.snapshot_dir).await;

        Ok(AuditStats {
            total_operations,
            operations_by_type,
            total_snapshot_size_bytes,
            oldest_entry,
            newest_entry,
        })
    }
}

async fn compute_dir_size(dir: &Path) -> u64 {
    let mut size = 0u64;
    if let Ok(mut entries) = tokio::fs::read_dir(dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Ok(meta) = entry.metadata().await {
                size += meta.len();
            }
        }
    }
    size
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_audit_log_record_and_query() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let log = AuditLog::new(temp_dir.path()).await.unwrap();

        let entry = AuditEntry::new(OperationType::Create, "notes/test.md")
            .with_after("abc123".to_string(), "snap_abc123".to_string());

        log.record(&entry).await.unwrap();

        let entries = log.query(&AuditFilter::new()).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "notes/test.md");
        assert_eq!(entries[0].operation, OperationType::Create);

        let zero_limit = log.query(&AuditFilter::new().with_limit(0)).await.unwrap();
        assert!(zero_limit.is_empty());
    }

    #[tokio::test]
    async fn test_audit_filter_by_operation() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let log = AuditLog::new(temp_dir.path()).await.unwrap();

        log.record(&AuditEntry::new(OperationType::Create, "a.md"))
            .await
            .unwrap();
        log.record(&AuditEntry::new(OperationType::Update, "b.md"))
            .await
            .unwrap();
        log.record(&AuditEntry::new(OperationType::Delete, "c.md"))
            .await
            .unwrap();

        let filter = AuditFilter::new().with_operation(OperationType::Update);
        let entries = log.query(&filter).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "b.md");
    }

    #[tokio::test]
    async fn test_audit_get_entry() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let log = AuditLog::new(temp_dir.path()).await.unwrap();

        let entry = AuditEntry::new(OperationType::Create, "test.md");
        let id = entry.id.clone();
        log.record(&entry).await.unwrap();

        let found = log.get_entry(&id).await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, id);

        let not_found = log.get_entry("nonexistent").await.unwrap();
        assert!(not_found.is_none());
    }

    #[tokio::test]
    async fn test_audit_stats() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let log = AuditLog::new(temp_dir.path()).await.unwrap();

        log.record(&AuditEntry::new(OperationType::Create, "a.md"))
            .await
            .unwrap();
        log.record(&AuditEntry::new(OperationType::Update, "a.md"))
            .await
            .unwrap();

        let stats = log.stats().await.unwrap();
        assert_eq!(stats.total_operations, 2);
        assert_eq!(stats.operations_by_type.get("CREATE"), Some(&1));
        assert_eq!(stats.operations_by_type.get("UPDATE"), Some(&1));
    }

    #[tokio::test]
    async fn test_audit_empty_log() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let log = AuditLog::new(temp_dir.path()).await.unwrap();

        let entries = log.query(&AuditFilter::new()).await.unwrap();
        assert!(entries.is_empty());

        let stats = log.stats().await.unwrap();
        assert_eq!(stats.total_operations, 0);
    }
}
