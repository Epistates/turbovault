//! # Vault Manager
//!
//! Vault operations, file watching, and atomic operations.
//!
//! This crate provides the core vault management functionality including:
//! - File reading and writing with error handling
//! - Real-time file system watching
//! - Atomic operations with transaction support
//! - Edit engine for advanced file modifications
//! - Diff-based updates with fuzzy matching
//!
//! ## Quick Start
//!
//! ```no_run
//! use turbovault_vault::VaultManager;
//! use turbovault_core::ServerConfig;
//! use std::path::PathBuf;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! // Create a vault manager
//! let config = ServerConfig::default();
//! let manager = VaultManager::new(config)?;
//!
//! // Read a file
//! let path = PathBuf::from("notes/example.md");
//! let _vault_path = manager.vault_path();
//!
//! # Ok(())
//! # }
//! ```
//!
//! ## Core Modules
//!
//! ### Manager
//!
//! [`manager::VaultManager`] is the primary interface for vault operations:
//! - Read/write files
//! - Query metadata
//! - List files
//! - Traverse directory structure
//!
//! ### File Watching
//!
//! [`watcher::VaultWatcher`] monitors the vault for changes:
//! - File creates, modifies, deletes
//! - Directory changes
//! - Debounced events
//! - Configurable filtering
//!
//! Example:
//! ```
//! use turbovault_vault::WatcherConfig;
//!
//! let _config = WatcherConfig::default();
//! // Watcher runs in background, emit events via channel
//! ```
//!
//! ### Atomic Operations
//!
//! [`atomic::AtomicFileOps`] ensures data integrity:
//! - Atomic writes (write-to-temp then rename)
//! - Transaction support
//! - Rollback on failure
//!
//! ### Edit Engine
//!
//! [`edit::EditEngine`] provides advanced editing capabilities:
//! - Search and replace with context
//! - Block-based edits
//! - Diff-based fuzzy matching
//! - Hash verification
//!
//! Example:
//! ```
//! use turbovault_vault::EditEngine;
//!
//! let _edit_engine = EditEngine::new();
//! // Use edit_engine for advanced file modifications
//! ```
//!
//! ## Thread Safety
//!
//! All components are thread-safe:
//! - `VaultManager` uses `Arc<RwLock<...>>` internally
//! - Safe to share across async tasks
//! - Concurrent read access with exclusive write access
//!
//! ## Error Handling
//!
//! All operations return [`turbovault_core::Result<T>`]:
//! - File not found errors
//! - Permission errors
//! - Invalid paths
//! - Encoding errors
//! - Atomicity violations

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
