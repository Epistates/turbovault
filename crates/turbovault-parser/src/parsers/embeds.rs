//! Embed parser: ![[Image.png]], ![[Note]]

use lazy_static::lazy_static;
use turbovault_core::{Link, LinkType, SourcePosition};
use regex::Regex;
use std::path::Path;

lazy_static! {
    /// Matches ![...]] for embedded files/notes
    static ref EMBED_PATTERN: Regex = Regex::new(r"!\[\[([^\]]+)\]\]").unwrap();
}

/// Parse all embeds from content
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
                position: SourcePosition::new(0, 0, full_match.start(), full_match.len()),
                resolved_target: None,
                is_valid: true,
            }
        })
        .collect()
}

#[cfg(test)]
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
}
