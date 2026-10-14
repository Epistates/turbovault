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
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RollbackPreview {
    /// The audit entry the rollback would undo.
    pub operation_id: String,
    /// Vault-relative path of the note it would change.
    pub path: String,
    /// What the entry recorded doing.
    pub operation: OperationType,
    /// Whether the note exists now.
    pub current_exists: bool,
    /// The note's content hash now, if it exists.
    pub current_hash: Option<String>,
    /// The content hash the note would have after the rollback, if known.
    pub rollback_to_hash: Option<String>,
    /// The rollback would recreate a note that's gone.
    pub would_create: bool,
    /// The rollback would delete the note (undoing a create).
    pub would_delete: bool,
    /// The rollback would replace the note's current content.
    pub would_modify: bool,
    /// Unified diff from the current content to the restored content, when
    /// the rollback would modify the note.
    pub diff_preview: Option<String>,
    /// Why the rollback can't go ahead as is, or what it would overwrite.
    pub warnings: Vec<String>,
}

/// Result of a rollback execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct RollbackResult {
    /// The audit entry that was undone.
    pub operation_id: String,
    /// Vault-relative path of the note that changed.
    pub path: String,
    /// Whether the rollback was applied.
    pub success: bool,
    /// What the rollback did, in words.
    pub action_taken: String,
    /// The audit entry recording the rollback, which can itself be rolled
    /// back.
    pub audit_entry_id: String,
}

/// Engine for previewing and executing rollbacks
pub struct RollbackEngine {
    audit_log: Arc<AuditLog>,
    snapshot_store: Arc<SnapshotStore>,
}

impl RollbackEngine {
    /// An engine that reads entries from `audit_log` and content from
    /// `snapshot_store`.
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
        let current = match tokio::fs::read(&file_path).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(Error::io(error)),
        };
        let current_exists = current.is_some();
        let current_hash = current.as_deref().map(turbovault_core::compute_hash_bytes);

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
            OperationType::Rollback => match target_state(&entry) {
                Ok(Target::Absent) => (false, current_exists, false),
                Ok(Target::Snapshot(_)) => (!current_exists, false, current_exists),
                Err(_) => {
                    warnings.push(
                        "Cannot roll back a rollback recorded without the content it replaced"
                            .to_string(),
                    );
                    (false, false, false)
                }
            },
        };
        if let Some(change) = changed_since(&entry, current.as_deref()) {
            warnings.push(format!(
                "File {change} since this operation; rolling back would discard that, \
                 so it needs force"
            ));
        }

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

    /// Work out what rolling back `operation_id` does, checked against the
    /// file as it is now. Unless `force` is set, a file that no longer holds
    /// what the operation left behind is refused, since rolling it back
    /// would discard whatever changed it since.
    pub async fn plan(
        &self,
        operation_id: &str,
        vault_path: &Path,
        force: bool,
    ) -> Result<RollbackPlan> {
        let entry = self
            .audit_log
            .get_entry(operation_id)
            .await?
            .ok_or_else(|| Error::not_found(format!("Audit entry not found: {}", operation_id)))?;
        let file_path = PathValidator::validate_path_in_vault(vault_path, Path::new(&entry.path))?;

        let restore_snapshot_id = match target_state(&entry)? {
            Target::Absent => None,
            Target::Snapshot(id) => Some(id),
        };
        let restore = match &restore_snapshot_id {
            Some(id) => Some(self.snapshot_store.retrieve(id).await?),
            None => None,
        };
        let current = match tokio::fs::read(&file_path).await {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(Error::io_at(entry.path.clone(), error)),
        };

        if !force && let Some(change) = changed_since(&entry, current.as_deref()) {
            return Err(Error::concurrency_error(format!(
                "'{}' {change} since operation {}, so rolling it back would discard that. \
                 Check rollback_preview, then roll back with force to discard it anyway.",
                entry.path, entry.id
            )));
        }

        Ok(RollbackPlan {
            entry,
            restore,
            restore_snapshot_id,
            current,
        })
    }

    /// Execute a rollback (restores the file to its state before the
    /// operation), refusing one that would discard a later change.
    ///
    /// This writes the file directly. A running server applies a
    /// [`RollbackPlan`] through its own write path instead, so the write
    /// takes the vault's write lock and is checked like any other.
    pub async fn execute(&self, operation_id: &str, vault_path: &Path) -> Result<RollbackResult> {
        self.execute_checked(operation_id, vault_path, false).await
    }

    /// [`Self::execute`], rolling back over a later change when `force` is set.
    pub async fn execute_checked(
        &self,
        operation_id: &str,
        vault_path: &Path,
        force: bool,
    ) -> Result<RollbackResult> {
        let plan = self.plan(operation_id, vault_path, force).await?;
        let file_path =
            PathValidator::validate_path_in_vault(vault_path, Path::new(&plan.entry.path))?;

        match &plan.restore {
            None if plan.current.is_none() => {}
            None => tokio::fs::remove_file(&file_path)
                .await
                .map_err(|e| Error::io_at(plan.entry.path.clone(), e))?,
            Some(content) => {
                // Through the shared atomic write. This used to write through
                // `note.tmp` for `note.md`, overwriting a real note of that
                // name and then renaming it away.
                let (target, bytes) = (file_path.clone(), content.clone().into_bytes());
                tokio::task::spawn_blocking(move || turbovault_core::write_atomic(&target, &bytes))
                    .await
                    .map_err(|e| Error::Other(format!("rollback write task failed: {e}")))?
                    .map_err(Error::io)?;
            }
        }

        let audit_entry_id = self.record(&plan).await;
        Ok(RollbackResult {
            operation_id: operation_id.to_string(),
            path: plan.entry.path.clone(),
            success: true,
            action_taken: plan.action(),
            audit_entry_id,
        })
    }

    /// Record a rollback that [`Self::execute_checked`] applied, with the
    /// content it replaced as its before-state so it can be rolled back in
    /// turn. Returns the new entry's id.
    async fn record(&self, plan: &RollbackPlan) -> String {
        let mut entry = AuditEntry::new(OperationType::Rollback, &plan.entry.path).with_metadata(
            serde_json::json!({
                "rolled_back_operation": plan.entry.id,
                "rolled_back_type": plan.entry.operation.to_string(),
                "before_absent": plan.current.is_none(),
                "after_absent": plan.restore.is_none(),
            }),
        );
        if let Some(before) = plan
            .current
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
        {
            match self.snapshot_store.store(before).await {
                Ok(id) => entry = entry.with_before(SnapshotStore::compute_hash(before), id),
                Err(e) => tracing::warn!("Failed to store the content a rollback replaced: {}", e),
            }
        }
        if let (Some(content), Some(id)) = (&plan.restore, &plan.restore_snapshot_id) {
            entry = entry.with_after(SnapshotStore::compute_hash(content), id.clone());
        }

        let id = entry.id.clone();
        // Fire-and-forget audit recording
        if let Err(e) = self.audit_log.record(&entry).await {
            tracing::warn!("Failed to record rollback audit entry: {}", e);
        }
        id
    }
}

