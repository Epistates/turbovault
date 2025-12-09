//! Wikilink parser: `[[Note]]`, `[[folder/Note]]`, `[[Note#Heading]]`, `[[Note#^block]]`
//!
//! **Deprecated**: Use `turbovault_parser::parse_wikilinks()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;
use turbovault_core::{LineIndex, Link, LinkType, SourcePosition};

/// Matches [[...]] pattern
static WIKILINK_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]]+)\]\]").unwrap());

/// Parse all wikilinks from content (excludes embeds which start with !)
///
/// **Deprecated**: Use `turbovault_parser::parse_wikilinks()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_wikilinks() instead"
)]
pub fn parse_wikilinks(content: &str, source_file: &Path) -> Vec<Link> {
    WIKILINK_PATTERN
        .captures_iter(content)
        .filter_map(|caps| {
            let full_match = caps.get(0).unwrap();
            let start = full_match.start();

            // Skip if preceded by ! (it's an embed, not a wikilink)
            if start > 0 && content.as_bytes().get(start - 1) == Some(&b'!') {
                return None;
            }

            let raw_target = caps.get(1).unwrap().as_str();

            // Handle display text syntax: [[target|display_text]]
            let (target, display_text) = if let Some(pipe_idx) = raw_target.find('|') {
                let target = raw_target[..pipe_idx].to_string();
                let display = raw_target[pipe_idx + 1..].to_string();
                (target, Some(display))
            } else {
                (raw_target.to_string(), None)
            };

            Some(Link {
                type_: LinkType::WikiLink,
                source_file: source_file.to_path_buf(),
                target,
                display_text,
                position: SourcePosition::from_offset(content, start, full_match.len()),
                resolved_target: None,
                is_valid: true,
            })
        })
        .collect()
}

/// Parse wikilinks with O(log n) position lookup using pre-computed line index.
///
/// **Deprecated**: Use `turbovault_parser::parse_wikilinks()` instead (uses LineIndex internally).
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_wikilinks() instead"
)]
pub fn parse_wikilinks_indexed(content: &str, source_file: &Path, index: &LineIndex) -> Vec<Link> {
    WIKILINK_PATTERN
        .captures_iter(content)
        .filter_map(|caps| {
            let full_match = caps.get(0).unwrap();
            let start = full_match.start();

            // Skip if preceded by ! (it's an embed, not a wikilink)
            if start > 0 && content.as_bytes().get(start - 1) == Some(&b'!') {
                return None;
            }

            let raw_target = caps.get(1).unwrap().as_str();

            let (target, display_text) = if let Some(pipe_idx) = raw_target.find('|') {
                let target = raw_target[..pipe_idx].to_string();
                let display = raw_target[pipe_idx + 1..].to_string();
                (target, Some(display))
            } else {
                (raw_target.to_string(), None)
            };

            Some(Link {
                type_: LinkType::WikiLink,
                source_file: source_file.to_path_buf(),
                target,
                display_text,
                position: SourcePosition::from_offset_indexed(index, start, full_match.len()),
                resolved_target: None,
                is_valid: true,
            })
        })
        .collect()
}

/// Parse wikilinks without source file context.
///
/// **Deprecated**: Use `turbovault_parser::parse_wikilinks()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_wikilinks() instead"
)]
pub fn parse_wikilinks_standalone(content: &str) -> Vec<Link> {
    #[allow(deprecated)]
    parse_wikilinks(content, Path::new(""))
}

/// Parse wikilinks standalone with indexed positions.
///
/// **Deprecated**: Use `turbovault_parser::parse_wikilinks()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_wikilinks() instead"
)]
pub fn parse_wikilinks_standalone_indexed(content: &str, index: &LineIndex) -> Vec<Link> {
    #[allow(deprecated)]
    parse_wikilinks_indexed(content, Path::new(""), index)
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_simple_wikilink() {
        let content = "See [[Note]]";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Note");
        assert_eq!(links[0].type_, LinkType::WikiLink);
    }

    #[test]
    fn test_wikilink_with_folder() {
        let content = "See [[folder/Note]]";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "folder/Note");
    }

    #[test]
    fn test_wikilink_with_heading() {
        let content = "See [[Note#Heading]]";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Note#Heading");
    }

    #[test]
    fn test_wikilink_with_block_ref() {
        let content = "See [[Note#^block]]";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Note#^block");
    }

    #[test]
    fn test_multiple_wikilinks() {
        let content = "[[Note1]] and [[Note2]] and [[Note3]]";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 3);
        assert_eq!(links[0].target, "Note1");
        assert_eq!(links[1].target, "Note2");
        assert_eq!(links[2].target, "Note3");
    }

    #[test]
    fn test_not_embed() {
        let content = "See ![[Image.png]]";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 0); // Should not match embeds
    }

    #[test]
    fn test_wikilink_with_display_text() {
        let content = "See [[Note|Display Text]]";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Note");
        assert_eq!(links[0].display_text, Some("Display Text".to_string()));
    }

    #[test]
    fn test_wikilink_folder_with_display_text() {
        let content = "See [[capabilities/File Management|File Management]]";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "capabilities/File Management");
        assert_eq!(links[0].display_text, Some("File Management".to_string()));
    }

    #[test]
    fn test_wikilink_position_multiline() {
        let content = "Line 1\nLine 2 [[Link]] here\nLine 3";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].position.line, 2);
        assert_eq!(links[0].position.column, 8); // "Line 2 " = 7 chars + 1
        assert_eq!(links[0].position.offset, 14); // byte offset
    }

    #[test]
    fn test_wikilink_position_first_line() {
        let content = "[[Link]] at start";
        let links = parse_wikilinks(content, &PathBuf::from("test.md"));
        assert_eq!(links[0].position.line, 1);
        assert_eq!(links[0].position.column, 1);
    }

    #[test]
    fn test_wikilink_indexed_matches_regular() {
        let content = "Line 1\n[[Link1]] and [[Link2]]\nLine 3 [[Link3]]";
        let index = LineIndex::new(content);

        let regular = parse_wikilinks(content, &PathBuf::from("test.md"));
        let indexed = parse_wikilinks_indexed(content, &PathBuf::from("test.md"), &index);

        assert_eq!(regular.len(), indexed.len());
        for (r, i) in regular.iter().zip(indexed.iter()) {
            assert_eq!(r.target, i.target);
            assert_eq!(r.position.line, i.position.line);
            assert_eq!(r.position.column, i.position.column);
            assert_eq!(r.position.offset, i.position.offset);
        }
    }

    #[test]
    fn test_standalone_parsing() {
        let content = "[[Note1]] and [[Note2]]";
        let links = parse_wikilinks_standalone(content);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].source_file, PathBuf::from(""));
    }
}
