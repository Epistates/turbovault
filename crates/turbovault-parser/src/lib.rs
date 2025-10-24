//! # TurboVault Parser
//!
//! Obsidian Flavored Markdown (OFM) parser built on `pulldown-cmark`.
//!
//! This crate provides:
//! - Fast markdown parsing (CommonMark foundation)
//! - Frontmatter extraction (YAML)
//! - Obsidian-specific syntax: wikilinks, embeds, callouts, tasks, tags, headings
//! - Link extraction and resolution
//!
//! ## Architecture
//!
//! Parser pipeline:
//! 1. **Frontmatter Extraction**: Using regex for ---YAML---
//! 2. **Link Parsing**: Wikilinks and embeds
//! 3. **OFM Elements**: Tags, tasks, callouts, headings with regex

pub mod parsers;

pub use parsers::Parser;

pub mod prelude {
    pub use crate::parsers::Parser;
    pub use turbovault_core::prelude::*;
}
