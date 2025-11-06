//! Wikilink parser: `[[Note]]`, `[[folder/Note]]`, `[[Note#Heading]]`, `[[Note#^block]]`

use lazy_static::lazy_static;
use regex::Regex;
use std::path::Path;
use turbovault_core::{Link, LinkType, SourcePosition};

lazy_static! {
    /// Matches [[...]] pattern
    static ref WIKILINK_PATTERN: Regex = Regex::new(r"\[\[([^\]]+)\]\]").unwrap();
}

/// Parse all wikilinks from content (excludes embeds which start with !)
pub fn parse_wikilinks(content: &str, source_file: &Path) -> Vec<Link> {
    WIKILINK_PATTERN
        .captures_iter(content)
        .filter_map(|caps| {
            let full_match = caps.get(0).unwrap();
            let start = full_match.start();

            // Skip if preceded by ! (it's an embed, not a wikilink)
            if start > 0 && content.chars().nth(start - 1) == Some('!') {
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
                position: SourcePosition::new(0, 0, start, full_match.len()),
                resolved_target: None,
                is_valid: true,
            })
        })
        .collect()
}

#[cfg(test)]
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
}
