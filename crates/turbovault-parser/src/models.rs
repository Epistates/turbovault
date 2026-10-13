//! Parser models that aren't shared through `turbovault-core`.

use serde::{Deserialize, Serialize};

/// Obsidian task checkbox states
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TaskStatus {
    /// `[ ]` — pending/incomplete
    Pending,
    /// `[x]` or `[X]` — completed/done
    Done,
    /// `[/]` — in progress
    InProgress,
    /// `[-]` — cancelled
    Cancelled,
}

impl TaskStatus {
    /// Whether the task is done (`[x]`). Cancelled tasks are not.
    pub fn is_completed(&self) -> bool {
        matches!(self, TaskStatus::Done)
    }

    /// The status a checkbox character means: `x` done, `/` in progress,
    /// `-` cancelled, anything else open.
    pub fn from_marker(c: char) -> Self {
        match c {
            'x' | 'X' => TaskStatus::Done,
            '/' => TaskStatus::InProgress,
            '-' => TaskStatus::Cancelled,
            _ => TaskStatus::Pending,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_from_marker_done() {
        assert_eq!(TaskStatus::from_marker('x'), TaskStatus::Done);
        assert_eq!(TaskStatus::from_marker('X'), TaskStatus::Done);
    }

    #[test]
    fn test_from_marker_in_progress() {
        assert_eq!(TaskStatus::from_marker('/'), TaskStatus::InProgress);
    }

    #[test]
    fn test_from_marker_cancelled() {
        assert_eq!(TaskStatus::from_marker('-'), TaskStatus::Cancelled);
    }

    #[test]
    fn test_from_marker_pending() {
        assert_eq!(TaskStatus::from_marker(' '), TaskStatus::Pending);
    }

    #[test]
    fn test_from_marker_unknown_defaults_pending() {
        assert_eq!(TaskStatus::from_marker('?'), TaskStatus::Pending);
        assert_eq!(TaskStatus::from_marker('a'), TaskStatus::Pending);
    }

    #[test]
    fn test_is_completed() {
        assert!(TaskStatus::Done.is_completed());
        assert!(!TaskStatus::Pending.is_completed());
        assert!(!TaskStatus::InProgress.is_completed());
        assert!(!TaskStatus::Cancelled.is_completed());
    }

    #[test]
    fn test_serde_roundtrip() {
        let variants = [
            TaskStatus::Pending,
            TaskStatus::Done,
            TaskStatus::InProgress,
            TaskStatus::Cancelled,
        ];

        for variant in &variants {
            let json = serde_json::to_string(variant)
                .unwrap_or_else(|e| panic!("serialize {:?} failed: {}", variant, e));
            let roundtripped: TaskStatus = serde_json::from_str(&json).unwrap_or_else(|e| {
                panic!("deserialize {:?} from {:?} failed: {}", variant, json, e)
            });
            assert_eq!(
                variant, &roundtripped,
                "roundtrip mismatch for {:?}: serialized to {:?}",
                variant, json
            );
        }
    }
}
