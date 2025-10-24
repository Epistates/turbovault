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
//! use turbovault_vault::prelude::*;
//! use std::path::PathBuf;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     // Create a vault manager
//!     let vault_path = PathBuf::from("/path/to/vault");
//!     let manager = VaultManager::new(&vault_path, Default::default()).await?;
//!
//!     // Read a file
//!     let content = manager.read_file(&PathBuf::from("notes/example.md")).await?;
//!     println!("Content: {}", content);
//!
//!     // Write a file
//!     manager.write_file(&PathBuf::from("notes/new.md"), "# Hello\n").await?;
//!
//!     Ok(())
//! }
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
//! ```no_run
//! use turbovault_vault::prelude::*;
//!
//! # async fn example() -> Result<()> {
//! let watcher = VaultWatcher::new("/path/to/vault", Default::default()).await?;
//! // Watcher runs in background, emit events via channel
//! # Ok(())
//! # }
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
//! ```no_run
//! use turbovault_vault::prelude::*;
//!
//! # async fn example() -> Result<()> {
//! let edit_engine = EditEngine::new();
//! let original = "# Old Title\nContent";
//! let modified = edit_engine.apply_edits(
//!     original,
//!     "# New Title\nContent"
//! ).await?;
//! # Ok(())
//! # }
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
