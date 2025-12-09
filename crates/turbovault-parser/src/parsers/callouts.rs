//! Callout parser: > `[!NOTE]`, > `[!TIP]`, etc.
//!
//! Supports Obsidian callout syntax with:
//! - 13 standard types: note, tip, info, todo, important, success, question, warning, failure, danger, bug, example, quote
//! - Custom callout types (any unrecognized type is preserved as Custom)
//! - Foldable callouts with `+` or `-` markers
//! - Multi-line content continuation
//!
//! **Deprecated**: Use `turbovault_parser::parse_callouts()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use regex::Regex;
use std::sync::LazyLock;
use turbovault_core::{Callout, CalloutType, LineIndex, SourcePosition};

/// Matches > [!TYPE] callout start
static CALLOUT_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*>\s*\[!(\w+)\]([+-]?)\s*(.*?)$").unwrap());

/// Matches continuation lines (start with >)
static CONTINUATION_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*>\s*(.*)$").unwrap());

/// Fast pre-filter: skip regex if no callout pattern exists.
#[inline]
fn has_callout(content: &str) -> bool {
    content.contains("[!")
}

/// Parse callout type string into CalloutType enum.
fn parse_callout_type(type_str: &str) -> CalloutType {
    match type_str.to_lowercase().as_str() {
        "note" => CalloutType::Note,
        "tip" => CalloutType::Tip,
        "info" => CalloutType::Info,
        "todo" => CalloutType::Todo,
        "important" => CalloutType::Important,
        "success" => CalloutType::Success,
        "question" => CalloutType::Question,
        "warning" => CalloutType::Warning,
        "failure" | "fail" | "missing" => CalloutType::Failure,
        "danger" | "error" => CalloutType::Danger,
        "bug" => CalloutType::Bug,
        "example" => CalloutType::Example,
        "quote" | "cite" => CalloutType::Quote,
        _ => CalloutType::Note, // Default to Note for unknown types
    }
}

/// Parse all callouts from content (simple, single-line parsing).
///
/// **Deprecated**: Use `turbovault_parser::parse_callouts()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_callouts() instead"
)]
pub fn parse_callouts(content: &str) -> Vec<Callout> {
    if !has_callout(content) {
        return Vec::new();
    }

    let mut offset = 0;
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line_start = offset;
            offset += line.len() + 1; // +1 for newline

            CALLOUT_PATTERN.captures(line).map(|caps| {
                let type_str = caps.get(1).unwrap().as_str();
                let type_ = parse_callout_type(type_str);

                let fold_marker = caps.get(2).unwrap().as_str();
                let is_foldable = !fold_marker.is_empty();

                let title = caps.get(3).unwrap().as_str();
                let title = if title.is_empty() {
                    None
                } else {
                    Some(title.to_string())
                };

                Callout {
                    type_,
                    title,
                    content: String::new(),
                    position: SourcePosition::new(idx + 1, 1, line_start, line.len()),
                    is_foldable,
                }
            })
        })
        .collect()
}

/// Parse callouts with pre-computed line index (for consistency with other parsers).
///
/// **Deprecated**: Use `turbovault_parser::parse_callouts()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_callouts() instead"
)]
#[allow(deprecated)]
pub fn parse_callouts_indexed(content: &str, _index: &LineIndex) -> Vec<Callout> {
    parse_callouts(content)
}

