//! Heading parser: # H1, ## H2, etc.

use lazy_static::lazy_static;
use regex::Regex;
use turbovault_core::{Heading, SourcePosition};

lazy_static! {
    /// Matches # Heading, ## Heading, etc.
    static ref HEADING_PATTERN: Regex = Regex::new(r"^(#{1,6})\s+(.+)$").unwrap();
}

/// Parse all headings from content
pub fn parse_headings(content: &str) -> Vec<Heading> {
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
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
                    position: SourcePosition::new(idx, 0, 0, full_match.len()),
                    anchor: Some(anchor),
                }
            })
        })
        .collect()
}

#[cfg(test)]
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
}
