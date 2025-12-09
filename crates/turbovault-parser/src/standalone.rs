//! Standalone markdown parsing without vault context.
//!
//! Use this when you just need to parse markdown content without
//! the full vault file management. Perfect for integration with
//! tools like treemd that need OFM parsing capabilities.
//!
//! # Example
//!
//! ```
//! use turbovault_parser::ParsedContent;
//!
//! let content = r#"---
//! title: My Note
//! ---
//!
//! # Heading
//!
//! [[WikiLink]] and [markdown](link) with #tag
//!
//! > [!NOTE] A callout
//! > With content
//! "#;
//!
//! let parsed = ParsedContent::parse(content);
//! assert!(parsed.frontmatter.is_some());
//! assert_eq!(parsed.wikilinks.len(), 1);
//! assert_eq!(parsed.markdown_links.len(), 1);
//! assert_eq!(parsed.tags.len(), 1);
//! ```

use turbovault_core::{Callout, Frontmatter, Heading, Link, Tag, TaskItem};

use crate::engine::ParseEngine;

/// Options for selective parsing.
///
/// Use this to parse only the elements you need, improving performance
/// for large documents when you don't need all OFM features.
#[derive(Debug, Clone)]
pub struct ParseOptions {
    /// Parse YAML frontmatter
    pub parse_frontmatter: bool,
    /// Parse wikilinks and embeds
    pub parse_wikilinks: bool,
    /// Parse markdown links [text](url)
    pub parse_markdown_links: bool,
    /// Parse headings (H1-H6)
    pub parse_headings: bool,
    /// Parse task items (- [ ] / - [x])
    pub parse_tasks: bool,
    /// Parse callout blocks (> [!NOTE])
    pub parse_callouts: bool,
    /// Parse inline tags (#tag)
    pub parse_tags: bool,
    /// Use full callout parsing (extracts multi-line content)
    pub full_callouts: bool,
}

impl Default for ParseOptions {
    fn default() -> Self {
        Self::all()
    }
}

impl ParseOptions {
    /// Parse all OFM elements.
    pub fn all() -> Self {
        Self {
            parse_frontmatter: true,
            parse_wikilinks: true,
            parse_markdown_links: true,
            parse_headings: true,
            parse_tasks: true,
            parse_callouts: true,
            parse_tags: true,
            full_callouts: false,
        }
    }

    /// Parse nothing - useful as a starting point for selective parsing.
    pub fn none() -> Self {
        Self {
            parse_frontmatter: false,
            parse_wikilinks: false,
            parse_markdown_links: false,
            parse_headings: false,
            parse_tasks: false,
            parse_callouts: false,
            parse_tags: false,
            full_callouts: false,
        }
    }

    /// Preset for treemd: links + headings + callouts.
    pub fn treemd() -> Self {
        Self {
            parse_frontmatter: false,
            parse_wikilinks: true,
            parse_markdown_links: true,
            parse_headings: true,
            parse_tasks: false,
            parse_callouts: true,
            parse_tags: false,
            full_callouts: true, // treemd needs full callout content
        }
    }

    /// Preset for link analysis: wikilinks + markdown links + embeds.
    pub fn links_only() -> Self {
        Self {
            parse_frontmatter: false,
            parse_wikilinks: true,
            parse_markdown_links: true,
            parse_headings: false,
            parse_tasks: false,
            parse_callouts: false,
            parse_tags: false,
            full_callouts: false,
        }
    }

    /// Builder method to enable frontmatter parsing.
    pub fn with_frontmatter(mut self) -> Self {
        self.parse_frontmatter = true;
        self
    }

    /// Builder method to enable full callout parsing.
    pub fn with_full_callouts(mut self) -> Self {
        self.full_callouts = true;
        self
    }
}

