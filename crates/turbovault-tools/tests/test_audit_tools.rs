//! Rollback through the vault's write path.

use std::sync::Arc;
use tempfile::TempDir;
use turbovault_audit::{AuditEntry, AuditLog, OperationType, SnapshotStore};
use turbovault_core::{ConfigProfile, VaultConfig};
use turbovault_tools::AuditTools;
use turbovault_vault::VaultManager;

async fn vault() -> (TempDir, VaultManager, AuditTools) {
    vault_with(|_| {}).await
}

/// [`vault`], with the vault's config shaped by `shape`.
async fn vault_with(shape: impl FnOnce(&mut VaultConfig)) -> (TempDir, VaultManager, AuditTools) {
    let temp_dir = TempDir::new().unwrap();
    let mut config = ConfigProfile::Development.create_config();
    let mut vault = VaultConfig::builder("test", temp_dir.path())
        .build()
        .unwrap();
    shape(&mut vault);
    config.vaults.push(vault);
    let audit_log = Arc::new(AuditLog::new(temp_dir.path()).await.unwrap());
    let snapshots = Arc::new(SnapshotStore::new(audit_log.snapshot_dir().to_path_buf()));
    let mut manager = VaultManager::new(config).unwrap();
    manager.set_audit_log(Arc::clone(&audit_log), Arc::clone(&snapshots));
    manager.initialize().await.unwrap();
    (temp_dir, manager, AuditTools::new(audit_log, snapshots))
}

/// An audit entry naming a protected path (a tampered log, say) is refused
/// by the same policy every write goes through.
#[tokio::test]
async fn rollback_cannot_write_a_protected_path() {
    let (temp_dir, manager, tools) = vault().await;
    let settings = temp_dir.path().join(".obsidian/app.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(&settings, "{}").unwrap();
    let entry = AuditEntry::new(OperationType::Create, ".obsidian/app.json");
    tools.audit_log().record(&entry).await.unwrap();

    assert!(tools.rollback(&manager, &entry.id, true).await.is_err());
    assert_eq!(std::fs::read_to_string(&settings).unwrap(), "{}");
}

/// A rollback is a write like any other, so a read-only vault refuses it.
#[tokio::test]
async fn read_only_vault_refuses_rollback() {
    let (temp_dir, manager, tools) = vault_with(|vault| {
        vault.allowed_operations = Some(["read".to_string()].into());
    })
    .await;
    let note = temp_dir.path().join("note.md");
    std::fs::write(&note, "new\n").unwrap();
    let before = tools.snapshot_store().store("old\n").await.unwrap();
    let after = tools.snapshot_store().store("new\n").await.unwrap();
    let entry = AuditEntry::new(OperationType::Update, "note.md")
        .with_before(SnapshotStore::compute_hash("old\n"), before)
        .with_after(SnapshotStore::compute_hash("new\n"), after);
    tools.audit_log().record(&entry).await.unwrap();

    let error = tools
        .rollback(&manager, &entry.id, false)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("allowed_operations"), "{error}");
    assert_eq!(std::fs::read_to_string(&note).unwrap(), "new\n");
}

/// A rollback is recorded once, as a rollback that can itself be undone.
#[tokio::test]
async fn rollback_is_recorded_as_an_undoable_rollback() {
    let (temp_dir, manager, tools) = vault().await;
    let note = temp_dir.path().join("note.md");
    std::fs::write(&note, "new\n").unwrap();
    let before = tools.snapshot_store().store("old\n").await.unwrap();
    let after = tools.snapshot_store().store("new\n").await.unwrap();
    let entry = AuditEntry::new(OperationType::Update, "note.md")
        .with_before(SnapshotStore::compute_hash("old\n"), before)
        .with_after(SnapshotStore::compute_hash("new\n"), after);
    tools.audit_log().record(&entry).await.unwrap();

    let result = tools.rollback(&manager, &entry.id, false).await.unwrap();
    assert_eq!(std::fs::read_to_string(&note).unwrap(), "old\n");
    let recorded = tools
        .audit_log()
        .get_entry(&result.audit_entry_id)
        .await
        .unwrap()
        .expect("the rollback's own entry");
    assert_eq!(recorded.operation, OperationType::Rollback);
    assert_eq!(
        recorded.metadata["rolled_back_operation"],
        entry.id.as_str()
    );

    tools
        .rollback(&manager, &result.audit_entry_id, false)
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&note).unwrap(), "new\n");
}
