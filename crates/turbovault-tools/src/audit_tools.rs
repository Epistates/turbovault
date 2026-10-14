//! Audit trail tools for querying operation history and managing rollbacks

use std::sync::Arc;
use turbovault_audit::{AuditEntry, AuditFilter, AuditLog, AuditStats};
use turbovault_audit::{RollbackEngine, RollbackPreview, RollbackResult, SnapshotStore};
use turbovault_core::ChangePlan;
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

/// Audit tools for querying and managing the operation audit trail
pub struct AuditTools {
    audit_log: Arc<AuditLog>,
    snapshot_store: Arc<SnapshotStore>,
}

impl AuditTools {
    /// Audit tools over a vault's audit log and the snapshots its entries refer to.
    pub fn new(audit_log: Arc<AuditLog>, snapshot_store: Arc<SnapshotStore>) -> Self {
        Self {
            audit_log,
            snapshot_store,
        }
    }

    /// Query audit log with filters
    pub async fn query_log(&self, filter: &AuditFilter) -> Result<Vec<AuditEntry>> {
        self.audit_log.query(filter).await
    }

    /// Get audit statistics
    pub async fn stats(&self) -> Result<AuditStats> {
        self.audit_log.stats().await
    }

    /// Preview a rollback (dry run)
    pub async fn rollback_preview(
        &self,
        operation_id: &str,
        vault_path: &std::path::Path,
    ) -> Result<RollbackPreview> {
        let engine = RollbackEngine::new(self.audit_log.clone(), self.snapshot_store.clone());
        engine.preview(operation_id, vault_path).await
    }

    /// Execute a rollback
    pub async fn rollback_execute(
        &self,
        operation_id: &str,
        vault_path: &std::path::Path,
    ) -> Result<RollbackResult> {
        let engine = RollbackEngine::new(self.audit_log.clone(), self.snapshot_store.clone());
        engine.execute(operation_id, vault_path).await
    }

    /// Roll back `operation_id` through `manager`'s write path, like any
    /// other write: the vault's write lock, its protected-path policy, and a
    /// precondition that the file still holds what the plan was checked
    /// against, so a change racing the rollback is refused rather than lost.
    /// Unless `force` is set, a file changed since the operation is refused.
    ///
    /// The write path records it as a ROLLBACK entry carrying
    /// `rolled_back_operation` and the content it replaced, so it can be
    /// rolled back in turn; its id is the result's `audit_entry_id`.
    pub async fn rollback(
        &self,
        manager: &VaultManager,
        operation_id: &str,
        force: bool,
    ) -> Result<RollbackResult> {
        let engine = RollbackEngine::new(self.audit_log.clone(), self.snapshot_store.clone());
        let plan = engine
            .plan(operation_id, manager.vault_path(), force)
            .await?;
        let path = plan.entry.path.clone();
        let result = |audit_entry_id: String| RollbackResult {
            operation_id: operation_id.to_string(),
            path: path.clone(),
            success: true,
            action_taken: plan.action(),
            audit_entry_id,
        };
        if plan.restore.is_none() && plan.current.is_none() {
            return Ok(result(String::new()));
        }

        let metadata = serde_json::json!({
            "rolled_back_operation": operation_id,
            "rolled_back_type": plan.entry.operation.to_string(),
            "before_absent": plan.current.is_none(),
            "after_absent": plan.restore.is_none(),
        });
        let mut change = ChangePlan::new(format!("Roll back {} of {}", plan.entry.operation, path))
            .with_metadata(metadata);
        change = match &plan.restore {
            Some(content) => change.upsert(path.clone(), content.clone().into_bytes()),
            None => change.remove(path.clone()),
        };
        change = match &plan.current {
            Some(bytes) => change.expect_blob(path.clone(), manager.hash_bytes(bytes)?),
            None => change.expect_absent(path.clone()),
        };
        manager.apply_changes(&change).await?.into_result()?;

        let recorded = self
            .audit_log
            .query(&AuditFilter::new().with_path(path.clone()).with_limit(20))
            .await?
            .into_iter()
            .find(|entry| {
                entry.path == path
                    && entry.metadata.get("rolled_back_operation")
                        == Some(&serde_json::Value::String(operation_id.to_string()))
            })
            .map(|entry| entry.id)
            .unwrap_or_default();
        Ok(result(recorded))
    }

    /// Get the snapshot store (for diff_note_version integration)
    pub fn snapshot_store(&self) -> &SnapshotStore {
        &self.snapshot_store
    }

    /// Get the audit log (for direct access)
    pub fn audit_log(&self) -> &AuditLog {
        &self.audit_log
    }
}
