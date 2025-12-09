//! # TurboVault Core
//!
//! Core data models, error types, and configuration for the Obsidian vault management system.
//! This crate defines the canonical types that all other crates depend on.
//!
//! ## Architecture Principles
//!
//! - **No External Crate Dependencies Beyond Serialization**: Only serde + basic Rust stdlib
//! - **Type-Driven Design**: Strong types replace string-based APIs
//! - **Zero Panic in Libraries**: All errors are Result<T, ObsidianError>
//! - **Builder Pattern for Complex Types**: Configuration structs use builders
//! - **Immutable by Default**: Mutation through explicit methods only
//!
//! ## Core Modules
//!
//! - [`models`] - Core vault data types (VaultFile, Link, Frontmatter, etc.)
//! - [`error`] - Comprehensive error types and Result aliases
//! - [`config`] - Server and vault configuration structures
//! - [`validation`] - Content validation framework
//! - [`metrics`] - Performance monitoring and statistics
//! - [`multi_vault`] - Multi-vault management support
//! - [`profiles`] - Configuration profiles for different environments
//! - [`utils`] - Utility functions and builders
//!
//! ## Usage Examples
//!
//! ### Working with Vault Files
//!
//! ```
//! use turbovault_core::prelude::*;
//! use std::path::PathBuf;
//!
//! // Create a vault file metadata
//! let metadata = FileMetadata {
//!     path: PathBuf::from("my-note.md"),
//!     size: 1024,
//!     created_at: 0.0,
//!     modified_at: 1234567890.0,
//!     checksum: "abc123".to_string(),
//!     is_attachment: false,
//! };
//! ```
//!
//! ### Error Handling
//!
//! ```
//! use turbovault_core::prelude::*;
//! use std::path::PathBuf;
//!
//! fn process_vault() -> Result<()> {
//!     // All operations return Result<T>
//!     let _path = PathBuf::from("vault.md");
//!     // Error handling with type-safe error variants
//!     let _err = Error::parse_error("Invalid markdown content");
//!     Ok(())
//! }
//! ```
//!
//! ### Configuration
//!
//! ```
//! use turbovault_core::prelude::*;
//!
//! let config = ServerConfig::default();
//! // Access configuration properties safely
//! let _config_has_vaults = !config.vaults.is_empty();
//! ```
//!
//! ## Type Safety
//!
//! The core types use enums and strong types instead of strings:
//!
//! - [`LinkType`] - Distinguishes wikilinks, embeds, markdown links, etc.
//! - [`Severity`] - Validation issue severity levels
//! - [`models`] - Rich data models with position tracking

pub mod cache;
pub mod config;
pub mod error;
pub mod metrics;
pub mod models;
pub mod multi_vault;
pub mod profiles;
pub mod resilience;
pub mod utils;
pub mod validation;

pub use config::*;
pub use error::{Error, Result};
pub use metrics::{Counter, Histogram, HistogramStats, HistogramTimer, MetricsContext};
pub use models::*;
pub use multi_vault::{MultiVaultManager, VaultInfo};
pub use profiles::ConfigProfile;
pub use utils::{CSVBuilder, PathValidator, TransactionBuilder, to_json_string};
pub use validation::{
    CompositeValidator, ContentValidator, FrontmatterValidator, LinkValidator, Severity,
    ValidationIssue, ValidationReport, ValidationSummary, Validator,
};

/// Re-export commonly used types
pub mod prelude {
    pub use crate::config::{ServerConfig, VaultConfig};
    pub use crate::error::{Error, Result};
    pub use crate::metrics::{Counter, Histogram, MetricsContext};
    pub use crate::models::{
        Block, Callout, CalloutType, ContentBlock, FileMetadata, Frontmatter, Heading,
        InlineElement, LineIndex, Link, LinkType, ListItem, SourcePosition, TableAlignment, Tag,
        TaskItem, VaultFile,
    };
    pub use crate::multi_vault::{MultiVaultManager, VaultInfo};
    pub use crate::profiles::ConfigProfile;
    pub use crate::validation::{
        CompositeValidator, ContentValidator, FrontmatterValidator, LinkValidator, Severity,
        ValidationIssue, ValidationReport, Validator,
    };
}