/// Parsed markdown content without vault context.
///
/// This is a lightweight alternative to `VaultFile` when you don't need
/// file metadata, backlinks, or other vault-specific features.
#[derive(Debug, Clone, Default)]
pub struct ParsedContent {
    /// YAML frontmatter if present
    pub frontmatter: Option<Frontmatter>,
    /// Document headings (H1-H6)
    pub headings: Vec<Heading>,
    /// Wikilinks: [[Note]], [[Note|alias]], [[Note#heading]]
    pub wikilinks: Vec<Link>,
    /// Embeds: ![[image.png]], ![[Note]]
    pub embeds: Vec<Link>,
    /// Standard markdown links: [text](url)
    pub markdown_links: Vec<Link>,
    /// Inline tags: #tag, #nested/tag
    pub tags: Vec<Tag>,
    /// Task items: - [ ], - [x]
    pub tasks: Vec<TaskItem>,
    /// Callout blocks: > [!NOTE]
    pub callouts: Vec<Callout>,
}

impl ParsedContent {
    /// Parse markdown content with default options (all elements).
    ///
    /// # Example
    /// ```
    /// use turbovault_parser::ParsedContent;
    ///
    /// let content = "# Title\n\n[[Link]] and #tag";
    /// let parsed = ParsedContent::parse(content);
    /// assert_eq!(parsed.headings.len(), 1);
    /// assert_eq!(parsed.wikilinks.len(), 1);
    /// assert_eq!(parsed.tags.len(), 1);
    /// ```
    pub fn parse(content: &str) -> Self {
        Self::parse_with_options(content, ParseOptions::all())
    }

    /// Parse markdown content with custom options.
    ///
    /// Use this for better performance when you only need specific elements.
    ///
    /// # Example
    /// ```
    /// use turbovault_parser::{ParsedContent, ParseOptions};
    ///
    /// let content = "# Title\n\n[[Link]] and #tag";
    /// let opts = ParseOptions::none().with_frontmatter();
    /// let parsed = ParsedContent::parse_with_options(content, opts);
    /// // Only frontmatter was parsed
    /// assert!(parsed.headings.is_empty());
    /// ```
    pub fn parse_with_options(content: &str, opts: ParseOptions) -> Self {
        let engine = ParseEngine::new(content);
        let result = engine.parse(&opts);

        Self {
            frontmatter: result.frontmatter,
            headings: result.headings,
            wikilinks: result.wikilinks,
            embeds: result.embeds,
            markdown_links: result.markdown_links,
            tags: result.tags,
            tasks: result.tasks,
            callouts: result.callouts,
        }
    }

    /// Get all links combined (wikilinks + embeds + markdown links).
    pub fn all_links(&self) -> impl Iterator<Item = &Link> {
        self.wikilinks
            .iter()
            .chain(self.embeds.iter())
            .chain(self.markdown_links.iter())
    }

    /// Check if content has any links.
    pub fn has_links(&self) -> bool {
        !self.wikilinks.is_empty() || !self.embeds.is_empty() || !self.markdown_links.is_empty()
    }

    /// Get total link count.
    pub fn link_count(&self) -> usize {
        self.wikilinks.len() + self.embeds.len() + self.markdown_links.len()
    }