/// Parse callouts with full multi-line content extraction.
///
/// **Deprecated**: Use `turbovault_parser::parse_callouts_full()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_callouts_full() instead"
)]
pub fn parse_callouts_full(content: &str) -> Vec<Callout> {
    if !has_callout(content) {
        return Vec::new();
    }

    let lines: Vec<&str> = content.lines().collect();
    let mut callouts = Vec::new();
    let mut i = 0;

    // Calculate byte offsets for each line
    let mut line_offsets: Vec<usize> = vec![0];
    for (idx, ch) in content.char_indices() {
        if ch == '\n' {
            line_offsets.push(idx + 1);
        }
    }

    while i < lines.len() {
        if let Some(caps) = CALLOUT_PATTERN.captures(lines[i]) {
            let start_line = i;
            let type_str = caps.get(1).unwrap().as_str();
            let type_ = parse_callout_type(type_str);

            let fold_marker = caps.get(2).unwrap().as_str();
            let is_foldable = !fold_marker.is_empty();

            let title_text = caps.get(3).unwrap().as_str();
            let title = if title_text.is_empty() {
                None
            } else {
                Some(title_text.to_string())
            };

            // Collect continuation lines
            let mut callout_content = String::new();
            i += 1;

            while i < lines.len() {
                if let Some(cont_caps) = CONTINUATION_PATTERN.captures(lines[i]) {
                    // Check if this line starts a new callout
                    if CALLOUT_PATTERN.is_match(lines[i]) {
                        break;
                    }

                    let line_content = cont_caps.get(1).unwrap().as_str();
                    if !callout_content.is_empty() {
                        callout_content.push('\n');
                    }
                    callout_content.push_str(line_content);
                    i += 1;
                } else {
                    // Line doesn't continue the callout
                    break;
                }
            }

            let offset = line_offsets.get(start_line).copied().unwrap_or(0);
            callouts.push(Callout {
                type_,
                title,
                content: callout_content,
                position: SourcePosition::new(start_line + 1, 1, offset, lines[start_line].len()),
                is_foldable,
            });
        } else {
            i += 1;
        }
    }

    callouts
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn test_note_callout() {
        let content = "> [!NOTE]";
        let callouts = parse_callouts(content);
        assert_eq!(callouts.len(), 1);
        assert_eq!(callouts[0].type_, CalloutType::Note);
    }

    #[test]
    fn test_callout_with_title() {
        let content = "> [!TIP] Pro tip";
        let callouts = parse_callouts(content);
        assert_eq!(callouts.len(), 1);
        assert_eq!(callouts[0].title, Some("Pro tip".to_string()));
    }

    #[test]
    fn test_foldable_callout() {
        let content = "> [!WARNING]- Click to expand";
        let callouts = parse_callouts(content);
        assert_eq!(callouts.len(), 1);
        assert!(callouts[0].is_foldable);
    }

    #[test]
    fn test_multiple_callout_types() {
        let content = "> [!NOTE]\n> [!DANGER] Error\n> [!SUCCESS]";
        let callouts = parse_callouts(content);
        assert_eq!(callouts.len(), 3);
    }

    #[test]
    fn test_all_callout_types() {
        let types = [
            ("NOTE", CalloutType::Note),
            ("TIP", CalloutType::Tip),
            ("INFO", CalloutType::Info),
            ("TODO", CalloutType::Todo),
            ("IMPORTANT", CalloutType::Important),
            ("SUCCESS", CalloutType::Success),
            ("QUESTION", CalloutType::Question),
            ("WARNING", CalloutType::Warning),
            ("FAILURE", CalloutType::Failure),
            ("DANGER", CalloutType::Danger),
            ("BUG", CalloutType::Bug),
            ("EXAMPLE", CalloutType::Example),
            ("QUOTE", CalloutType::Quote),
        ];

        for (type_str, expected) in types {
            let content = format!("> [!{}]", type_str);
            let callouts = parse_callouts(&content);
            assert_eq!(callouts.len(), 1, "Failed for type: {}", type_str);
            assert_eq!(callouts[0].type_, expected, "Wrong type for: {}", type_str);
        }
    }

    #[test]
    fn test_callout_aliases() {
        // Test aliases
        let content = "> [!FAIL]";
        let callouts = parse_callouts(content);
        assert_eq!(callouts[0].type_, CalloutType::Failure);

        let content = "> [!ERROR]";
        let callouts = parse_callouts(content);
        assert_eq!(callouts[0].type_, CalloutType::Danger);

        let content = "> [!CITE]";
        let callouts = parse_callouts(content);
        assert_eq!(callouts[0].type_, CalloutType::Quote);
    }

    #[test]
    fn test_callout_full_multiline() {
        let content = r#"> [!NOTE] Title here
> First line of content
> Second line of content
> Third line"#;

        let callouts = parse_callouts_full(content);
        assert_eq!(callouts.len(), 1);
        assert_eq!(callouts[0].title, Some("Title here".to_string()));
        assert_eq!(
            callouts[0].content,
            "First line of content\nSecond line of content\nThird line"
        );
    }

    #[test]
    fn test_callout_full_multiple() {
        let content = r#"> [!NOTE] First
> Content 1

> [!WARNING] Second
> Content 2"#;

        let callouts = parse_callouts_full(content);
        assert_eq!(callouts.len(), 2);
        assert_eq!(callouts[0].title, Some("First".to_string()));
        assert_eq!(callouts[0].content, "Content 1");
        assert_eq!(callouts[1].title, Some("Second".to_string()));
        assert_eq!(callouts[1].content, "Content 2");
    }

    #[test]
    fn test_callout_full_empty_content() {
        let content = "> [!TIP] Just a title";
        let callouts = parse_callouts_full(content);
        assert_eq!(callouts.len(), 1);
        assert_eq!(callouts[0].title, Some("Just a title".to_string()));
        assert_eq!(callouts[0].content, "");
    }

    #[test]
    fn test_callout_position() {
        let content = "Some text\n> [!NOTE] Title\n> Content";
        let callouts = parse_callouts_full(content);
        assert_eq!(callouts.len(), 1);
        assert_eq!(callouts[0].position.line, 2);
        assert_eq!(callouts[0].position.offset, 10); // "Some text\n" = 10 chars
    }

    #[test]
    fn test_callout_simple_position() {
        let content = "Line 1\n> [!TIP] Tip here";
        let callouts = parse_callouts(content);
        assert_eq!(callouts.len(), 1);
        assert_eq!(callouts[0].position.line, 2);
        assert_eq!(callouts[0].position.offset, 7); // "Line 1\n" = 7 chars
    }

    #[test]
    fn test_fast_path_no_callouts() {
        let content = "No callouts here, just plain text without the pattern.";
        let callouts = parse_callouts(content);
        assert_eq!(callouts.len(), 0);

        let callouts_full = parse_callouts_full(content);
        assert_eq!(callouts_full.len(), 0);
    }

    #[test]
    fn test_indexed_matches_regular() {
        let content = "Text\n> [!NOTE] Note\n> [!TIP] Tip";
        let index = LineIndex::new(content);

        let regular = parse_callouts(content);
        let indexed = parse_callouts_indexed(content, &index);

        assert_eq!(regular.len(), indexed.len());
        for (r, i) in regular.iter().zip(indexed.iter()) {
            assert_eq!(r.type_, i.type_);
            assert_eq!(r.position.line, i.position.line);
            assert_eq!(r.position.offset, i.position.offset);
        }
    }
}
