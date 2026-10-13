//! Append-only JSONL audit log for vault operations

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use turbovault_core::Result;
use turbovault_core::error::Error;
use uuid::Uuid;

/// Type of operation tracked in the audit log
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct AuditEntry {
    pub id: String,
    pub timestamp: String,
    pub operation: OperationType,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_snapshot_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_snapshot_id: Option<String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// Who and what the operation was done for, when known. Recorded from
    /// the [`AuditContext`] the operation ran in if the entry has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<AuditContext>,
    /// SHA-256 of the log line before this entry's (of an empty line, for
    /// the first), set when the entry is recorded. It chains the log, so
    /// [`AuditLog::verify`] finds an entry that was edited or removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hash: Option<String>,
}

/// Who and what an operation was done for: the tool call that made it and
/// the client that asked. Run the operation inside [`AuditContext::scope`]
/// and the audit entries recorded while it runs carry the context.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct AuditContext {
    /// The MCP tool that made the change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The MCP request it answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// The client that sent the request, when the transport identifies it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// The authenticated user, when the transport authenticates requests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    /// The client session, when the transport has sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

tokio::task_local! {
    static CURRENT_CONTEXT: AuditContext;
}

impl AuditContext {
    /// Run `future` in this context: audit entries recorded on its task
    /// while it runs carry it.
    pub async fn scope<F: std::future::Future>(self, future: F) -> F::Output {
        CURRENT_CONTEXT.scope(self, future).await
    }

    /// The context of the operation running on this task, if any.
    pub fn current() -> Option<Self> {
        CURRENT_CONTEXT.try_with(Clone::clone).ok()
    }
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
            context: None,
            prev_hash: None,
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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct AuditStats {
    pub total_operations: usize,
    pub operations_by_type: HashMap<String, usize>,
    pub total_snapshot_size_bytes: u64,
    pub oldest_entry: Option<String>,
    pub newest_entry: Option<String>,
    /// Whether the log's hash chain is intact.
    pub chain: ChainVerification,
}

/// The result of checking an audit log's hash chain.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct ChainVerification {
    /// Lines in the log.
    pub entries: usize,
    /// Entries recorded before the log was chained, which can't be checked.
    pub unchained: usize,
    /// The 1-based line of the first entry that does not follow from the
    /// line before it: that line, or one before it, was edited or removed,
    /// or entries were added out of order. `None` when the chain is intact.
    ///
    /// A chain shows changes to everything but its last entry, which has no
    /// successor to vouch for it.
    pub broken_at: Option<usize>,
}

impl ChainVerification {
    fn of(content: &str) -> Self {
        let mut verification = Self::default();
        let mut previous = "";
        let mut chained = false;
        for (index, line) in content.lines().enumerate() {
            verification.entries += 1;
            let prev_hash = serde_json::from_str::<serde_json::Value>(line)
                .ok()
                .and_then(|value| value.get("prev_hash")?.as_str().map(str::to_string));
            match prev_hash {
                Some(hash) if hash == line_hash(previous.as_bytes()) => chained = true,
                None if !chained => verification.unchained += 1,
                _ => {
                    verification.broken_at = Some(index + 1);
                    break;
                }
            }
            previous = line;
        }
        verification
    }
}

/// SHA-256 of one log line, without its newline: what the next entry's
/// `prev_hash` holds.
fn line_hash(line: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    turbovault_core::bytes_to_lower_hex(Sha256::digest(line))
}

