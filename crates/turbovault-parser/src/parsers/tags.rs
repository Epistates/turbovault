//! Tag parser: #tag, #parent/child
//!
//! **Deprecated**: Use `turbovault_parser::parse_tags()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use regex::Regex;
use std::sync::LazyLock;
use turbovault_core::{LineIndex, SourcePosition, Tag};

/// Matches #tag or #parent/child tags
static TAG_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"#([a-zA-Z0-9_\-/]+)").unwrap());

/// Parse all tags from content.
///
/// **Deprecated**: Use `turbovault_parser::parse_tags()` instead.
#[deprecated(since = "1.2.0", note = "Use turbovault_parser::parse_tags() instead")]
pub fn parse_tags(content: &str) -> Vec<Tag> {
    TAG_PATTERN
        .captures_iter(content)
        .map(|caps| {
            let full_match = caps.get(0).unwrap();
            let name = caps.get(1).unwrap().as_str();
            let is_nested = name.contains('/');

            Tag {
                name: name.to_string(),
                position: SourcePosition::from_offset(
                    content,
                    full_match.start(),
                    full_match.len(),
                ),
                is_nested,
            }
        })
        .collect()
}

/// Parse tags with O(log n) position lookup using pre-computed line index.
///
/// **Deprecated**: Use `turbovault_parser::parse_tags()` instead (uses LineIndex internally).
#[deprecated(since = "1.2.0", note = "Use turbovault_parser::parse_tags() instead")]
pub fn parse_tags_indexed(content: &str, index: &LineIndex) -> Vec<Tag> {
    TAG_PATTERN
        .captures_iter(content)
        .map(|caps| {
            let full_match = caps.get(0).unwrap();
            let name = caps.get(1).unwrap().as_str();
            let is_nested = name.contains('/');

            Tag {
                name: name.to_string(),
                position: SourcePosition::from_offset_indexed(
                    index,
                    full_match.start(),
                    full_match.len(),
                ),
                is_nested,
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_tag() {
        let content = "This is #rust code";
        let tags = parse_tags(content);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "rust");
        assert!(!tags[0].is_nested);
    }

    #[test]
    fn test_nested_tag() {
        let content = "Tagged as #project/obsidian";
        let tags = parse_tags(content);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].name, "project/obsidian");
        assert!(tags[0].is_nested);
    }

    #[test]
    fn test_multiple_tags() {
        let content = "#rust #async #mcp";
        let tags = parse_tags(content);
        assert_eq!(tags.len(), 3);
    }

    #[test]
    fn test_tag_position_tracking() {
        let content = "First line\nSecond #tag here";
        let tags = parse_tags(content);
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].position.line, 2);
        assert_eq!(tags[0].position.column, 8); // "Second " = 7 chars + 1
    }

    #[test]
    fn test_tag_position_first_line() {
        let content = "#tag at start";
        let tags = parse_tags(content);
        assert_eq!(tags[0].position.line, 1);
        assert_eq!(tags[0].position.column, 1);
    }

    #[test]
    fn test_tag_indexed_matches_regular() {
        let content = "Line 1\n#tag1 and #tag2\nLine 3 #tag3";
        let index = LineIndex::new(content);

        let regular = parse_tags(content);
        let indexed = parse_tags_indexed(content, &index);

        assert_eq!(regular.len(), indexed.len());
        for (r, i) in regular.iter().zip(indexed.iter()) {
            assert_eq!(r.name, i.name);
            assert_eq!(r.position.line, i.position.line);
            assert_eq!(r.position.column, i.position.column);
        }
    }
}
