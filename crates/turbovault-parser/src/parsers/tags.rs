//! Tag parser: #tag, #parent/child

use lazy_static::lazy_static;
use regex::Regex;
use turbovault_core::{SourcePosition, Tag};

lazy_static! {
    /// Matches #tag or #parent/child tags
    static ref TAG_PATTERN: Regex = Regex::new(r"#([a-zA-Z0-9_\-/]+)").unwrap();
}

/// Parse all tags from content
pub fn parse_tags(content: &str) -> Vec<Tag> {
    TAG_PATTERN
        .captures_iter(content)
        .map(|caps| {
            let full_match = caps.get(0).unwrap();
            let name = caps.get(1).unwrap().as_str();
            let is_nested = name.contains('/');

            Tag {
                name: name.to_string(),
                position: SourcePosition::new(0, 0, full_match.start(), full_match.len()),
                is_nested,
            }
        })
        .collect()
}

#[cfg(test)]
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
}
