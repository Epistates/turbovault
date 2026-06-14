//! Citation parsing for the OKF `# Citations` convention (spec §8).
//!
//! Citations are numbered markdown links listed under a heading whose text is
//! "Citations". Each entry looks like:
//!
//! ```text
//! # Citations
//!
//! [1] [BigQuery public dataset announcement](https://cloud.google.com/...)
//! [2] [Internal data quality runbook](https://wiki.acme.internal/data/quality)
//! ```
//!
//! Parsing is lenient: the leading `[N]` ordinal is optional, the section ends
//! at the next heading of the same or higher level, and citation targets may be
//! external URLs or bundle/relative paths.

use regex::Regex;
use std::sync::LazyLock;
use turbovault_core::Citation;

use crate::parse_headings;

/// Matches a single citation entry: an optional `[N]` ordinal followed by a
/// markdown link `[text](url)`.
static CITATION_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:\[(?P<idx>\d+)\]\s*)?\[(?P<text>[^\[\]]+)\]\((?P<url>[^()\s]+)\)"#).unwrap()
});

/// Parse citations from the `# Citations` section of a markdown body.
///
/// Returns an empty vector when no Citations heading is present. The section is
/// delimited by the Citations heading and the next heading at the same or a
/// higher level (or end of document).
///
/// # Example
/// ```
/// use turbovault_parser::parse_citations;
///
/// let body = "# Notes\n\ntext\n\n# Citations\n\n[1] [Source A](https://a.example)\n[2] [Source B](/refs/b.md)\n";
/// let cites = parse_citations(body);
/// assert_eq!(cites.len(), 2);
/// assert_eq!(cites[0].index, Some(1));
/// assert_eq!(cites[0].url, "https://a.example");
/// assert_eq!(cites[1].url, "/refs/b.md");
/// ```
pub fn parse_citations(content: &str) -> Vec<Citation> {
    // Fast pre-filter: no "citations" mention means nothing to do. Case-insensitive
    // byte scan avoids allocating a lowercased copy of the whole body per call
    // (this runs once per note during vault-wide scans).
    if !contains_ascii_ci(content, b"citations") {
        return Vec::new();
    }

    let headings = parse_headings(content);
    let Some(start) = headings
        .iter()
        .find(|h| h.text.trim().eq_ignore_ascii_case("citations"))
    else {
        return Vec::new();
    };

    // Section runs until the next heading at the same or higher level.
    let section_level = start.level;
    let start_line = start.position.line; // 1-indexed
    let end_line = headings
        .iter()
        .filter(|h| h.position.line > start_line && h.level <= section_level)
        .map(|h| h.position.line)
        .min();

    let lines: Vec<&str> = content.lines().collect();
    let from = start_line; // skip the heading line itself (lines are 1-indexed)
    let to = end_line.map(|l| l - 1).unwrap_or(lines.len());

    let mut citations = Vec::new();
    for line in lines.iter().take(to).skip(from) {
        for caps in CITATION_LINE.captures_iter(line) {
            // Skip images: a link preceded by '!' is not a citation.
            let m = caps.get(0).expect("regex match always has group 0");
            if m.start() > 0 && line.as_bytes().get(m.start() - 1) == Some(&b'!') {
                continue;
            }
            let index = caps
                .name("idx")
                .and_then(|m| m.as_str().parse::<u32>().ok());
            let text = caps
                .name("text")
                .map(|m| m.as_str().trim().to_string())
                .unwrap_or_default();
            let url = caps
                .name("url")
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            if !url.is_empty() {
                citations.push(Citation { index, text, url });
            }
        }
    }

    citations
}

/// Case-insensitive substring search over bytes, without allocating.
/// `needle` must be lowercase ASCII.
fn contains_ascii_ci(haystack: &str, needle: &[u8]) -> bool {
    let h = haystack.as_bytes();
    if needle.is_empty() {
        return true;
    }
    if h.len() < needle.len() {
        return false;
    }
    h.windows(needle.len())
        .any(|w| w.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_citations_section() {
        assert!(parse_citations("# Notes\n\njust text\n").is_empty());
    }

    #[test]
    fn parses_numbered_citations() {
        let body =
            "# Schema\n\nstuff\n\n# Citations\n\n[1] [A](https://a.example)\n[2] [B](/refs/b.md)\n";
        let cites = parse_citations(body);
        assert_eq!(cites.len(), 2);
        assert_eq!(cites[0].index, Some(1));
        assert_eq!(cites[0].text, "A");
        assert_eq!(cites[0].url, "https://a.example");
        assert_eq!(cites[1].index, Some(2));
        assert_eq!(cites[1].url, "/refs/b.md");
    }

    #[test]
    fn parses_unnumbered_citation() {
        let body = "# Citations\n\n[Source](https://src.example)\n";
        let cites = parse_citations(body);
        assert_eq!(cites.len(), 1);
        assert_eq!(cites[0].index, None);
        assert_eq!(cites[0].text, "Source");
    }

    #[test]
    fn stops_at_next_same_level_heading() {
        let body =
            "# Citations\n\n[1] [A](https://a.example)\n\n# Other\n\n[2] [B](https://b.example)\n";
        let cites = parse_citations(body);
        assert_eq!(cites.len(), 1);
        assert_eq!(cites[0].url, "https://a.example");
    }

    #[test]
    fn subsection_heading_does_not_end_section() {
        let body =
            "## Citations\n\n[1] [A](https://a.example)\n\n### Sub\n\n[2] [B](https://b.example)\n";
        let cites = parse_citations(body);
        assert_eq!(cites.len(), 2);
    }

    #[test]
    fn case_insensitive_heading() {
        let body = "# CITATIONS\n\n[1] [A](https://a.example)\n";
        assert_eq!(parse_citations(body).len(), 1);
    }
}
