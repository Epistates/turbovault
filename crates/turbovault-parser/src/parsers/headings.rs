//! Heading parser: # H1, ## H2, etc.
//!
//! **Deprecated**: Use `turbovault_parser::parse_headings()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use regex::Regex;
use std::sync::LazyLock;
use turbovault_core::{Heading, LineIndex, SourcePosition};

/// Matches # Heading, ## Heading, etc.
static HEADING_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.+)$").unwrap());

/// Fast pre-filter: skip regex if no heading pattern exists.
#[inline]
fn has_heading(content: &str) -> bool {
    content.contains('#')
}

/// Parse all headings from content.
///
/// **Deprecated**: Use `turbovault_parser::parse_headings()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_headings() instead"
)]
pub fn parse_headings(content: &str) -> Vec<Heading> {
    if !has_heading(content) {
        return Vec::new();
    }

    let mut offset = 0;
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line_start = offset;
            offset += line.len() + 1; // +1 for newline

            HEADING_PATTERN.captures(line).map(|caps| {
                let level = caps.get(1).unwrap().as_str().len() as u8;
                let text = caps.get(2).unwrap().as_str();
                let full_match = caps.get(0).unwrap();

                // Generate anchor from heading text (lowercase, spaces to hyphens)
                let anchor = text
                    .to_lowercase()
                    .chars()
                    .map(|c| if c.is_whitespace() { '-' } else { c })
                    .filter(|c| c.is_alphanumeric() || *c == '-')
                    .collect::<String>();

                Heading {
                    text: text.to_string(),
                    level,
                    position: SourcePosition::new(idx + 1, 1, line_start, full_match.len()),
                    anchor: Some(anchor),
                }
            })
        })
        .collect()
}

/// Parse headings with pre-computed line index (for consistency with other parsers).
///
/// **Deprecated**: Use `turbovault_parser::parse_headings()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_headings() instead"
)]
#[allow(deprecated)]
pub fn parse_headings_indexed(content: &str, _index: &LineIndex) -> Vec<Heading> {
    // Line-based parsing doesn't benefit from LineIndex, but we accept it for API consistency
    parse_headings(content)
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn test_h1_heading() {
        let content = "# Main Title";
        let headings = parse_headings(content);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[0].text, "Main Title");
    }

    #[test]
    fn test_h2_heading() {
        let content = "## Section";
        let headings = parse_headings(content);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].level, 2);
    }

    #[test]
    fn test_heading_anchor_generation() {
        let content = "# This is a Long Title!";
        let headings = parse_headings(content);
        assert_eq!(headings[0].anchor, Some("this-is-a-long-title".to_string()));
    }

    #[test]
    fn test_multiple_headings() {
        let content = "# H1\n## H2\n### H3\n## H2-2";
        let headings = parse_headings(content);
        assert_eq!(headings.len(), 4);
        assert_eq!(headings[0].level, 1);
        assert_eq!(headings[1].level, 2);
        assert_eq!(headings[2].level, 3);
    }

    #[test]
    fn test_all_heading_levels() {
        for level in 1..=6 {
            let hashes = "#".repeat(level);
            let content = format!("{} Heading", hashes);
            let headings = parse_headings(&content);
            assert_eq!(headings.len(), 1);
            assert_eq!(headings[0].level, level as u8);
        }
    }

    #[test]
    fn test_heading_position_tracking() {
        let content = "Some text\n# Heading on line 2\nMore text";
        let headings = parse_headings(content);
        assert_eq!(headings.len(), 1);
        assert_eq!(headings[0].position.line, 2);
        assert_eq!(headings[0].position.column, 1);
        assert_eq!(headings[0].position.offset, 10); // "Some text\n" = 10 chars
    }

    #[test]
    fn test_heading_position_first_line() {
        let content = "# First heading";
        let headings = parse_headings(content);
        assert_eq!(headings[0].position.line, 1);
        assert_eq!(headings[0].position.column, 1);
        assert_eq!(headings[0].position.offset, 0);
    }

    #[test]
    fn test_fast_path_no_headings() {
        let content = "No headings here, just plain text without the hash symbol.";
        let headings = parse_headings(content);
        assert_eq!(headings.len(), 0);
    }

    #[test]
    fn test_indexed_matches_regular() {
        let content = "Text\n# H1\n## H2";
        let index = LineIndex::new(content);

        let regular = parse_headings(content);
        let indexed = parse_headings_indexed(content, &index);

        assert_eq!(regular.len(), indexed.len());
        for (r, i) in regular.iter().zip(indexed.iter()) {
            assert_eq!(r.text, i.text);
            assert_eq!(r.position.line, i.position.line);
            assert_eq!(r.position.offset, i.position.offset);
        }
    }
}
