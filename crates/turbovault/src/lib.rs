//! # TurboVault Server
//!
//! Main server implementation and CLI for the Model Context Protocol (MCP) Obsidian vault manager.
//!
//! TurboVault provides a production-grade MCP server that transforms Obsidian vaults into intelligent
//! knowledge systems for AI agents. It offers advanced editing, search, graph analysis, and batch operations.
//!
//! ## Features
//!
//! - **MCP Server Framework**: Full Model Context Protocol implementation
//! - **Vault Management**: File operations, watching, and atomic transactions
//! - **Advanced Search**: Full-text search with Tantivy
//! - **Graph Analysis**: Link relationships, backlinks, and health analysis
//! - **Batch Operations**: Atomic multi-file operations with rollback
//! - **Multiple Transports**: Stdio (default), HTTP, WebSocket, TCP, Unix sockets
//! - **Export & Reporting**: JSON/CSV export for analysis results
//!
//! ## Architecture
//!
//! The crate is organized into several modules:
//!
//! - [`tools`] - MCP tool implementations for vault operations
//! - Re-exports from [`turbovault_core`] - Core types and models
//! - Re-exports from [`turbovault_tools`] - Tool framework and utilities
//!
//! ## Quick Start
//!
//! ```no_run
//! use turbovault_core::ServerConfig;
//! use turbovault_vault::VaultManager;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Initialize vault configuration
//!     let config = ServerConfig::default();
//!     
//!     // Create vault manager
//!     let _manager = VaultManager::new(config)?;
//!     
//!     Ok(())
//! }
//! ```
//!
//! ## Transport Features
//!
//! By default, the server uses stdio transport (suitable for Claude Desktop).
//! Optional transports can be enabled via Cargo features:
//!
//! - `stdio` - Standard input/output (always included)
//! - `http` - HTTP server support
//! - `websocket` - WebSocket support
//! - `tcp` - TCP socket support
//! - `unix` - Unix domain socket support
//! - `full` - All transports combined
//!
//! ## Documentation
//!
//! See the main modules for detailed API documentation:
//! - [`tools`] - Tool implementations
//! - `turbovault_core` - Core types and error handling (see <https://docs.rs/turbovault-core>)
//! - `turbovault_tools` - MCP tools framework (see <https://docs.rs/turbovault-tools>)
//! - `turbovault_vault` - Vault operations (see <https://docs.rs/turbovault-vault>)
//! - `turbovault_parser` - Markdown parsing (see <https://docs.rs/turbovault-parser>)
//! - `turbovault_graph` - Graph analysis (see <https://docs.rs/turbovault-graph>)
//! - `turbovault_batch` - Batch operations (see <https://docs.rs/turbovault-batch>)

pub mod tools;

pub use turbovault_core::prelude::*;
pub use turbovault_tools::*;
pub use tools::ObsidianMcpServer;
