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

/// Split the inside of a wikilink or embed into its target and display text.
///
/// The first `|` separates them: `[[Note|Shown]]`. Inside a Markdown table a
/// bare `|` would end the cell, so Obsidian writes it escaped as `\|`
/// (`[[Note\|Shown]]`, `![[image.png\|300]]`) and reads it the same way; the
/// backslash belongs to neither half.
///
/// # Examples
///
/// ```
/// use turbovault_parser::parsers::link_utils::split_wikilink;
///
/// assert_eq!(split_wikilink("Note"), ("Note".to_string(), None));
/// assert_eq!(split_wikilink("Note|Shown"), ("Note".to_string(), Some("Shown".to_string())));
/// assert_eq!(split_wikilink("Note\\|Shown"), ("Note".to_string(), Some("Shown".to_string())));
/// ```
pub fn split_wikilink(inner: &str) -> (String, Option<String>) {
    match inner.find('|') {
        Some(pipe) => {
            let target = &inner[..pipe];
            let target = target.strip_suffix('\\').unwrap_or(target);
            (target.to_string(), Some(inner[pipe + 1..].to_string()))
        }
        None => (inner.to_string(), None),
    }
}

/// Characters a Markdown link destination cannot carry as they are: a space
/// ends the destination, parentheses and angle brackets end or open it, `#`
/// starts the fragment, and `%` starts an escape. Obsidian writes a link to
/// `My Note.md` as `My%20Note.md`.
const LINK_PATH_ESCAPES: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'%')
    .add(b'(')
    .add(b')')
    .add(b'<')
    .add(b'>')
    .add(b'#');

/// The target a Markdown link destination names, with its percent escapes
/// decoded: `My%20Note.md` is the note `My Note.md`. External URLs are kept
/// as written, since they are the browser's to interpret. A destination
/// whose escapes do not decode to UTF-8 is also kept as written.
///
/// # Examples
///
/// ```
/// use turbovault_parser::parsers::link_utils::markdown_link_target;
///
/// assert_eq!(markdown_link_target("My%20Note.md#Part%20Two"), "My Note.md#Part Two");
/// assert_eq!(markdown_link_target("https://example.com/a%20b"), "https://example.com/a%20b");
/// ```
pub fn markdown_link_target(destination: &str) -> String {
    if classify_url(destination) == LinkType::ExternalLink {
        return destination.to_string();
    }
    percent_encoding::percent_decode_str(destination)
        .decode_utf8()
        .map_or_else(|_| destination.to_string(), |decoded| decoded.into_owned())
}

/// A vault path written as a Markdown link destination, escaped the way
/// Obsidian writes one and the way [`markdown_link_target`] reads it back.
///
/// # Examples
///
/// ```
/// use turbovault_parser::parsers::link_utils::markdown_link_path;
///
/// assert_eq!(markdown_link_path("Archive/My Note.md"), "Archive/My%20Note.md");
/// ```
pub fn markdown_link_path(path: &str) -> String {
    percent_encoding::utf8_percent_encode(path, LINK_PATH_ESCAPES).to_string()
}

/// Where a Markdown link destination written at byte `start` of `text` ends:
/// past the `>` of `<My Note.md>`, or for an unbracketed destination at the
/// first whitespace or at the `)` that closes the link (parentheses inside
/// it must balance). `None` when there is no destination at `start`.
///
/// # Examples
///
/// ```
/// use turbovault_parser::parsers::link_utils::link_destination_end;
///
/// let link = "[x](a(1).md \"title\")";
/// assert_eq!(&link[4..link_destination_end(link, 4).unwrap()], "a(1).md");
/// assert_eq!(link_destination_end("<My Note.md>", 0), Some(12));
/// ```
pub fn link_destination_end(text: &str, start: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut end = start;
    if bytes.get(start) == Some(&b'<') {
        while *bytes.get(end)? != b'>' {
            end += if bytes[end] == b'\\' { 2 } else { 1 };
        }
        return Some(end + 1);
    }
    let mut parens = 0usize;
    while let Some(&byte) = bytes.get(end) {
        match byte {
            b'\\' => end += 1,
            b'(' => parens += 1,
            b')' if parens == 0 => break,
            b')' => parens -= 1,
            b' ' | b'\t' | b'\r' | b'\n' => break,
            _ => {}
        }
        end += 1;
    }
    let end = end.min(bytes.len());
    (end > start).then_some(end)
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