/// Append `entry` to the log at `path`, chained to the line before it.
/// Holds an exclusive lock on the file throughout, so another process
/// appending at the same moment lands wholly before or after and the chain
/// stays one chain, and syncs the entry to disk before returning.
fn append_chained(path: &Path, mut entry: AuditEntry) -> Result<()> {
    use std::io::{Read, Seek, SeekFrom, Write};

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(path)
        .map_err(Error::io)?;
    file.lock().map_err(Error::io)?;

    // The last line: read back from the end until the newline before it.
    let len = file.metadata().map_err(Error::io)?.len();
    let mut tail = Vec::new();
    let mut start = len;
    let ends_cleanly = loop {
        if start == 0 {
            break true;
        }
        let step = start.min(8192);
        start -= step;
        let mut chunk = vec![0; step as usize];
        file.seek(SeekFrom::Start(start)).map_err(Error::io)?;
        file.read_exact(&mut chunk).map_err(Error::io)?;
        chunk.extend_from_slice(&tail);
        tail = chunk;
        let body = tail.strip_suffix(b"\n").unwrap_or(&tail);
        if body.contains(&b'\n') || start == 0 {
            break tail.last() == Some(&b'\n') || tail.is_empty();
        }
    };
    let body = tail.strip_suffix(b"\n").unwrap_or(&tail);
    let last = match body.iter().rposition(|&b| b == b'\n') {
        Some(newline) => &body[newline + 1..],
        None => body,
    };
    entry.prev_hash = Some(line_hash(last));

    let mut json = serde_json::to_string(&entry)
        .map_err(|e| Error::Other(format!("Failed to serialize audit entry: {}", e)))?;
    json.push('\n');
    // A line cut short by a crash is closed first, so this entry starts on
    // a line of its own.
    if !ends_cleanly {
        json.insert(0, '\n');
    }
    file.write_all(json.as_bytes()).map_err(Error::io)?;
    file.sync_data().map_err(Error::io)
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

    /// Record an audit entry: append it to the JSONL log, chained to the
    /// entry before it and synced to disk. An entry with no context gets the
    /// [`AuditContext`] it was recorded in, if any.
    pub async fn record(&self, entry: &AuditEntry) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let mut entry = entry.clone();
        if entry.context.is_none() {
            entry.context = AuditContext::current();
        }
        let log_file = self.log_file.clone();
        tokio::task::spawn_blocking(move || append_chained(&log_file, entry))
            .await
            .map_err(|e| Error::Other(format!("audit write task failed: {e}")))?
    }

    /// Check the log's hash chain: whether any entry but the last was edited
    /// or removed since it was recorded.
    pub async fn verify(&self) -> Result<ChainVerification> {
        match tokio::fs::read_to_string(&self.log_file).await {
            Ok(content) => Ok(ChainVerification::of(&content)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(ChainVerification::default())
            }
            Err(error) => Err(Error::io(error)),
        }
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
        let mut chain = ChainVerification::default();

        if self.log_file.exists() {
            let content = tokio::fs::read_to_string(&self.log_file)
                .await
                .map_err(Error::io)?;
            chain = ChainVerification::of(&content);

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
            chain,
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

    fn log_lines(temp_dir: &tempfile::TempDir) -> Vec<String> {
        std::fs::read_to_string(temp_dir.path().join(".turbovault/audit/operations.jsonl"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn write_lines(temp_dir: &tempfile::TempDir, lines: &[String]) {
        std::fs::write(
            temp_dir.path().join(".turbovault/audit/operations.jsonl"),
            lines.join("\n") + "\n",
        )
        .unwrap();
    }

    /// Each entry names the one before it, so editing or removing an entry
    /// breaks the chain at the entry after it.
    #[tokio::test]
    async fn entries_are_chained_and_tampering_is_found() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let log = AuditLog::new(temp_dir.path()).await.unwrap();
        for path in ["a.md", "b.md", "c.md"] {
            log.record(&AuditEntry::new(OperationType::Create, path))
                .await
                .unwrap();
        }
        let intact = log.verify().await.unwrap();
        assert_eq!((intact.entries, intact.broken_at), (3, None));

        let lines = log_lines(&temp_dir);
        let mut edited = lines.clone();
        edited[1] = edited[1].replace("b.md", "x.md");
        write_lines(&temp_dir, &edited);
        assert_eq!(log.verify().await.unwrap().broken_at, Some(3));

        write_lines(&temp_dir, &[lines[0].clone(), lines[2].clone()]);
        assert_eq!(log.verify().await.unwrap().broken_at, Some(2));
    }

    /// Entries from before the chain existed are accepted, and the chain
    /// starts with the first entry recorded after them.
    #[tokio::test]
    async fn the_chain_starts_after_older_unchained_entries() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let log = AuditLog::new(temp_dir.path()).await.unwrap();
        let old = serde_json::json!({
            "id": "old", "timestamp": "2026-01-01T00:00:00Z",
            "operation": "CREATE", "path": "old.md"
        });
        write_lines(&temp_dir, &[old.to_string()]);
        log.record(&AuditEntry::new(OperationType::Update, "old.md"))
            .await
            .unwrap();
        let verified = log.verify().await.unwrap();
        assert_eq!(
            (verified.entries, verified.unchained, verified.broken_at),
            (2, 1, None)
        );
    }

    /// Two processes appending to one log extend one chain.
    #[tokio::test]
    async fn two_writers_keep_one_chain() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let first = AuditLog::new(temp_dir.path()).await.unwrap();
        let second = AuditLog::new(temp_dir.path()).await.unwrap();
        for i in 0..6 {
            let log = if i % 2 == 0 { &first } else { &second };
            log.record(&AuditEntry::new(OperationType::Create, format!("{i}.md")))
                .await
                .unwrap();
        }
        assert_eq!(first.verify().await.unwrap().broken_at, None);
    }

    /// An entry recorded while a tool call runs says which call it was.
    #[tokio::test]
    async fn entries_carry_the_running_operations_context() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let log = AuditLog::new(temp_dir.path()).await.unwrap();
        let context = AuditContext {
            tool: Some("write_note".into()),
            request_id: Some("req-7".into()),
            ..Default::default()
        };
        context
            .clone()
            .scope(log.record(&AuditEntry::new(OperationType::Create, "a.md")))
            .await
            .unwrap();
        log.record(&AuditEntry::new(OperationType::Create, "b.md"))
            .await
            .unwrap();

        let entries = log.query(&AuditFilter::new()).await.unwrap();
        assert_eq!(entries[1].context.as_ref(), Some(&context));
        assert_eq!(entries[0].context, None);
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
