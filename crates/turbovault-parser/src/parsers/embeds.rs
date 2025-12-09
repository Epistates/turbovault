//! Embed parser: `![[Image.png]]`, `![[Note]]`
//!
//! **Deprecated**: Use `turbovault_parser::parse_embeds()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;
use turbovault_core::{LineIndex, Link, LinkType, SourcePosition};

/// Matches ![[...]] for embedded files/notes
static EMBED_PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"!\[\[([^\]]+)\]\]").unwrap());

/// Parse all embeds from content.
///
/// **Deprecated**: Use `turbovault_parser::parse_embeds()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_embeds() instead"
)]
pub fn parse_embeds(content: &str, source_file: &Path) -> Vec<Link> {
    EMBED_PATTERN
        .captures_iter(content)
        .map(|caps| {
            let full_match = caps.get(0).unwrap();
            let raw_target = caps.get(1).unwrap().as_str();

            // Handle display text syntax: ![[target|display_text]]
            let (target, display_text) = if let Some(pipe_idx) = raw_target.find('|') {
                let target = raw_target[..pipe_idx].to_string();
                let display = raw_target[pipe_idx + 1..].to_string();
                (target, Some(display))
            } else {
                (raw_target.to_string(), None)
            };

            Link {
                type_: LinkType::Embed,
                source_file: source_file.to_path_buf(),
                target,
                display_text,
                position: SourcePosition::from_offset(
                    content,
                    full_match.start(),
                    full_match.len(),
                ),
                resolved_target: None,
                is_valid: true,
            }
        })
        .collect()
}

/// Parse embeds with O(log n) position lookup using pre-computed line index.
///
/// **Deprecated**: Use `turbovault_parser::parse_embeds()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_embeds() instead"
)]
pub fn parse_embeds_indexed(content: &str, source_file: &Path, index: &LineIndex) -> Vec<Link> {
    EMBED_PATTERN
        .captures_iter(content)
        .map(|caps| {
            let full_match = caps.get(0).unwrap();
            let raw_target = caps.get(1).unwrap().as_str();

            let (target, display_text) = if let Some(pipe_idx) = raw_target.find('|') {
                let target = raw_target[..pipe_idx].to_string();
                let display = raw_target[pipe_idx + 1..].to_string();
                (target, Some(display))
            } else {
                (raw_target.to_string(), None)
            };

            Link {
                type_: LinkType::Embed,
                source_file: source_file.to_path_buf(),
                target,
                display_text,
                position: SourcePosition::from_offset_indexed(
                    index,
                    full_match.start(),
                    full_match.len(),
                ),
                resolved_target: None,
                is_valid: true,
            }
        })
        .collect()
}

/// Parse embeds without source file context.
///
/// **Deprecated**: Use `turbovault_parser::parse_embeds()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_embeds() instead"
)]
pub fn parse_embeds_standalone(content: &str) -> Vec<Link> {
    #[allow(deprecated)]
    parse_embeds(content, Path::new(""))
}

/// Parse embeds standalone with indexed positions.
///
/// **Deprecated**: Use `turbovault_parser::parse_embeds()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_embeds() instead"
)]
pub fn parse_embeds_standalone_indexed(content: &str, index: &LineIndex) -> Vec<Link> {
    #[allow(deprecated)]
    parse_embeds_indexed(content, Path::new(""), index)
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_simple_embed() {
        let content = "See ![[Image.png]]";
        let links = parse_embeds(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "Image.png");
        assert_eq!(links[0].type_, LinkType::Embed);
    }

    #[test]
    fn test_embed_note() {
        let content = "Embed: ![[OtherNote]]";
        let links = parse_embeds(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "OtherNote");
    }

    #[test]
    fn test_embed_with_folder() {
        let content = "See ![[attachments/image.jpg]]";
        let links = parse_embeds(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "attachments/image.jpg");
    }

    #[test]
    fn test_multiple_embeds() {
        let content = "![[img1.png]] and ![[img2.png]]";
        let links = parse_embeds(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 2);
    }

    #[test]
    fn test_embed_position_multiline() {
        let content = "Line 1\nLine 2 ![[image.png]] here\nLine 3";
        let links = parse_embeds(content, &PathBuf::from("test.md"));
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].position.line, 2);
        assert_eq!(links[0].position.column, 8); // "Line 2 " = 7 chars + 1
    }

    #[test]
    fn test_embed_position_first_line() {
        let content = "![[image.png]] at start";
        let links = parse_embeds(content, &PathBuf::from("test.md"));
        assert_eq!(links[0].position.line, 1);
        assert_eq!(links[0].position.column, 1);
    }

    #[test]
    fn test_embed_indexed_matches_regular() {
        let content = "Line 1\n![[img1.png]] and ![[img2.png]]\nLine 3";
        let index = LineIndex::new(content);

        let regular = parse_embeds(content, &PathBuf::from("test.md"));
        let indexed = parse_embeds_indexed(content, &PathBuf::from("test.md"), &index);

        assert_eq!(regular.len(), indexed.len());
        for (r, i) in regular.iter().zip(indexed.iter()) {
            assert_eq!(r.target, i.target);
            assert_eq!(r.position.line, i.position.line);
            assert_eq!(r.position.column, i.position.column);
        }
    }

    #[test]
    fn test_standalone_parsing() {
        let content = "![[image.png]]";
        let links = parse_embeds_standalone(content);
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].source_file, PathBuf::from(""));
    }
}
