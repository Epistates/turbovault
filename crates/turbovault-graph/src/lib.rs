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
