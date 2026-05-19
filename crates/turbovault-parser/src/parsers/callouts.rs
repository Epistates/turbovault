//! Callout parser: > `[!NOTE]`, > `[!TIP]`, etc.
//!
//! Supports Obsidian callout syntax with:
//! - 13 standard types: note, tip, info, todo, important, success, question, warning, failure, danger, bug, example, quote
//! - Unknown callout identifiers parsed with the legacy `CalloutType` fallback
//! - Foldable callouts with `+` or `-` markers
//! - Multi-line content continuation
//!
//! **Deprecated**: Use `turbovault_parser::parse_callouts()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use turbovault_core::{Callout, LineIndex};

#[cfg(test)]
use turbovault_core::CalloutType;

/// Parse all callouts from content (simple, single-line parsing).
///
/// **Deprecated**: Use `turbovault_parser::parse_callouts()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_callouts() instead"
)]
pub fn parse_callouts(content: &str) -> Vec<Callout> {
    crate::parse_callouts(content)
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
    crate::parse_callouts(content)
}

/// Parse callouts with full multi-line content extraction.
///
/// **Deprecated**: Use `turbovault_parser::parse_callouts_full()` instead.
#[deprecated(
    since = "1.2.0",
    note = "Use turbovault_parser::parse_callouts_full() instead"
)]
pub fn parse_callouts_full(content: &str) -> Vec<Callout> {
    crate::parse_callouts_full(content)
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
    fn test_callout_simple_crlf_position() {
        let content = "Line 1\r\n> [!TIP] Tip here";
        let callouts = parse_callouts(content);
        assert_eq!(callouts.len(), 1);
        assert_eq!(callouts[0].position.line, 2);
        assert_eq!(callouts[0].position.offset, 8); // "Line 1\r\n" = 8 bytes
    }

    #[test]
    fn test_callout_in_fenced_code_block_not_matched() {
        let content =
            "> [!NOTE] Real\n\n```markdown\n> [!WARNING] Not real\n```\n\n> [!TIP] Also real";
        let callouts = parse_callouts(content);
        assert_eq!(callouts.len(), 2);
        assert_eq!(callouts[0].title, Some("Real".to_string()));
        assert_eq!(callouts[1].title, Some("Also real".to_string()));
    }

    #[test]
    fn test_callout_full_in_fenced_code_block_not_matched() {
        let content =
            "> [!NOTE] Real\n\n```markdown\n> [!WARNING] Not real\n```\n\n> [!TIP] Also real";
        let callouts = parse_callouts_full(content);
        assert_eq!(callouts.len(), 2);
        assert_eq!(callouts[0].title, Some("Real".to_string()));
        assert_eq!(callouts[1].title, Some("Also real".to_string()));
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
