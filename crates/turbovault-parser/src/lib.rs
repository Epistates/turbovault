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
//!
//! ## Quick Start
//!
//! ```
//! use turbovault_parser::Parser;
//! use std::path::PathBuf;
//!
//! let content = r#"---
//! title: My Note
//! tags: [important, review]
//! ---
//!
//! # Heading
//!
//! [[WikiLink]] and [[Other Note#Heading]].
//!
//! - [x] Completed task
//! - [ ] Pending task
//! "#;
//!
//! let vault_path = PathBuf::from("/vault");
//! let parser = Parser::new(vault_path);
//!
//! let path = PathBuf::from("my-note.md");
//! if let Ok(result) = parser.parse_file(&path, content) {
//!     // Access parsed components
//!     if let Some(frontmatter) = &result.frontmatter {
//!         println!("Frontmatter data: {:?}", frontmatter.data);
//!     }
//!     println!("Links: {}", result.links.len());
//!     println!("Tasks: {}", result.tasks.len());
//! }
//! ```
//!
//! ## Supported OFM Features
//!
//! ### Links
//! - Wikilinks: `[[Note]]`
//! - Aliases: `[[Note|Alias]]`
//! - Block references: `[[Note#^blockid]]`
//! - Heading references: `[[Note#Heading]]`
//! - Embeds: `![[Note]]`
//!
//! ### Frontmatter
//! YAML frontmatter between `---` delimiters is extracted and parsed.
//!
//! ### Elements
//! - **Headings**: H1-H6 with level tracking
//! - **Tasks**: Markdown checkboxes with completion status
//! - **Tags**: Inline tags like `#important`
//! - **Callouts**: Obsidian callout syntax `> [!TYPE]`
//!
//! ## Performance
//!
//! The parser uses `pulldown-cmark` for the CommonMark foundation, providing:
//! - Linear time complexity O(n)
//! - Zero-copy parsing where possible
//! - Streaming-friendly architecture
//!
//! ## Error Handling
//!
//! Parsing errors are wrapped in [`turbovault_core::error::Result`]. Common errors:
//! - Invalid file paths
//! - YAML parsing failures in frontmatter
//! - Invalid unicode in content

pub mod parsers;

pub use parsers::Parser;

pub mod prelude {
    pub use crate::parsers::Parser;
    pub use turbovault_core::prelude::*;
}
