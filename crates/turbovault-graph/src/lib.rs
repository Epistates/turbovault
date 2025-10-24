//! # Link Graph Analysis
//!
//! Link graph implementation for Obsidian vault relationships using petgraph.
//!
//! Provides:
//! - Directed graph of vault files and links
//! - Link resolution (wikilinks, aliases, folder paths)
//! - Backlink queries
//! - Related notes discovery (BFS)
//! - Orphan detection
//! - Cycle detection
//! - Graph statistics
//! - Vault health analysis
//! - Broken link detection
//!
//! ## Quick Start
//!
//! ```no_run
//! use turbovault_graph::prelude::*;
//! use std::path::PathBuf;
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<()> {
//!     // Create a new link graph
//!     let graph = LinkGraph::new();
//!
//!     // Add notes to the graph
//!     let note1 = PathBuf::from("notes/note1.md");
//!     let note2 = PathBuf::from("notes/note2.md");
//!     graph.add_node(&note1).await?;
//!     graph.add_node(&note2).await?;
//!
//!     // Create links between notes
//!     graph.add_link(
//!         &note1,
//!         &note2,
//!         turbovault_core::models::LinkType::WikiLink
//!     ).await?;
//!
//!     // Get graph statistics
//!     let stats = graph.stats().await?;
//!     println!("Total nodes: {}", stats.total_nodes);
//!     println!("Total edges: {}", stats.total_edges);
//!
//!     // Analyze vault health
//!     let health = graph.analyze_health().await?;
//!     println!("Health score: {}", health.health_score);
//!
//!     Ok(())
//! }
//! ```
//!
//! ## Core Concepts
//!
//! ### Nodes and Edges
//! - **Nodes**: Represent vault files (notes)
//! - **Edges**: Represent links between files
//! - **Directed**: Links flow from source to target
//!
//! ### Graph Operations
//!
//! - **Backlinks**: Find all notes linking to a given note
//! - **Forward Links**: Find all notes linked from a given note
//! - **Related Notes**: Discover related notes through BFS traversal
//! - **Orphans**: Find isolated notes with no links in or out
//!
//! ### Vault Health Metrics
//!
//! The health analyzer provides:
//! - **Health Score**: Overall vault connectivity (0-100)
//! - **Connectivity Rate**: Percentage of connected notes
//! - **Link Density**: Ratio of existing links to possible links
//! - **Broken Links**: Links to non-existent targets
//! - **Orphaned Notes**: Isolated notes with no relationships
//!
//! ## Advanced Usage
//!
//! ### Finding Broken Links
//!
//! ```no_run
//! use turbovault_graph::prelude::*;
//!
//! # async fn example() -> Result<()> {
//! let graph = LinkGraph::new();
//! let health = graph.analyze_health().await?;
//! let broken_links = health.broken_links;
//! for link in broken_links {
//!     println!("Broken: {} -> {}", link.source_file.display(), link.target);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! ### Graph Statistics
//!
//! ```no_run
//! use turbovault_graph::prelude::*;
//!
//! # async fn example() -> Result<()> {
//! let graph = LinkGraph::new();
//! let stats = graph.stats().await?;
//! println!("Avg links per node: {}", stats.avg_degree);
//! println!("Density: {}", stats.density);
//! println!("Cycles: {}", stats.cycle_count);
//! # Ok(())
//! # }
//! ```
//!
//! ## Modules
//!
//! - [`graph`] - Main LinkGraph implementation
//! - [`health`] - Vault health analysis
//!
//! ## Performance Characteristics
//!
//! Built on `petgraph` for optimal performance:
//! - Graph construction: O(n + m) where n = nodes, m = edges
//! - Backlink queries: O(degree) with caching
//! - Orphan detection: O(n)
//! - Cycle detection: O(n + m)
//! - Health analysis: O(n + m)

pub mod graph;
pub mod health;

pub use graph::{GraphStats, LinkGraph};
pub use health::{BrokenLink, HealthAnalyzer, HealthReport};
pub use turbovault_core::prelude::*;

pub mod prelude {
    pub use crate::graph::{GraphStats, LinkGraph};
    pub use crate::health::{BrokenLink, HealthAnalyzer, HealthReport};
    pub use turbovault_core::prelude::*;
}