    /// Check if content has any OFM elements.
    pub fn is_empty(&self) -> bool {
        self.frontmatter.is_none()
            && self.headings.is_empty()
            && self.wikilinks.is_empty()
            && self.embeds.is_empty()
            && self.markdown_links.is_empty()
            && self.tags.is_empty()
            && self.tasks.is_empty()
            && self.callouts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_complete() {
        let content = r#"---
title: Test Note
tags: [test]
---

# Heading 1

This has [[WikiLink]] and [markdown](url).

## Heading 2

- [ ] Task 1
- [x] Task 2 #tag

> [!NOTE]
> Callout content

![[image.png]]
"#;

        let parsed = ParsedContent::parse(content);

        assert!(parsed.frontmatter.is_some());
        assert_eq!(parsed.headings.len(), 2);
        assert_eq!(parsed.wikilinks.len(), 1);
        assert_eq!(parsed.markdown_links.len(), 1);
        assert_eq!(parsed.tasks.len(), 2);
        assert_eq!(parsed.tags.len(), 1);
        assert_eq!(parsed.callouts.len(), 1);
        assert_eq!(parsed.embeds.len(), 1);
    }

    #[test]
    fn test_all_links() {
        let content = "[[wiki]] and [md](url) and ![[embed]]";
        let parsed = ParsedContent::parse(content);
        assert_eq!(parsed.all_links().count(), 3);
        assert_eq!(parsed.link_count(), 3);
        assert!(parsed.has_links());
    }

    #[test]
    fn test_empty_content() {
        let parsed = ParsedContent::parse("");
        assert!(parsed.frontmatter.is_none());
        assert!(parsed.headings.is_empty());
        assert!(!parsed.has_links());
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_parse_options_none() {
        let content = "# Title\n\n[[Link]] #tag";
        let parsed = ParsedContent::parse_with_options(content, ParseOptions::none());

        assert!(parsed.frontmatter.is_none());
        assert!(parsed.headings.is_empty());
        assert!(parsed.wikilinks.is_empty());
        assert!(parsed.tags.is_empty());
    }

    #[test]
    fn test_parse_options_links_only() {
        let content = "# Title\n\n[[Link]] #tag";
        let parsed = ParsedContent::parse_with_options(content, ParseOptions::links_only());

        assert!(parsed.headings.is_empty()); // Not parsed
        assert_eq!(parsed.wikilinks.len(), 1); // Parsed
        assert!(parsed.tags.is_empty()); // Not parsed
    }

    #[test]
    fn test_parse_options_treemd() {
        let content = r#"# Title

[[Link]] #tag

> [!NOTE] Title
> Content here
"#;
        let parsed = ParsedContent::parse_with_options(content, ParseOptions::treemd());

        assert_eq!(parsed.headings.len(), 1); // Parsed
        assert_eq!(parsed.wikilinks.len(), 1); // Parsed
        assert!(parsed.tags.is_empty()); // Not parsed for treemd
        assert_eq!(parsed.callouts.len(), 1); // Parsed
        assert_eq!(parsed.callouts[0].content, "Content here"); // Full content
    }

    #[test]
    fn test_full_callouts() {
        let content = r#"> [!WARNING] Important
> Line 1
> Line 2"#;

        let simple = ParsedContent::parse_with_options(content, ParseOptions::all());
        let full =
            ParsedContent::parse_with_options(content, ParseOptions::all().with_full_callouts());

        assert!(simple.callouts[0].content.is_empty()); // Simple parsing
        assert_eq!(full.callouts[0].content, "Line 1\nLine 2"); // Full parsing
    }

    #[test]
    fn test_frontmatter_parsing() {
        let content = r#"---
title: Test
author: Alice
---

Content here"#;

        let parsed = ParsedContent::parse(content);
        let fm = parsed.frontmatter.unwrap();
        assert_eq!(fm.data.get("title").and_then(|v| v.as_str()), Some("Test"));
        assert_eq!(
            fm.data.get("author").and_then(|v| v.as_str()),
            Some("Alice")
        );
    }

    #[test]
    fn test_position_tracking() {
        let content = "Line 1\n[[Link]] on line 2";
        let parsed = ParsedContent::parse(content);

        assert_eq!(parsed.wikilinks[0].position.line, 2);
        assert_eq!(parsed.wikilinks[0].position.column, 1);
    }

    #[test]
    fn test_code_block_awareness() {
        // Patterns inside code blocks should NOT be parsed
        // This is powered by pulldown-cmark integration
        let content = r#"
Normal [[Valid Link]] here.

```rust
// This is a code block
let link = "[[Fake Link Inside Code]]";
```

Also valid: [[Another Valid Link]]
"#;

        let parsed = ParsedContent::parse(content);

        // Should only find 2 valid links, NOT the one inside the code block
        assert_eq!(parsed.wikilinks.len(), 2);
        assert_eq!(parsed.wikilinks[0].target, "Valid Link");
        assert_eq!(parsed.wikilinks[1].target, "Another Valid Link");
    }
}
