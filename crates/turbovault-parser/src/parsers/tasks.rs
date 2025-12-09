//! Task parser: `- [ ] Task`, `- [x] Completed`
//!
//! **Deprecated**: Use `turbovault_parser::parse_tasks()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use regex::Regex;
use std::sync::LazyLock;
use turbovault_core::{LineIndex, SourcePosition, TaskItem};

/// Matches - [ ] or - [x] followed by task text
static TASK_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\s*)- \[([ xX])\]\s+(.+)$").unwrap());

/// Fast pre-filter: skip regex if no task pattern exists.
#[inline]
fn has_task(content: &str) -> bool {
    content.contains("- [")
}

/// Parse all tasks from content.
///
/// **Deprecated**: Use `turbovault_parser::parse_tasks()` instead.
#[deprecated(since = "1.2.0", note = "Use turbovault_parser::parse_tasks() instead")]
pub fn parse_tasks(content: &str) -> Vec<TaskItem> {
    if !has_task(content) {
        return Vec::new();
    }

    let mut offset = 0;
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            let line_start = offset;
            offset += line.len() + 1; // +1 for newline

            TASK_PATTERN.captures(line).map(|caps| {
                let indent = caps.get(1).unwrap().as_str();
                let is_completed = matches!(caps.get(2).unwrap().as_str(), "x" | "X");
                let task_content = caps.get(3).unwrap().as_str();
                let full_match = caps.get(0).unwrap();

                TaskItem {
                    content: task_content.to_string(),
                    is_completed,
                    position: SourcePosition::new(
                        idx + 1,
                        indent.len() + 1, // column accounts for indentation
                        line_start + indent.len(),
                        full_match.len() - indent.len(),
                    ),
                    due_date: None,
                }
            })
        })
        .collect()
}

/// Parse tasks with pre-computed line index (for consistency with other parsers).
///
/// **Deprecated**: Use `turbovault_parser::parse_tasks()` instead.
#[deprecated(since = "1.2.0", note = "Use turbovault_parser::parse_tasks() instead")]
#[allow(deprecated)]
pub fn parse_tasks_indexed(content: &str, _index: &LineIndex) -> Vec<TaskItem> {
    // Line-based parsing doesn't benefit from LineIndex, but we accept it for API consistency
    parse_tasks(content)
}

#[cfg(test)]
#[allow(deprecated)]
mod tests {
    use super::*;

    #[test]
    fn test_uncompleted_task() {
        let content = "- [ ] Write parser";
        let tasks = parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].content, "Write parser");
        assert!(!tasks[0].is_completed);
    }

    #[test]
    fn test_completed_task() {
        let content = "- [x] Complete setup";
        let tasks = parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].content, "Complete setup");
        assert!(tasks[0].is_completed);
    }

    #[test]
    fn test_completed_task_uppercase() {
        let content = "- [X] Complete setup";
        let tasks = parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert!(tasks[0].is_completed);
    }

    #[test]
    fn test_multiple_tasks() {
        let content = "- [ ] Task 1\n- [x] Task 2\n- [ ] Task 3";
        let tasks = parse_tasks(content);
        assert_eq!(tasks.len(), 3);
        assert!(!tasks[0].is_completed);
        assert!(tasks[1].is_completed);
        assert!(!tasks[2].is_completed);
    }

    #[test]
    fn test_indented_task() {
        let content = "  - [ ] Indented task";
        let tasks = parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].position.column, 3); // Indentation of 2 + 1
    }

    #[test]
    fn test_task_position_tracking() {
        let content = "Some text\n- [ ] Task on line 2\nMore text";
        let tasks = parse_tasks(content);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].position.line, 2);
        assert_eq!(tasks[0].position.column, 1);
        assert_eq!(tasks[0].position.offset, 10); // "Some text\n" = 10 chars
    }

    #[test]
    fn test_task_position_first_line() {
        let content = "- [x] First task";
        let tasks = parse_tasks(content);
        assert_eq!(tasks[0].position.line, 1);
        assert_eq!(tasks[0].position.column, 1);
        assert_eq!(tasks[0].position.offset, 0);
    }

    #[test]
    fn test_fast_path_no_tasks() {
        let content = "No tasks here, just plain text without the checkbox pattern.";
        let tasks = parse_tasks(content);
        assert_eq!(tasks.len(), 0);
    }

    #[test]
    fn test_indexed_matches_regular() {
        let content = "Text\n- [ ] Task 1\n- [x] Task 2";
        let index = LineIndex::new(content);

        let regular = parse_tasks(content);
        let indexed = parse_tasks_indexed(content, &index);

        assert_eq!(regular.len(), indexed.len());
        for (r, i) in regular.iter().zip(indexed.iter()) {
            assert_eq!(r.content, i.content);
            assert_eq!(r.position.line, i.position.line);
            assert_eq!(r.position.offset, i.position.offset);
        }
    }
}
