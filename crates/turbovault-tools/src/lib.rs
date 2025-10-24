//! # MCP Tools
//!
//! Tools implementation using turbomcp macros and vault manager integration.
//! Designed for LLM vault management with holistic workflows.
//!
//! ## Overview
//!
//! This crate provides the main MCP (Model Context Protocol) tool implementations
//! that enable AI agents to interact with Obsidian vaults. Tools are organized by
//! domain and include file operations, graph analysis, search, validation, and more.
//!
//! ## Core Tool Categories
//!
//! ### File Tools
//!
//! [`file_tools::FileTools`] - Direct file operations:
//! - Read file content
//! - Write/create files
//! - Delete files
//! - List vault files and directories
//! - Get file metadata
//!
//! ### Graph Tools
//!
//! [`graph_tools::GraphTools`] - Link analysis and relationships:
//! - Build vault link graph
//! - Find backlinks to a note
//! - Discover related notes
//! - Detect orphaned notes
//! - Analyze vault health
//! - Find broken links
//!
//! ### Search Tools
//!
//! [`search_tools::SearchTools`] - Full-text search capabilities:
//! - Search vault content
//! - Search file names
//! - Advanced query syntax
//! - Result ranking and filtering
//!
//! ### Analysis Tools
//!
//! [`analysis_tools::AnalysisTools`] - Vault analysis:
//! - Compute vault statistics
//! - Generate health reports
//! - Identify improvement areas
//! - Create recommendations
//!
//! ### Batch Tools
//!
//! [`batch_tools::BatchTools`] - Atomic operations:
//! - Execute multi-file operations
//! - Atomic transactions
//! - Conflict detection
//! - Result tracking
//!
//! ### Metadata Tools
//!
//! [`metadata_tools::MetadataTools`] - Note metadata:
//! - Read frontmatter
//! - Parse tags
//! - Extract headers
//! - Get file properties
//!
//! ### Validation Tools
//!
//! [`validation_tools::ValidationTools`] - Content validation:
//! - Validate frontmatter format
//! - Check link validity
//! - Verify content structure
//! - Report issues
//!
//! ### Export Tools
//!
//! [`export_tools::ExportTools`] - Data export:
//! - Export health reports
//! - Export vault statistics
//! - Export analysis results
//! - Support JSON and CSV formats
//!
//! ### Relationship Tools
//!
//! [`relationship_tools::RelationshipTools`] - Note relationships:
//! - Find note connections
//! - Build relationship maps
//! - Analyze link patterns
//!
//! ### Template Tools
//!
//! [`templates::TemplateEngine`] - Template management:
//! - Define templates
//! - Render templates
//! - Template validation
//!
//! ### Vault Lifecycle
//!
//! [`vault_lifecycle::VaultLifecycleTools`] - Vault management:
//! - Initialize vaults
//! - Backup operations
//! - Migration utilities
//!
//! ## Key Types
//!
//! - [`VaultStats`] - Vault statistics data
//! - [`HealthInfo`] - Vault health metrics
//! - [`BrokenLinkInfo`] - Broken link information
//! - [`SearchResultInfo`] - Search result details
//! - [`SearchQuery`] - Search query specification
//! - [`ValidationReportInfo`] - Validation issue report
//! - [`TemplateDefinition`] - Template specification
//!
//! ## Utilities
//!
//! ### Output Formatting
//!
//! [`output_formatter::ResponseFormatter`] - Format tool responses:
//! - JSON output
//! - Plain text output
//! - Table formatting
//! - Customizable formatting
//!
//! ### Response Utilities
//!
//! [`response_utils`] - Helper functions for response formatting
//!
//! ### Search Engine
//!
//! [`search_engine::SearchEngine`] - Tantivy-based full-text search:
//! - Index vault content
//! - Execute search queries
//! - Rank results
//!
//! ## Integration with Vault Manager
//!
//! All tools integrate with [`turbovault_vault::VaultManager`] for:
//! - File access and modification
//! - Error handling and validation
//! - Thread-safe operations
//! - Atomic transactions
//!
//! ## Example Usage
//!
//! ```no_run
//! use turbovault_core::Result;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     // Initialize tools (typically done by MCP server)
//!     // let tools = initialize_tools(&vault_path).await?;
//!
//!     // Tools are typically called by the MCP server framework
//!     // Example: FileTools::read_file(path).await?
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Error Handling
//!
//! All tools return [`turbovault_core::Result<T>`]:
//! - File not found
//! - Permission denied
//! - Parse errors
//! - Invalid input
//! - Vault errors
//!
//! See [`turbovault_core::error`] for error types.

pub mod analysis_tools;
pub mod batch_tools;
pub mod export_tools;
pub mod file_tools;
pub mod graph_tools;
pub mod metadata_tools;
pub mod output_formatter;
pub mod relationship_tools;
pub mod response_utils;
pub mod search_engine;
pub mod search_tools;
pub mod templates;
pub mod validation_tools;
pub mod vault_lifecycle;

pub use analysis_tools::{AnalysisTools, VaultStats};
pub use batch_tools::BatchTools;
pub use export_tools::ExportTools;
pub use file_tools::FileTools;
pub use graph_tools::{BrokenLinkInfo, GraphTools, HealthInfo};
pub use turbovault_batch::{BatchOperation, BatchResult};
pub use turbovault_core::prelude::*;
pub use metadata_tools::MetadataTools;
pub use output_formatter::{OutputFormat, ResponseFormatter};
pub use relationship_tools::RelationshipTools;
pub use search_engine::{SearchEngine, SearchQuery, SearchResultInfo};
pub use search_tools::SearchTools;
pub use templates::{TemplateDefinition, TemplateEngine, TemplateFieldType};
pub use validation_tools::{ValidationReportInfo, ValidationTools};
pub use vault_lifecycle::VaultLifecycleTools;
