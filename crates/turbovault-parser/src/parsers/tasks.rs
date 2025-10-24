//! Task parser: - [ ] Task, - [x] Completed

use lazy_static::lazy_static;
use turbovault_core::{SourcePosition, TaskItem};
use regex::Regex;

lazy_static! {
    /// Matches - [ ] or - [x] followed by task text
    static ref TASK_PATTERN: Regex = Regex::new(r"^(\s*)- \[([ x])\]\s+(.+)$").unwrap();
}

/// Parse all tasks from content
pub fn parse_tasks(content: &str) -> Vec<TaskItem> {
    content
        .lines()
        .enumerate()
        .filter_map(|(idx, line)| {
            TASK_PATTERN.captures(line).map(|caps| {
                let is_completed = caps.get(2).unwrap().as_str() == "x";
                let content = caps.get(3).unwrap().as_str();
                let full_match = caps.get(0).unwrap();

                TaskItem {
                    content: content.to_string(),
                    is_completed,
                    position: SourcePosition::new(idx, 0, 0, full_match.len()),
                    due_date: None,
                }
            })
        })
        .collect()
}

#[cfg(test)]
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
    }
}
