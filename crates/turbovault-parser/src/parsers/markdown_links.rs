//! Markdown link parser: `[text](url)`, `[text](url "title")`, `[text](<url with spaces>)`
//!
//! **Deprecated**: Use `turbovault_parser::parse_markdown_links()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use regex::Regex;
use std::sync::LazyLock;
use turbovault_core::{LineIndex, Link, SourcePosition};

use super::link_utils::classify_url;

// Re-export for tests
#[cfg(test)]
use turbovault_core::LinkType;

/// Matches markdown links:
/// - `[text](url)`
/// - `[text](url "title")`
/// - `[text](<url with spaces>)`
///
/// Note: Images (`![alt](url)`) are filtered out after matching.
static MARKDOWN_LINK: LazyLock<Regex> = LazyLock::new(|| {
    // This regex handles:
    // - Basic links: [text](url)
    // - Links with titles: [text](url "title")
    // - Links with angle-bracketed URLs: [text](<url with spaces>)
    // Note: We can't use look-behind in Rust's regex, so we filter images manually
    Regex::new(
        r#"\[(?P<text>[^\[\]]*(?:\[[^\[\]]*\][^\[\]]*)*)\]\((?:(?P<angle><[^>]+>)|(?P<url>[^()\s"]+))(?:\s+"(?P<title>[^"]*)")?\)"#,
    )
    .unwrap()
});

/// Fast pre-filter: skip regex if no markdown link pattern exists.
#[inline]
fn has_markdown_link(content: &str) -> bool {
    content.contains("](")
}

/// Parse all markdown links from content.
///
/// **Deprecated**: Use `turbovault_parser::parse_markdown_links()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_markdown_links() instead"
)]
pub fn parse_markdown_links(content: &str) -> Vec<Link> {
    if !has_markdown_link(content) {
        return Vec::new();
    }

    MARKDOWN_LINK
        .captures_iter(content)
        .filter_map(|caps| {
            let full_match = caps.get(0).unwrap();
            let start = full_match.start();

            // Skip if preceded by ! (it's an image, not a link)
            // Rust's regex doesn't support look-behind, so we filter manually
            if start > 0 && content.as_bytes().get(start - 1) == Some(&b'!') {
                return None;
            }

            let text = caps.name("text").map(|m| m.as_str()).unwrap_or("");

            // URL is either in angle brackets or plain
            let url = caps
                .name("angle")
                .map(|m| {
                    // Strip < and > from angle-bracketed URL
                    let s = m.as_str();
                    &s[1..s.len() - 1]
                })
                .or_else(|| caps.name("url").map(|m| m.as_str()))
                .unwrap_or("");

            let link_type = classify_url(url);
            let position = SourcePosition::from_offset(content, start, full_match.len());

            Some(Link {
                type_: link_type,
                source_file: std::path::PathBuf::new(),
                target: url.to_string(),
                display_text: Some(text.to_string()),
                position,
                resolved_target: None,
                is_valid: true,
            })
        })
        .collect()
}

/// Parse markdown links with O(log n) position lookup using pre-computed line index.
///
/// **Deprecated**: Use `turbovault_parser::parse_markdown_links()` instead (uses LineIndex internally).
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_markdown_links() instead"
)]
pub fn parse_markdown_links_indexed(content: &str, index: &LineIndex) -> Vec<Link> {
    if !has_markdown_link(content) {
        return Vec::new();
    }

    MARKDOWN_LINK
        .captures_iter(content)
        .filter_map(|caps| {
            let full_match = caps.get(0).unwrap();
            let start = full_match.start();

            // Skip if preceded by ! (it's an image, not a link)
            if start > 0 && content.as_bytes().get(start - 1) == Some(&b'!') {
                return None;
            }

            let text = caps.name("text").map(|m| m.as_str()).unwrap_or("");

            let url = caps
                .name("angle")
                .map(|m| {
                    let s = m.as_str();
                    &s[1..s.len() - 1]
                })
                .or_else(|| caps.name("url").map(|m| m.as_str()))
                .unwrap_or("");

            let link_type = classify_url(url);
            let position = SourcePosition::from_offset_indexed(index, start, full_match.len());

            Some(Link {
                type_: link_type,
                source_file: std::path::PathBuf::new(),
                target: url.to_string(),
                display_text: Some(text.to_string()),
                position,
                resolved_target: None,
                is_valid: true,
            })
        })
        .collect()
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_link() {
        let content = "See [example](https://example.com) for more.";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "https://example.com");
        assert_eq!(links[0].display_text, Some("example".to_string()));
        assert_eq!(links[0].type_, LinkType::ExternalLink);
    }

    #[test]
    fn test_relative_link() {
        let content = "See [docs](./docs/api.md) for API.";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "./docs/api.md");
        assert_eq!(links[0].type_, LinkType::MarkdownLink);
    }

    #[test]
    fn test_anchor_link() {
        let content = "Jump to [section](#installation).";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "#installation");
        assert_eq!(links[0].type_, LinkType::Anchor);
    }

    #[test]
    fn test_link_with_heading() {
        let content = "See [API](docs/api.md#methods) reference.";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "docs/api.md#methods");
        assert_eq!(links[0].type_, LinkType::HeadingRef);
    }

    #[test]
    fn test_not_image() {
        let content = "![alt](image.png) and [link](url)";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "url");
    }

    #[test]
    fn test_link_with_spaces() {
        let content = "See [doc](<path/to/my file.md>) here.";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "path/to/my file.md");
    }

    #[test]
    fn test_link_with_title() {
        let content = r#"See [example](url "Title text") here."#;
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "url");
    }

    #[test]
    fn test_position_tracking() {
        let content = "Line 1\n[link](url) on line 2";
        let links = parse_markdown_links(content);
        assert_eq!(links[0].position.line, 2);
        assert_eq!(links[0].position.column, 1);
    }

    #[test]
    fn test_multiple_links() {
        let content = "[a](1) and [b](2) and [c](3)";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 3);
    }

    #[test]
    fn test_no_links() {
        let content = "No links here, just plain text.";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 0);
    }

    #[test]
    fn test_mailto_link() {
        let content = "Contact [me](mailto:test@example.com)";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].type_, LinkType::ExternalLink);
    }

    #[test]
    fn test_indexed_matches_regular() {
        let content = "Line 1\n[link1](url1) and [link2](url2)\nLine 3";
        let index = LineIndex::new(content);

        let regular = parse_markdown_links(content);
        let indexed = parse_markdown_links_indexed(content, &index);

        assert_eq!(regular.len(), indexed.len());
        for (r, i) in regular.iter().zip(indexed.iter()) {
            assert_eq!(r.target, i.target);
            assert_eq!(r.position.line, i.position.line);
            assert_eq!(r.position.column, i.position.column);
        }
    }

    #[test]
    fn test_fast_path_no_links() {
        // This should hit the fast path and skip regex entirely
        let content = "No links here, just text without the special pattern.";
        let links = parse_markdown_links(content);
        assert_eq!(links.len(), 0);
    }
}
