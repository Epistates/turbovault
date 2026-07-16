//! Rollback engine for restoring notes to previous states

use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use turbovault_core::error::Error;
use turbovault_core::{PathValidator, Result};

use crate::log::{AuditEntry, AuditLog, OperationType};
use crate::snapshot::SnapshotStore;

/// Preview of what a rollback would do
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackPreview {
    pub operation_id: String,
    pub path: String,
    pub operation: OperationType,
    pub current_exists: bool,
    pub current_hash: Option<String>,
    pub rollback_to_hash: Option<String>,
    pub would_create: bool,
    pub would_delete: bool,
    pub would_modify: bool,
    pub diff_preview: Option<String>,
    pub warnings: Vec<String>,
}

/// Result of a rollback execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RollbackResult {
    pub operation_id: String,
    pub path: String,
    pub success: bool,
    pub action_taken: String,
    pub audit_entry_id: String,
}

/// Engine for previewing and executing rollbacks
pub struct RollbackEngine {
    audit_log: Arc<AuditLog>,
    snapshot_store: Arc<SnapshotStore>,
}

impl RollbackEngine {
    pub fn new(audit_log: Arc<AuditLog>, snapshot_store: Arc<SnapshotStore>) -> Self {
        Self {
            audit_log,
            snapshot_store,
        }
    }

