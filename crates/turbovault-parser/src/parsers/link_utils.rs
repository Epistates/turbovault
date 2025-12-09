//! Shared utilities for link classification and parsing.
//!
//! This module provides common functionality used by multiple parser modules
//! to ensure consistent link type classification across the codebase.

use turbovault_core::LinkType;

/// Classify a URL into the appropriate `LinkType`.
///
/// This function handles all URL patterns:
/// - External links: `http://`, `https://`, `mailto:`
/// - Same-document anchors: `#section`
/// - Cross-file heading references: `file.md#section`
/// - Block references: `file.md#^blockid` or `#^blockid`
/// - Relative file links: `./path/file.md`
///
/// # Examples
///
/// ```
/// use turbovault_parser::parsers::link_utils::classify_url;
/// use turbovault_core::LinkType;
///
/// assert_eq!(classify_url("https://example.com"), LinkType::ExternalLink);
/// assert_eq!(classify_url("#section"), LinkType::Anchor);
/// assert_eq!(classify_url("file.md#section"), LinkType::HeadingRef);
/// assert_eq!(classify_url("#^blockid"), LinkType::BlockRef);
/// assert_eq!(classify_url("file.md#^blockid"), LinkType::BlockRef);
/// assert_eq!(classify_url("./docs/api.md"), LinkType::MarkdownLink);
/// ```
pub fn classify_url(url: &str) -> LinkType {
    // External links first (most specific match)
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
        return LinkType::ExternalLink;
    }

    // Check for anchor patterns
    if let Some(hash_pos) = url.find('#') {
        // Check if it's a block reference (contains #^)
        if url[hash_pos..].starts_with("#^") {
            return LinkType::BlockRef;
        }

        // Pure anchor (starts with #) vs file#anchor
        if hash_pos == 0 {
            LinkType::Anchor
        } else {
            LinkType::HeadingRef
        }
    } else {
        // No anchor, just a relative file link
        LinkType::MarkdownLink
    }
}

/// Classify a wikilink target into the appropriate `LinkType`.
///
/// Wikilinks can be:
/// - Simple: `[[Note]]` → WikiLink
/// - Heading reference: `[[Note#Heading]]` → HeadingRef
/// - Block reference: `[[Note#^blockid]]` → BlockRef
/// - Same-doc anchor: `[[#Heading]]` → Anchor (heading in same file)
/// - Same-doc block: `[[#^blockid]]` → BlockRef (block in same file)
///
/// # Examples
///
/// ```
/// use turbovault_parser::parsers::link_utils::classify_wikilink;
/// use turbovault_core::LinkType;
///
/// assert_eq!(classify_wikilink("Note"), LinkType::WikiLink);
/// assert_eq!(classify_wikilink("Note#Heading"), LinkType::HeadingRef);
/// assert_eq!(classify_wikilink("Note#^blockid"), LinkType::BlockRef);
/// assert_eq!(classify_wikilink("#Heading"), LinkType::Anchor);
/// assert_eq!(classify_wikilink("#^blockid"), LinkType::BlockRef);
/// ```
pub fn classify_wikilink(target: &str) -> LinkType {
    if let Some(hash_pos) = target.find('#') {
        // Check if it's a block reference (contains #^)
        if target[hash_pos..].starts_with("#^") {
            return LinkType::BlockRef;
        }

        // Pure anchor (starts with #) vs file#anchor
        if hash_pos == 0 {
            LinkType::Anchor
        } else {
            LinkType::HeadingRef
        }
    } else {
        LinkType::WikiLink
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_url_external() {
        assert_eq!(classify_url("https://example.com"), LinkType::ExternalLink);
        assert_eq!(
            classify_url("http://example.com/path"),
            LinkType::ExternalLink
        );
        assert_eq!(
            classify_url("mailto:user@example.com"),
            LinkType::ExternalLink
        );
    }

    #[test]
    fn test_classify_url_anchor() {
        assert_eq!(classify_url("#section"), LinkType::Anchor);
        assert_eq!(classify_url("#heading-with-dashes"), LinkType::Anchor);
    }

    #[test]
    fn test_classify_url_heading_ref() {
        assert_eq!(classify_url("file.md#section"), LinkType::HeadingRef);
        assert_eq!(classify_url("./path/file.md#heading"), LinkType::HeadingRef);
        assert_eq!(classify_url("note#Heading"), LinkType::HeadingRef);
    }

    #[test]
    fn test_classify_url_block_ref() {
        assert_eq!(classify_url("#^blockid"), LinkType::BlockRef);
        assert_eq!(classify_url("file.md#^blockid"), LinkType::BlockRef);
        assert_eq!(classify_url("note#^abc123"), LinkType::BlockRef);
    }

    #[test]
    fn test_classify_url_markdown_link() {
        assert_eq!(classify_url("./docs/api.md"), LinkType::MarkdownLink);
        assert_eq!(classify_url("relative/path.md"), LinkType::MarkdownLink);
        assert_eq!(classify_url("../parent/file.txt"), LinkType::MarkdownLink);
    }

    #[test]
    fn test_classify_wikilink_simple() {
        assert_eq!(classify_wikilink("Note"), LinkType::WikiLink);
        assert_eq!(classify_wikilink("My Note"), LinkType::WikiLink);
        assert_eq!(classify_wikilink("folder/Note"), LinkType::WikiLink);
    }

    #[test]
    fn test_classify_wikilink_heading() {
        assert_eq!(classify_wikilink("Note#Heading"), LinkType::HeadingRef);
        assert_eq!(
            classify_wikilink("Note#Heading with spaces"),
            LinkType::HeadingRef
        );
    }

    #[test]
    fn test_classify_wikilink_block() {
        assert_eq!(classify_wikilink("Note#^blockid"), LinkType::BlockRef);
        assert_eq!(classify_wikilink("#^blockid"), LinkType::BlockRef);
    }

    #[test]
    fn test_classify_wikilink_same_doc_anchor() {
        assert_eq!(classify_wikilink("#Heading"), LinkType::Anchor);
        assert_eq!(classify_wikilink("#Section Title"), LinkType::Anchor);
    }
}
