//! # Vault Manager
//!
//! Vault operations, file watching, and atomic operations.

pub mod atomic;
pub mod edit;
pub mod manager;
pub mod watcher;

pub use atomic::{AtomicFileOps, FileOp, TransactionResult};
pub use edit::{EditEngine, EditResult, SearchReplaceBlock, compute_hash};
pub use manager::VaultManager;
pub use turbovault_core::prelude::*;
pub use watcher::{VaultEvent, VaultWatcher, WatcherConfig};

pub mod prelude {
    pub use crate::atomic::*;
    pub use crate::edit::*;
    pub use crate::manager::*;
    pub use crate::watcher::*;
    pub use turbovault_core::prelude::*;
}
