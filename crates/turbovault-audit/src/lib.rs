//! # TurboVault Audit
//!
//! Operation audit trail, content snapshots, and rollback for TurboVault vaults.
//!
//! Provides:
//! - **AuditLog**: Append-only JSONL operation log (CREATE, UPDATE, DELETE, MOVE)
//! - **SnapshotStore**: Content-addressed storage for note snapshots (SHA-256 dedup)
//! - **RollbackEngine**: Point-in-time rollback with dry-run preview

pub mod log;
pub mod rollback;
pub mod snapshot;

pub use log::{AuditEntry, AuditFilter, AuditLog, AuditStats, OperationType};
pub use rollback::{RollbackEngine, RollbackPlan, RollbackPreview, RollbackResult};
pub use snapshot::SnapshotStore;
