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
//! ```
//! use turbovault_graph::LinkGraph;
//!
//! // Create a new link graph
//! let graph = LinkGraph::new();
//! 
//! // The graph is built by adding VaultFile objects
//! // and their links will be indexed automatically
//! println!("Nodes: {}", graph.node_count());
//! println!("Edges: {}", graph.edge_count());
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
//! ```
//! use turbovault_graph::{LinkGraph, HealthAnalyzer};
//!
//! let graph = LinkGraph::new();
//! 
//! // Create health analyzer for comprehensive analysis
//! let analyzer = HealthAnalyzer::new(&graph);
//! 
//! // Analyze vault health (includes broken link detection)
//! if let Ok(report) = analyzer.analyze() {
//!     for broken in &report.broken_links {
//!         println!("Broken: {} -> {}", 
//!             broken.source_file.display(),
//!             broken.target
//!         );
//!     }
//! }
//! ```
//!
//! ### Graph Statistics
//!
//! ```
//! use turbovault_graph::LinkGraph;
//!
//! let graph = LinkGraph::new();
//! println!("Nodes: {}", graph.node_count());
//! println!("Edges: {}", graph.edge_count());
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