/// What rolling back one operation does, checked against the file as it was
/// when the plan was made.
#[derive(Debug, Clone)]
pub struct RollbackPlan {
    /// The operation being rolled back.
    pub entry: AuditEntry,
    /// The content to put back, or `None` to remove the file.
    pub restore: Option<String>,
    /// The snapshot `restore` comes from.
    pub restore_snapshot_id: Option<String>,
    /// The file's bytes when the plan was made, or `None` if it was absent.
    /// A writer applying the plan should refuse if the file no longer holds
    /// them.
    pub current: Option<Vec<u8>>,
}

impl RollbackPlan {
    /// A description of what applying the plan does.
    pub fn action(&self) -> String {
        let undoing = self.entry.operation.to_string().to_lowercase();
        match (&self.restore, &self.current, &self.restore_snapshot_id) {
            (None, None, _) => "File already absent, no action taken".to_string(),
            (None, Some(_), _) => format!("Deleted file (undoing {undoing})"),
            (Some(_), _, Some(id)) => {
                format!("Restored content from snapshot {id} (undoing {undoing})")
            }
            (Some(_), _, None) => format!("Restored content (undoing {undoing})"),
        }
    }
}

/// The state rolling back an entry returns its path to.
enum Target {
    Absent,
    Snapshot(String),
}

fn target_state(entry: &AuditEntry) -> Result<Target> {
    let before_absent = entry.metadata.get("before_absent") == Some(&serde_json::Value::Bool(true));
    match (&entry.operation, &entry.before_snapshot_id) {
        (OperationType::Create, _) => Ok(Target::Absent),
        (OperationType::Update | OperationType::Delete, Some(id)) => {
            Ok(Target::Snapshot(id.clone()))
        }
        (OperationType::Update | OperationType::Delete, None) => Err(Error::Other(
            "No before-snapshot — cannot restore".to_string(),
        )),
        (OperationType::Rollback, Some(id)) => Ok(Target::Snapshot(id.clone())),
        (OperationType::Rollback, None) if before_absent => Ok(Target::Absent),
        (OperationType::Rollback, None) => Err(Error::Other(
            "This ROLLBACK operation was recorded without the content it replaced, \
             so it cannot be rolled back"
                .to_string(),
        )),
        (OperationType::Move, _) => Err(Error::Other(
            "Rollback of MOVE operations is not supported".to_string(),
        )),
    }
}

