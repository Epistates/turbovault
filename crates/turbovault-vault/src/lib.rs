//! # Vault Manager
//!
//! Vault operations: reading, writing and editing notes, keeping the derived
//! state (note cache and link graph) fresh, and dispatching writes to the
//! configured backend.
//!
//! This crate provides:
//! - File reading and writing with error handling
//! - The freshness gate, which notices changes made outside TurboVault
//! - Write dispatch to a Direct or Git backend ([`substrate::WriteSubstrate`])
//! - Edit engine for SEARCH/REPLACE edits with fuzzy matching
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
//! ### Write substrate
//!
//! [`substrate::WriteSubstrate`] applies every change plan, either directly
//! (temp file and atomic rename) or as one Git commit.
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

pub mod edit;
pub mod manager;
pub mod reindex;
pub mod substrate;

pub use edit::{EditEngine, EditResult, SearchReplaceBlock, compute_hash};
pub use manager::{ChangeListener, ScannedNote, VaultManager};
pub use reindex::{
    CommitOrigin, PendingCommit, ReindexQueue, apply_commit_diff, watch_ref_changes,
};
pub use substrate::{ApplyOutcome, DirectSubstrate, GitSubstrate, WriteSubstrate};

pub mod prelude {
    pub use crate::edit::*;
    pub use crate::manager::*;
    pub use crate::reindex::*;
    pub use crate::substrate::*;
    pub use turbovault_core::prelude::*;
}
