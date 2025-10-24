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
        Block, Callout, FileMetadata, Frontmatter, Heading, Link, LinkType, SourcePosition, Tag,
        TaskItem, VaultFile,
    };
    pub use crate::multi_vault::{MultiVaultManager, VaultInfo};
    pub use crate::profiles::ConfigProfile;
    pub use crate::validation::{
        CompositeValidator, ContentValidator, FrontmatterValidator, LinkValidator, Severity,
        ValidationIssue, ValidationReport, Validator,
    };
}
