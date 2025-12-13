//! # TurboVault Parser
//!
//! Obsidian Flavored Markdown (OFM) parser built on `pulldown-cmark`.
//!
//! This crate provides:
//! - Fast markdown parsing via `pulldown-cmark` (CommonMark foundation)
//! - Frontmatter extraction (YAML via pulldown-cmark metadata blocks)
//! - Obsidian-specific syntax: wikilinks, embeds, callouts, tags
//! - **Code block awareness**: patterns inside code blocks/inline code are excluded
//! - Link extraction and resolution
//! - **Standalone parsing without vault context** (for tools like treemd)
//!
//! ## Architecture
//!
//! The parser uses a hybrid two-phase approach via unified `ParseEngine`:
//!
//! ### Phase 1: pulldown-cmark pass
//! - Extracts CommonMark elements: headings, markdown links, tasks, frontmatter
//! - Builds excluded ranges (code blocks, inline code, HTML) for Phase 2
//!
//! ### Phase 2: Regex pass (OFM extensions)
//! - Parses Obsidian-specific syntax: wikilinks `[[]]`, embeds `![[]]`, tags `#tag`, callouts
//! - **Skips excluded ranges** to avoid matching inside code blocks
//!
//! ### Performance optimizations
//! - Builds a `LineIndex` once for O(log n) position lookups
//! - Uses fast pre-filters to skip regex when patterns aren't present
//!
//! ## Quick Start
//!
//! ### With Vault Context
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
//! ### Standalone Parsing (No Vault Required)
//!
//! ```
//! use turbovault_parser::{ParsedContent, ParseOptions};
//!
//! let content = "# Title\n\n[[WikiLink]] and [markdown](url) with #tag";
//!
//! // Parse everything
//! let parsed = ParsedContent::parse(content);
//! assert_eq!(parsed.wikilinks.len(), 1);
//! assert_eq!(parsed.markdown_links.len(), 1);
//! assert_eq!(parsed.tags.len(), 1);
//!
//! // Or parse selectively for better performance
//! let parsed = ParsedContent::parse_with_options(content, ParseOptions::links_only());
//! ```
//!
//! ### Individual Parsers (Granular Control)
//!
//! ```
//! use turbovault_parser::{parse_wikilinks, parse_tags, parse_callouts};
//!
//! let content = "[[Link]] with #tag and > [!NOTE] callout";
//!
//! let wikilinks = parse_wikilinks(content);
//! let tags = parse_tags(content);
//! let callouts = parse_callouts(content);
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
//! - Markdown links: `[text](url)`
//!
//! ### Frontmatter
//! YAML frontmatter between `---` delimiters is extracted and parsed.
//!
//! ### Elements
//! - **Headings**: H1-H6 with level tracking
//! - **Tasks**: Markdown checkboxes with completion status
//! - **Tags**: Inline tags like `#important`
//! - **Callouts**: Obsidian callout syntax `> [!TYPE]` with multi-line content
//!
//! ## Performance
//!
//! The parser uses:
//! - `pulldown-cmark` for CommonMark parsing + code block detection (O(n) linear time)
//! - `std::sync::LazyLock` for compiled regex patterns (Rust 1.80+)
//! - `LineIndex` for O(log n) position lookups via binary search
//! - Fast pre-filters to skip regex when patterns aren't present
//! - Excluded range tracking to avoid parsing inside code blocks

// Core modules
mod blocks;
mod engine;
pub mod parsers;
mod standalone;

// Main exports
pub use parsers::Parser;
pub use standalone::{ParseOptions, ParsedContent};

// Re-export frontmatter extraction (deprecated but kept for backwards compatibility)
#[allow(deprecated)]
pub use parsers::frontmatter_parser::extract_frontmatter;

// Block-level parsing (for treemd integration)
pub use blocks::{parse_blocks, parse_blocks_from_line, slugify, to_plain_text};

// Re-export core types for consumers (no need to depend on turbovault-core separately)
pub use turbovault_core::{
    ContentBlock, InlineElement, LineIndex, LinkType, ListItem, SourcePosition, TableAlignment,
};

// ============================================================================
// Simplified Public API - Individual Parser Functions
// ============================================================================
//
// These functions provide granular parsing when you only need specific elements.
// They all use the unified engine internally with LineIndex for efficient position tracking.

/// Parse wikilinks from content.
///
/// Returns links with empty `source_file`. Use `Parser::parse_file()` for vault-aware parsing.
///
/// # Example
/// ```
/// use turbovault_parser::parse_wikilinks;
///
/// let links = parse_wikilinks("See [[Note]] and [[Other|alias]]");
/// assert_eq!(links.len(), 2);
/// assert_eq!(links[0].target, "Note");
/// ```
pub fn parse_wikilinks(content: &str) -> Vec<turbovault_core::Link> {
    let engine = engine::ParseEngine::new(content);
    let opts = ParseOptions {
        parse_wikilinks: true,
        ..ParseOptions::none()
    };
    engine.parse(&opts).wikilinks
}

