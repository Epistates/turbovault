//! Callout parser: > `[!NOTE]`, > `[!TIP]`, etc.

use lazy_static::lazy_static;
use regex::Regex;
use turbovault_core::{Callout, CalloutType, SourcePosition};

lazy_static! {
    /// Matches > [!TYPE] callout start
    static ref CALLOUT_PATTERN: Regex = Regex::new(r"^\s*>\s*\[!(\w+)\]([+-]?)\s*(.*?)$").unwrap();
}

/// Parse all callouts from content
pub fn parse_callouts(content: &str) -> Vec<Callout> {
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            CALLOUT_PATTERN.captures(line).map(|caps| {
                let type_str = caps.get(1).unwrap().as_str();
                let type_ = match type_str.to_lowercase().as_str() {
                    "note" => CalloutType::Note,
                    "tip" => CalloutType::Tip,
                    "info" => CalloutType::Info,
                    "todo" => CalloutType::Todo,
                    "important" => CalloutType::Important,
                    "success" => CalloutType::Success,
                    "question" => CalloutType::Question,
                    "warning" => CalloutType::Warning,
                    "failure" => CalloutType::Failure,
                    "danger" => CalloutType::Danger,
                    "bug" => CalloutType::Bug,
                    "example" => CalloutType::Example,
                    "quote" => CalloutType::Quote,
                    _ => CalloutType::Note,
                };

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
                    content: String::new(), // TODO: parse continuation lines
                    position: SourcePosition::new(idx, 0, 0, line.len()),
                    is_foldable,
                }
            })
        })
        .collect()
}

#[cfg(test)]
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
}
