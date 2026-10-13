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

pub use log::{
    AuditContext, AuditEntry, AuditFilter, AuditLog, AuditStats, ChainVerification, OperationType,
};
pub use rollback::{RollbackEngine, RollbackPlan, RollbackPreview, RollbackResult};
pub use snapshot::SnapshotStore;