    /// Preview what a rollback would change (dry run)
    pub async fn preview(&self, operation_id: &str, vault_path: &Path) -> Result<RollbackPreview> {
        let entry = self
            .audit_log
            .get_entry(operation_id)
            .await?
            .ok_or_else(|| Error::not_found(format!("Audit entry not found: {}", operation_id)))?;

        let file_path = PathValidator::validate_path_in_vault(vault_path, Path::new(&entry.path))?;
        let current_exists = file_path.exists();
        let current_hash = if current_exists {
            let content = tokio::fs::read_to_string(&file_path)
                .await
                .map_err(Error::io)?;
            Some(SnapshotStore::compute_hash(&content))
        } else {
            None
        };

        let mut warnings = Vec::new();

        let (would_create, would_delete, would_modify) = match entry.operation {
            OperationType::Create => {
                // Rolling back a create = deleting the file
                if !current_exists {
                    warnings.push("File no longer exists — nothing to roll back".to_string());
                }
                (false, current_exists, false)
            }
            OperationType::Update => {
                // Rolling back an update = restoring previous content
                if entry.before_snapshot_id.is_none() {
                    warnings.push(
                        "No before-snapshot stored — cannot restore previous content".to_string(),
                    );
                } else if !current_exists {
                    warnings.push("File no longer exists — rollback would recreate it".to_string());
                }
                (
                    !current_exists && entry.before_snapshot_id.is_some(),
                    false,
                    current_exists && entry.before_snapshot_id.is_some(),
                )
            }
            OperationType::Delete => {
                // Rolling back a delete = recreating the file
                if entry.before_snapshot_id.is_none() {
                    warnings.push(
                        "No before-snapshot stored — cannot restore deleted content".to_string(),
                    );
                }
                if current_exists {
                    warnings.push(
                        "File already exists at this path — rollback would overwrite".to_string(),
                    );
                }
                (
                    !current_exists && entry.before_snapshot_id.is_some(),
                    false,
                    current_exists && entry.before_snapshot_id.is_some(),
                )
            }
            OperationType::Move => {
                warnings.push("Move rollback not yet supported".to_string());
                (false, false, false)
            }
            OperationType::Rollback => {
                warnings.push("Cannot roll back a rollback operation".to_string());
                (false, false, false)
            }
        };

        // Build diff preview if we're modifying
        let diff_preview = if would_modify {
            if let (Some(before_id), true) = (&entry.before_snapshot_id, current_exists) {
                if let Ok(before_content) = self.snapshot_store.retrieve(before_id).await {
                    if let Ok(current_content) = tokio::fs::read_to_string(&file_path).await {
                        // Use similar for a basic diff preview
                        let diff = similar::TextDiff::from_lines(&current_content, &before_content);
                        Some(
                            diff.unified_diff()
                                .header("current", "rollback-target")
                                .context_radius(3)
                                .to_string(),
                        )
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        Ok(RollbackPreview {
            operation_id: operation_id.to_string(),
            path: entry.path,
            operation: entry.operation,
            current_exists,
            current_hash,
            rollback_to_hash: entry.before_hash,
            would_create,
            would_delete,
            would_modify,
            diff_preview,
            warnings,
        })
    }

    /// Execute a rollback (restores the file to its state before the operation)
    pub async fn execute(&self, operation_id: &str, vault_path: &Path) -> Result<RollbackResult> {
        let entry = self
            .audit_log
            .get_entry(operation_id)
            .await?
            .ok_or_else(|| Error::not_found(format!("Audit entry not found: {}", operation_id)))?;

        let file_path = PathValidator::validate_path_in_vault(vault_path, Path::new(&entry.path))?;

        if matches!(
            entry.operation,
            OperationType::Move | OperationType::Rollback
        ) {
            return Err(Error::Other(format!(
                "Rollback of {} operations is not supported",
                entry.operation
            )));
        }
        let action_taken;

        // Snapshot current state before rollback
        let _current_snapshot_id = if file_path.exists() {
            let content = tokio::fs::read_to_string(&file_path)
                .await
                .map_err(Error::io)?;
            Some(self.snapshot_store.store(&content).await?)
        } else {
            None
        };

        match entry.operation {
            OperationType::Create => {
                // Undo create = delete
                if file_path.exists() {
                    tokio::fs::remove_file(&file_path)
                        .await
                        .map_err(Error::io)?;
                    action_taken = "Deleted file (undoing create)".to_string();
                } else {
                    action_taken = "File already absent — no action taken".to_string();
                }
            }
            OperationType::Update | OperationType::Delete => {
                // Undo update/delete = restore before content
                let before_id = entry.before_snapshot_id.as_ref().ok_or_else(|| {
                    Error::Other("No before-snapshot — cannot restore".to_string())
                })?;

                let before_content = self.snapshot_store.retrieve(before_id).await?;

                // Ensure parent directory exists
                if let Some(parent) = file_path.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(Error::io)?;
                }

                // Atomic write
                let temp_path = file_path.with_extension("tmp");
                tokio::fs::write(&temp_path, &before_content)
                    .await
                    .map_err(Error::io)?;
                tokio::fs::rename(&temp_path, &file_path)
                    .await
                    .map_err(Error::io)?;

                action_taken = format!(
                    "Restored content from snapshot {} (undoing {})",
                    before_id,
                    entry.operation.to_string().to_lowercase()
                );
            }
            OperationType::Move | OperationType::Rollback => unreachable!(),
        }

        // Record the rollback itself as an audit entry
        let rollback_entry = AuditEntry::new(OperationType::Rollback, &entry.path).with_metadata(
            serde_json::json!({
                "rolled_back_operation": operation_id,
                "rolled_back_type": entry.operation.to_string(),
            }),
        );

        let audit_entry_id = rollback_entry.id.clone();

        // Fire-and-forget audit recording
        if let Err(e) = self.audit_log.record(&rollback_entry).await {
            log::warn!("Failed to record rollback audit entry: {}", e);
        }

        Ok(RollbackResult {
            operation_id: operation_id.to_string(),
            path: entry.path,
            success: true,
            action_taken,
            audit_entry_id,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::AuditFilter;

    async fn setup_engine() -> (
        tempfile::TempDir,
        Arc<AuditLog>,
        Arc<SnapshotStore>,
        RollbackEngine,
    ) {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let audit_log = Arc::new(AuditLog::new(temp_dir.path()).await.unwrap());
        let snapshot_store = Arc::new(SnapshotStore::new(audit_log.snapshot_dir().to_path_buf()));
        let engine = RollbackEngine::new(audit_log.clone(), snapshot_store.clone());
        (temp_dir, audit_log, snapshot_store, engine)
    }

    #[tokio::test]
    async fn test_rollback_preview_create() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let audit_log = Arc::new(AuditLog::new(temp_dir.path()).await.unwrap());
        let snapshot_store = Arc::new(SnapshotStore::new(audit_log.snapshot_dir().to_path_buf()));

        // Record a create operation
        let entry = AuditEntry::new(OperationType::Create, "test.md");
        let id = entry.id.clone();
        audit_log.record(&entry).await.unwrap();

        // Create the file
        tokio::fs::write(temp_dir.path().join("test.md"), "content")
            .await
            .unwrap();

        let engine = RollbackEngine::new(audit_log, snapshot_store);
        let preview = engine.preview(&id, temp_dir.path()).await.unwrap();

        assert!(preview.would_delete);
        assert!(!preview.would_create);
        assert!(!preview.would_modify);
    }

    #[tokio::test]
    async fn test_rollback_execute_update() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let audit_log = Arc::new(AuditLog::new(temp_dir.path()).await.unwrap());
        let snapshot_store = Arc::new(SnapshotStore::new(audit_log.snapshot_dir().to_path_buf()));

        let file_path = temp_dir.path().join("test.md");
        let original_content = "Original content";
        let new_content = "Updated content";

        // Store original snapshot
        let before_id = snapshot_store.store(original_content).await.unwrap();

        // Write updated file
        tokio::fs::write(&file_path, new_content).await.unwrap();

        // Record update operation
        let entry = AuditEntry::new(OperationType::Update, "test.md")
            .with_before(SnapshotStore::compute_hash(original_content), before_id);
        let op_id = entry.id.clone();
        audit_log.record(&entry).await.unwrap();

        // Execute rollback
        let engine = RollbackEngine::new(audit_log, snapshot_store);
        let result = engine.execute(&op_id, temp_dir.path()).await.unwrap();

        assert!(result.success);

        // Verify file was restored
        let restored = tokio::fs::read_to_string(&file_path).await.unwrap();
        assert_eq!(restored, original_content);
    }

    #[tokio::test]
    async fn test_rollback_nonexistent_operation() {
        let temp_dir = tempfile::TempDir::new().unwrap();
        let audit_log = Arc::new(AuditLog::new(temp_dir.path()).await.unwrap());
        let snapshot_store = Arc::new(SnapshotStore::new(audit_log.snapshot_dir().to_path_buf()));

        let engine = RollbackEngine::new(audit_log, snapshot_store);
        let result = engine.preview("nonexistent", temp_dir.path()).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn preview_describes_missing_snapshots_recreation_overwrite_and_unsupported_types() {
        let (temp_dir, audit_log, snapshot_store, engine) = setup_engine().await;
        let original = "Original content\n";
        let snapshot_id = snapshot_store.store(original).await.unwrap();
        let original_hash = SnapshotStore::compute_hash(original);

        let create = AuditEntry::new(OperationType::Create, "missing-create.md");
        let update_missing_snapshot = AuditEntry::new(OperationType::Update, "update.md");
        let update_missing_file = AuditEntry::new(OperationType::Update, "missing-update.md")
            .with_before(original_hash.clone(), snapshot_id.clone());
        let delete_existing = AuditEntry::new(OperationType::Delete, "delete.md")
            .with_before(original_hash, snapshot_id);
        let move_entry = AuditEntry::new(OperationType::Move, "move.md");
        let rollback_entry = AuditEntry::new(OperationType::Rollback, "rollback.md");

        for entry in [
            &create,
            &update_missing_snapshot,
            &update_missing_file,
            &delete_existing,
            &move_entry,
            &rollback_entry,
        ] {
            audit_log.record(entry).await.unwrap();
        }
        tokio::fs::write(temp_dir.path().join("update.md"), "Current update\n")
            .await
            .unwrap();
        tokio::fs::write(temp_dir.path().join("delete.md"), "Replacement content\n")
            .await
            .unwrap();

        let create_preview = engine.preview(&create.id, temp_dir.path()).await.unwrap();
        assert!(!create_preview.would_delete);
        assert!(create_preview.warnings[0].contains("no longer exists"));

        let missing_snapshot = engine
            .preview(&update_missing_snapshot.id, temp_dir.path())
            .await
            .unwrap();
        assert!(!missing_snapshot.would_modify);
        assert!(missing_snapshot.warnings[0].contains("No before-snapshot"));

        let recreate = engine
            .preview(&update_missing_file.id, temp_dir.path())
            .await
            .unwrap();
        assert!(recreate.would_create);
        assert!(!recreate.would_modify);
        assert!(recreate.warnings[0].contains("would recreate"));

        let overwrite = engine
            .preview(&delete_existing.id, temp_dir.path())
            .await
            .unwrap();
        assert!(overwrite.would_modify);
        assert!(!overwrite.would_create);
        assert!(overwrite.diff_preview.is_some());
        assert!(overwrite.warnings[0].contains("would overwrite"));

        let move_preview = engine
            .preview(&move_entry.id, temp_dir.path())
            .await
            .unwrap();
        assert_eq!(
            move_preview.warnings,
            vec!["Move rollback not yet supported"]
        );
        let rollback_preview = engine
            .preview(&rollback_entry.id, temp_dir.path())
            .await
            .unwrap();
        assert_eq!(
            rollback_preview.warnings,
            vec!["Cannot roll back a rollback operation"]
        );
    }

    #[tokio::test]
    async fn execute_deletes_creates_nested_parents_and_records_each_rollback() {
        let (temp_dir, audit_log, snapshot_store, engine) = setup_engine().await;
        let created_path = temp_dir.path().join("created.md");
        tokio::fs::write(&created_path, "Created content\n")
            .await
            .unwrap();
        let create = AuditEntry::new(OperationType::Create, "created.md");
        audit_log.record(&create).await.unwrap();

        let deleted = engine.execute(&create.id, temp_dir.path()).await.unwrap();
        assert!(deleted.action_taken.contains("Deleted file"));
        assert!(!created_path.exists());
        let already_absent = engine.execute(&create.id, temp_dir.path()).await.unwrap();
        assert!(already_absent.action_taken.contains("already absent"));

        let original = "Restored nested content\n";
        let snapshot_id = snapshot_store.store(original).await.unwrap();
        let delete = AuditEntry::new(OperationType::Delete, "nested/deleted.md")
            .with_before(SnapshotStore::compute_hash(original), snapshot_id);
        audit_log.record(&delete).await.unwrap();

        let restored = engine.execute(&delete.id, temp_dir.path()).await.unwrap();
        assert!(restored.action_taken.contains("undoing delete"));
        assert_eq!(
            tokio::fs::read_to_string(temp_dir.path().join("nested/deleted.md"))
                .await
                .unwrap(),
            original
        );

        let rollbacks = audit_log
            .query(&AuditFilter::new().with_operation(OperationType::Rollback))
            .await
            .unwrap();
        assert_eq!(rollbacks.len(), 3);
        assert!(rollbacks.iter().all(|entry| {
            entry.metadata.get("rolled_back_operation").is_some()
                && entry.metadata.get("rolled_back_type").is_some()
        }));
    }

    #[tokio::test]
    async fn execute_rejects_missing_entries_snapshots_and_unsupported_operations() {
        let (temp_dir, audit_log, _snapshot_store, engine) = setup_engine().await;
        let missing_snapshot = AuditEntry::new(OperationType::Update, "update.md");
        let move_entry = AuditEntry::new(OperationType::Move, "binary.bin");
        let rollback_entry = AuditEntry::new(OperationType::Rollback, "rollback.md");
        for entry in [&missing_snapshot, &move_entry, &rollback_entry] {
            audit_log.record(entry).await.unwrap();
        }
        tokio::fs::write(temp_dir.path().join("update.md"), "Current\n")
            .await
            .unwrap();
        tokio::fs::write(temp_dir.path().join("binary.bin"), [0, 159, 255])
            .await
            .unwrap();

        let missing = engine
            .execute("missing", temp_dir.path())
            .await
            .unwrap_err();
        assert!(missing.to_string().contains("Audit entry not found"));
        let no_snapshot = engine
            .execute(&missing_snapshot.id, temp_dir.path())
            .await
            .unwrap_err();
        assert!(no_snapshot.to_string().contains("No before-snapshot"));
        let unsupported_move = engine
            .execute(&move_entry.id, temp_dir.path())
            .await
            .unwrap_err();
        assert!(unsupported_move.to_string().contains("MOVE operations"));
        let unsupported_rollback = engine
            .execute(&rollback_entry.id, temp_dir.path())
            .await
            .unwrap_err();
        assert!(
            unsupported_rollback
                .to_string()
                .contains("ROLLBACK operations")
        );
    }

    #[tokio::test]
    async fn preview_and_execute_reject_audit_paths_outside_the_vault() {
        let root = tempfile::TempDir::new().unwrap();
        let vault_path = root.path().join("vault");
        tokio::fs::create_dir(&vault_path).await.unwrap();
        let outside_path = root.path().join("outside.md");
        tokio::fs::write(&outside_path, "Must remain\n")
            .await
            .unwrap();
        let audit_log = Arc::new(AuditLog::new(&vault_path).await.unwrap());
        let snapshot_store = Arc::new(SnapshotStore::new(audit_log.snapshot_dir().to_path_buf()));
        let malicious = AuditEntry::new(OperationType::Create, "../outside.md");
        audit_log.record(&malicious).await.unwrap();
        let engine = RollbackEngine::new(audit_log, snapshot_store);

        assert!(engine.preview(&malicious.id, &vault_path).await.is_err());
        assert!(engine.execute(&malicious.id, &vault_path).await.is_err());
        assert_eq!(
            tokio::fs::read_to_string(outside_path).await.unwrap(),
            "Must remain\n"
        );
    }
}