/// Parse embeds from content.
///
/// # Example
/// ```
/// use turbovault_parser::parse_embeds;
///
/// let embeds = parse_embeds("![[image.png]] and ![[Note]]");
/// assert_eq!(embeds.len(), 2);
/// ```
pub fn parse_embeds(content: &str) -> Vec<turbovault_core::Link> {
    let engine = engine::ParseEngine::new(content);
    let opts = ParseOptions {
        parse_wikilinks: true, // Embeds are parsed with wikilinks
        ..ParseOptions::none()
    };
    engine.parse(&opts).embeds
}

/// Parse markdown links from content.
///
/// # Example
/// ```
/// use turbovault_parser::parse_markdown_links;
///
/// let links = parse_markdown_links("[text](url) and [other](http://example.com)");
/// assert_eq!(links.len(), 2);
/// ```
pub fn parse_markdown_links(content: &str) -> Vec<turbovault_core::Link> {
    let engine = engine::ParseEngine::new(content);
    let opts = ParseOptions {
        parse_markdown_links: true,
        ..ParseOptions::none()
    };
    engine.parse(&opts).markdown_links
}

/// Parse tags from content.
///
/// # Example
/// ```
/// use turbovault_parser::parse_tags;
///
/// let tags = parse_tags("Has #tag and #nested/tag");
/// assert_eq!(tags.len(), 2);
/// assert!(tags[1].is_nested);
/// ```
pub fn parse_tags(content: &str) -> Vec<turbovault_core::Tag> {
    let engine = engine::ParseEngine::new(content);
    let opts = ParseOptions {
        parse_tags: true,
        ..ParseOptions::none()
    };
    engine.parse(&opts).tags
}

/// Parse headings from content.
///
/// # Example
/// ```
/// use turbovault_parser::parse_headings;
///
/// let headings = parse_headings("# H1\n## H2\n### H3");
/// assert_eq!(headings.len(), 3);
/// assert_eq!(headings[0].level, 1);
/// ```
pub fn parse_headings(content: &str) -> Vec<turbovault_core::Heading> {
    let engine = engine::ParseEngine::new(content);
    let opts = ParseOptions {
        parse_headings: true,
        ..ParseOptions::none()
    };
    engine.parse(&opts).headings
}

/// Parse tasks from content.
///
/// # Example
/// ```
/// use turbovault_parser::parse_tasks;
///
/// let tasks = parse_tasks("- [ ] Todo\n- [x] Done");
/// assert_eq!(tasks.len(), 2);
/// assert!(!tasks[0].is_completed);
/// assert!(tasks[1].is_completed);
/// ```
pub fn parse_tasks(content: &str) -> Vec<turbovault_core::TaskItem> {
    let engine = engine::ParseEngine::new(content);
    let opts = ParseOptions {
        parse_tasks: true,
        ..ParseOptions::none()
    };
    engine.parse(&opts).tasks
}

/// Parse callouts from content (header only, no multi-line content).
///
/// # Example
/// ```
/// use turbovault_parser::parse_callouts;
///
/// let callouts = parse_callouts("> [!NOTE] Title\n> Content");
/// assert_eq!(callouts.len(), 1);
/// ```
pub fn parse_callouts(content: &str) -> Vec<turbovault_core::Callout> {
    let engine = engine::ParseEngine::new(content);
    let opts = ParseOptions {
        parse_callouts: true,
        full_callouts: false,
        ..ParseOptions::none()
    };
    engine.parse(&opts).callouts
}

/// Parse callouts with full multi-line content extraction.
///
/// # Example
/// ```
/// use turbovault_parser::parse_callouts_full;
///
/// let callouts = parse_callouts_full("> [!NOTE] Title\n> Line 1\n> Line 2");
/// assert_eq!(callouts[0].content, "Line 1\nLine 2");
/// ```
pub fn parse_callouts_full(content: &str) -> Vec<turbovault_core::Callout> {
    let engine = engine::ParseEngine::new(content);
    let opts = ParseOptions {
        parse_callouts: true,
        full_callouts: true,
        ..ParseOptions::none()
    };
    engine.parse(&opts).callouts
}

/// Convenient prelude for common imports.
///
/// Includes core types, the main parser, standalone parsing API, and all parser functions.
pub mod prelude {
    // Core types from turbovault-core
    pub use turbovault_core::{
        Callout, CalloutType, ContentBlock, Frontmatter, Heading, InlineElement, LineIndex, Link,
        LinkType, ListItem, SourcePosition, TableAlignment, Tag, TaskItem,
    };

    // Main parser
    pub use crate::Parser;

    // Standalone parsing API
    pub use crate::{ParseOptions, ParsedContent};

    // Individual parsers
    #[allow(deprecated)]
    pub use crate::{
        extract_frontmatter, parse_blocks, parse_blocks_from_line, parse_callouts,
        parse_callouts_full, parse_embeds, parse_headings, parse_markdown_links, parse_tags,
        parse_tasks, parse_wikilinks, slugify, to_plain_text,
    };
}
