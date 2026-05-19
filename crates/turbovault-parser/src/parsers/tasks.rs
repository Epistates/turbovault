//! Task parser: `- [ ] Task`, `- [x] Completed`, `- [/] In progress`, `- [-] Cancelled`
//!
//! **Deprecated**: Use `turbovault_parser::parse_tasks()` or `ParsedContent::parse()` instead.
//! These functions are kept for backwards compatibility but will be removed in a future version.

use turbovault_core::{LineIndex, TaskItem};

/// Parse all tasks from content.
///
/// **Deprecated**: Use `turbovault_parser::parse_tasks()` instead.
#[deprecated(since = "1.2.0", note = "Use turbovault_parser::parse_tasks() instead")]
pub fn parse_tasks(content: &str) -> Vec<TaskItem> {
    crate::parse_tasks(content)
}

/// Parse tasks with pre-computed line index (for consistency with other parsers).
///
/// **Deprecated**: Use `turbovault_parser::parse_tasks()` instead.
#[deprecated(since = "1.2.0", note = "Use turbovault_parser::parse_tasks() instead")]
#[allow(deprecated)]
pub fn parse_tasks_indexed(content: &str, _index: &LineIndex) -> Vec<TaskItem> {
    crate::parse_tasks(content)
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

    #[test]
    fn test_task_metadata() {
        let content = "- [ ] Review PR [due:: 2026-05-01], [project:: [[Team Work]]], [onCompletion:: delete] 🔁 every weekday #review";
        let tasks = parse_tasks(content);

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].content, "Review PR");
        assert_eq!(
            tasks[0].due_date.map(|date| date.to_string()).as_deref(),
            Some("2026-05-01")
        );
        assert_eq!(tasks[0].recurrence.as_deref(), Some("every weekday"));
        assert_eq!(tasks[0].on_completion.as_deref(), Some("delete"));
        assert_eq!(tasks[0].tags, vec!["review".to_string()]);
        assert_eq!(
            tasks[0].metadata.get("project").map(String::as_str),
            Some("[[Team Work]]")
        );
    }

    #[test]
    fn test_extended_task_states() {
        let content = "- [/] In progress\n- [-] Cancelled";
        let tasks = parse_tasks(content);

        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].content, "In progress");
        assert!(!tasks[0].is_completed);
        assert_eq!(tasks[1].content, "Cancelled");
        assert!(!tasks[1].is_completed);
    }

    #[test]
    fn test_task_in_code_block_not_parsed() {
        let content = "```md\n- [/] Not a task\n```\n\n- [-] Real task";
        let tasks = parse_tasks(content);

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].content, "Real task");
        assert_eq!(tasks[0].position.line, 5);
    }

    #[test]
    fn test_task_position_with_crlf() {
        let content = "Intro\r\n- [ ] Task after CRLF";
        let tasks = parse_tasks(content);

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].position.line, 2);
        assert_eq!(tasks[0].position.column, 1);
        assert_eq!(tasks[0].position.offset, 7);
    }
}
