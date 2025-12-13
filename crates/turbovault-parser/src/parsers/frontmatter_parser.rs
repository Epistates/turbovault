//! Frontmatter extraction: ---\nYAML\n---
//!
//! **Deprecated**: Use `ParseEngine` with `frontmatter_end_offset` instead for better performance.

use regex::Regex;
use std::sync::LazyLock;
use turbovault_core::Result;

/// Matches YAML frontmatter: --- ... ---
static FRONTMATTER_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^---\s*\n([\s\S]*?)\n---\s*\n").unwrap());

/// Extract YAML frontmatter from content
///
/// Returns (frontmatter_string, content_without_frontmatter)
///
/// # Deprecated
/// This function uses regex-based extraction which is redundant when using `ParseEngine`.
/// Prefer using `ParseEngine::parse()` which returns `ParseResult::frontmatter_end_offset`
/// for zero-allocation frontmatter stripping.
#[deprecated(
    since = "1.3.0",
    note = "Use ParseEngine with frontmatter_end_offset instead for better performance"
)]
pub fn extract_frontmatter(content: &str) -> Result<(Option<String>, String)> {
    if let Some(caps) = FRONTMATTER_PATTERN.captures(content) {
        let fm_str = caps.get(1).unwrap().as_str();
        let full_match_end = caps.get(0).unwrap().end();
        let content_without_fm = content[full_match_end..].to_string();

        Ok((Some(fm_str.to_string()), content_without_fm))
    } else {
        // No frontmatter found
        Ok((None, content.to_string()))
    }
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_frontmatter() {
        let content = "---\ntitle: Test\n---\nContent here";
        let (fm, rest) = extract_frontmatter(content).unwrap();
        assert_eq!(fm, Some("title: Test".to_string()));
        assert_eq!(rest, "Content here");
    }

    #[test]
    fn test_multiline_frontmatter() {
        let content = "---\ntitle: Test\ntags:\n  - rust\n  - parser\n---\nContent";
        let (fm, _) = extract_frontmatter(content).unwrap();
        assert!(fm.is_some());
        let fm_str = fm.unwrap();
        assert!(fm_str.contains("title: Test"));
        assert!(fm_str.contains("tags:"));
    }

    #[test]
    fn test_no_frontmatter() {
        let content = "Just content\nNo frontmatter";
        let (fm, rest) = extract_frontmatter(content).unwrap();
        assert_eq!(fm, None);
        assert_eq!(rest, content);
    }

    #[test]
    fn test_frontmatter_with_empty_content() {
        let content = "---\ntitle: Test\n---\n";
        let (fm, rest) = extract_frontmatter(content).unwrap();
        assert_eq!(fm, Some("title: Test".to_string()));
        assert_eq!(rest, "");
    }

    #[test]
    fn test_malformed_frontmatter_only_opening() {
        let content = "---\ntitle: Test\nNo closing";
        let (fm, _) = extract_frontmatter(content).unwrap();
        assert_eq!(fm, None);
    }
}
