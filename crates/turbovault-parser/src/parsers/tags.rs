//! Tag parser: #tag, #parent/child
//!
//! **Deprecated**: Use `turbovault_parser::parse_tags()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use turbovault_core::{LineIndex, Tag};

/// Parse all tags from content.
///
/// **Deprecated**: Use `turbovault_parser::parse_tags()` instead.
#[deprecated(since = "1.2.0", note = "Use turbovault_parser::parse_tags() instead")]
pub fn parse_tags(content: &str) -> Vec<Tag> {
    crate::parse_tags(content)
}

/// Parse tags with O(log n) position lookup using pre-computed line index.
///
/// **Deprecated**: Use `turbovault_parser::parse_tags()` instead (uses LineIndex internally).
#[deprecated(since = "1.2.0", note = "Use turbovault_parser::parse_tags() instead")]
pub fn parse_tags_indexed(content: &str, _index: &LineIndex) -> Vec<Tag> {
    crate::parse_tags(content)
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

    #[test]
    fn test_markdown_anchor_link_fragment_not_tag() {
        let content = "Jump to [section](#installation) and keep #real-tag";
        let tags = parse_tags(content);

        let names: Vec<&str> = tags.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, vec!["real-tag"]);
    }

    #[test]
    fn test_same_doc_wikilink_anchor_not_tag() {
        let content = "See [[#Heading]] and ![[#Preview]] but keep #real";
        let tags = parse_tags(content);

        let names: Vec<&str> = tags.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }

    #[test]
    fn test_code_block_tag_not_matched() {
        let content = "```md\n#not-a-tag\n```\n\n#real";
        let tags = parse_tags(content);

        let names: Vec<&str> = tags.iter().map(|tag| tag.name.as_str()).collect();
        assert_eq!(names, vec!["real"]);
    }
}