/// How the file differs from what `entry` left behind, if it does and the
/// entry recorded enough to tell.
fn changed_since(entry: &AuditEntry, current: Option<&[u8]>) -> Option<&'static str> {
    // A move's path is where the note came from; it can't be rolled back.
    if entry.operation == OperationType::Move {
        return None;
    }
    let after_absent = entry.metadata.get("after_absent") == Some(&serde_json::Value::Bool(true));
    let left_absent = matches!(entry.operation, OperationType::Delete)
        || (matches!(entry.operation, OperationType::Rollback) && after_absent);
    match current {
        Some(_) if left_absent => Some("was created again"),
        None if left_absent => None,
        // Older entries may not record the content they left.
        _ if entry.after_hash.is_none() => None,
        None => Some("was deleted"),
        Some(bytes) => (entry.after_hash.as_deref()
            != Some(&turbovault_core::compute_hash_bytes(bytes)))
        .then_some("has changed"),
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

    /// Record an update of `test.md` from `old` to `new`, with `test.md`
    /// holding `on_disk` now.
    async fn recorded_update(
        temp_dir: &tempfile::TempDir,
        audit_log: &AuditLog,
        snapshot_store: &SnapshotStore,
        on_disk: &str,
    ) -> String {
        let (old, new) = ("old\n", "new\n");
        let before = snapshot_store.store(old).await.unwrap();
        let after = snapshot_store.store(new).await.unwrap();
        tokio::fs::write(temp_dir.path().join("test.md"), on_disk)
            .await
            .unwrap();
        let entry = AuditEntry::new(OperationType::Update, "test.md")
            .with_before(SnapshotStore::compute_hash(old), before)
            .with_after(SnapshotStore::compute_hash(new), after);
        audit_log.record(&entry).await.unwrap();
        entry.id
    }

    async fn read(temp_dir: &tempfile::TempDir, path: &str) -> String {
        tokio::fs::read_to_string(temp_dir.path().join(path))
            .await
            .unwrap()
    }

    /// A file edited after the operation is not silently put back: the edit
    /// would be lost. Forcing it is the caller's explicit choice.
    #[tokio::test]
    async fn rollback_refuses_to_discard_a_later_edit_unless_forced() {
        let (temp_dir, audit_log, snapshot_store, engine) = setup_engine().await;
        let id = recorded_update(&temp_dir, &audit_log, &snapshot_store, "edited later\n").await;

        let error = engine.execute(&id, temp_dir.path()).await.unwrap_err();
        assert!(matches!(error, Error::ConcurrencyError { .. }), "{error}");
        assert!(error.to_string().contains("force"), "{error}");
        assert_eq!(read(&temp_dir, "test.md").await, "edited later\n");

        let preview = engine.preview(&id, temp_dir.path()).await.unwrap();
        assert!(
            preview.warnings.iter().any(|w| w.contains("changed since")),
            "{:?}",
            preview.warnings
        );

        engine
            .execute_checked(&id, temp_dir.path(), true)
            .await
            .unwrap();
        assert_eq!(read(&temp_dir, "test.md").await, "old\n");
    }

    /// A rollback records what it replaced, so it can be rolled back too.
    #[tokio::test]
    async fn a_rollback_can_be_rolled_back() {
        let (temp_dir, audit_log, snapshot_store, engine) = setup_engine().await;
        let id = recorded_update(&temp_dir, &audit_log, &snapshot_store, "new\n").await;

        let rollback = engine.execute(&id, temp_dir.path()).await.unwrap();
        assert_eq!(read(&temp_dir, "test.md").await, "old\n");
        let entry = audit_log
            .get_entry(&rollback.audit_entry_id)
            .await
            .unwrap()
            .unwrap();
        assert!(entry.before_snapshot_id.is_some());
        assert_eq!(entry.after_hash, Some(SnapshotStore::compute_hash("old\n")));

        engine
            .execute(&rollback.audit_entry_id, temp_dir.path())
            .await
            .unwrap();
        assert_eq!(read(&temp_dir, "test.md").await, "new\n");
    }

    /// Undoing a create deletes the file, so a file edited since is kept.
    #[tokio::test]
    async fn rolling_back_a_create_keeps_a_file_edited_since() {
        let (temp_dir, audit_log, snapshot_store, engine) = setup_engine().await;
        let created = snapshot_store.store("created\n").await.unwrap();
        let entry = AuditEntry::new(OperationType::Create, "made.md")
            .with_after(SnapshotStore::compute_hash("created\n"), created);
        audit_log.record(&entry).await.unwrap();
        tokio::fs::write(temp_dir.path().join("made.md"), "created, then edited\n")
            .await
            .unwrap();

        assert!(engine.execute(&entry.id, temp_dir.path()).await.is_err());
        assert_eq!(read(&temp_dir, "made.md").await, "created, then edited\n");
    }

    /// Undoing a delete recreates the file, so one made again since is kept.
    #[tokio::test]
    async fn rolling_back_a_delete_keeps_a_file_recreated_since() {
        let (temp_dir, audit_log, snapshot_store, engine) = setup_engine().await;
        let deleted = snapshot_store.store("deleted\n").await.unwrap();
        let entry = AuditEntry::new(OperationType::Delete, "gone.md")
            .with_before(SnapshotStore::compute_hash("deleted\n"), deleted);
        audit_log.record(&entry).await.unwrap();
        tokio::fs::write(temp_dir.path().join("gone.md"), "made again\n")
            .await
            .unwrap();

        assert!(engine.execute(&entry.id, temp_dir.path()).await.is_err());
        assert_eq!(read(&temp_dir, "gone.md").await, "made again\n");
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
            vec!["Cannot roll back a rollback recorded without the content it replaced"]
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
                .contains("ROLLBACK operation was recorded without")
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
