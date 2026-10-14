//! Tasks across the vault, filtered the way the Tasks plugin's queries are.
//!
//! A task is any Markdown checkbox item. Its status is the character in the
//! checkbox (` ` to do, `x` done, `/` in progress, `-` cancelled); its dates
//! and priority come from the Tasks plugin's emoji (📅 due, ⏳ scheduled,
//! 🛫 start, ✅ done, ❌ cancelled, ➕ created; 🔺 ⏫ 🔼 🔽 ⏬ priority) or
//! from Dataview inline fields (`[due:: 2026-10-20]`), as the parser reads
//! them.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use turbovault_core::prelude::*;
use turbovault_core::{TaskItem, TaskPriority};
use turbovault_vault::VaultManager;

/// A task's status, from its checkbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// `[ ]`
    Todo,
    /// `[x]` or `[X]`
    Done,
    /// `[/]`
    InProgress,
    /// `[-]`
    Cancelled,
    /// Any other checkbox character, a custom status.
    Other,
}

impl TaskState {
    /// The state `task`'s checkbox character stands for.
    pub fn of(task: &TaskItem) -> Self {
        match task.status {
            ' ' => Self::Todo,
            'x' | 'X' => Self::Done,
            '/' => Self::InProgress,
            '-' => Self::Cancelled,
            _ => Self::Other,
        }
    }
}

/// A Tasks plugin priority, lowest to highest: `Lowest` (⏬), `Low` (🔽),
/// `Normal` (no emoji), `Medium` (🔼), `High` (⏫), `Highest` (🔺).
// The variants are described here: a doc comment on each would turn the
// enum's JSON Schema from a plain `enum` into a longer `oneOf`.
#[allow(missing_docs)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Lowest,
    Low,
    Normal,
    Medium,
    High,
    Highest,
}

impl From<TaskPriority> for Priority {
    fn from(priority: TaskPriority) -> Self {
        match priority {
            TaskPriority::Lowest => Self::Lowest,
            TaskPriority::Low => Self::Low,
            TaskPriority::Normal => Self::Normal,
            TaskPriority::Medium => Self::Medium,
            TaskPriority::High => Self::High,
            TaskPriority::Highest => Self::Highest,
        }
    }
}

/// Which tasks to return. Every condition given must hold; an empty list or
/// `None` places no condition.
#[derive(Debug, Clone, Default)]
pub struct TaskQuery {
    /// Tasks in any of these states.
    pub status: Vec<TaskState>,
    /// Tasks due on or before this date.
    pub due_before: Option<NaiveDate>,
    /// Tasks due on or after this date.
    pub due_after: Option<NaiveDate>,
    /// Tasks scheduled on or before this date.
    pub scheduled_before: Option<NaiveDate>,
    /// Tasks scheduled on or after this date.
    pub scheduled_after: Option<NaiveDate>,
    /// A tag the task carries (`#work`, or `work`, which also matches
    /// `#work/client`).
    pub tag: Option<String>,
    /// A note's path, or a folder whose notes (at any depth) to search.
    pub path: Option<String>,
    /// Tasks with any of these priorities.
    pub priority: Vec<Priority>,
    /// Text the task's description contains, ignoring case.
    pub text: Option<String>,
}

/// One task that matched.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct TaskMatch {
    /// The note it's in, vault-relative.
    pub path: String,
    /// Its line in the note, from 1.
    pub line: usize,
    /// Its state, from the checkbox.
    pub status: TaskState,
    /// The checkbox character itself.
    pub status_char: String,
    /// The description, without its Tasks metadata.
    pub text: String,
    /// Its priority.
    pub priority: Priority,
    /// Due date, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due: Option<String>,
    /// Scheduled date, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled: Option<String>,
    /// Start date, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<String>,
    /// Done date, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<String>,
    /// Cancelled date, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled: Option<String>,
    /// Created date, `YYYY-MM-DD`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    /// Inline tags, without the `#`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// Recurrence rule, such as `every week`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence: Option<String>,
    /// Block reference, without the `^`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_ref: Option<String>,
}

/// Every task in the vault that matches `query`, ordered by due date
/// (undated last), then priority (highest first), then path and line.
pub async fn query_tasks(manager: &Arc<VaultManager>, query: &TaskQuery) -> Result<Vec<TaskMatch>> {
    let statuses: HashSet<TaskState> = query.status.iter().copied().collect();
    let priorities: HashSet<Priority> = query.priority.iter().copied().collect();
    let tag = query
        .tag
        .as_deref()
        .map(|tag| tag.trim_start_matches('#').to_lowercase());
    let text = query.text.as_deref().map(str::to_lowercase);
    let scope = query
        .path
        .as_deref()
        .map(|path| path.trim_matches('/').to_string())
        .filter(|path| !path.is_empty());

    let mut matches = Vec::new();
    for file in manager.notes_validated().await {
        let path = manager.relative_path(&file.path);
        if let Some(scope) = &scope
            && path != *scope
            && !path.starts_with(&format!("{scope}/"))
        {
            continue;
        }
        for task in &file.tasks {
            let state = TaskState::of(task);
            let priority = Priority::from(task.priority);
            if !statuses.is_empty() && !statuses.contains(&state) {
                continue;
            }
            if !priorities.is_empty() && !priorities.contains(&priority) {
                continue;
            }
            if !within(task.due_date, query.due_after, query.due_before)
                || !within(
                    task.scheduled_date,
                    query.scheduled_after,
                    query.scheduled_before,
                )
            {
                continue;
            }
            if let Some(tag) = &tag
                && !task.tags.iter().any(|t| {
                    let t = t.trim_start_matches('#').to_lowercase();
                    t == *tag || t.starts_with(&format!("{tag}/"))
                })
            {
                continue;
            }
            if let Some(text) = &text
                && !task.content.to_lowercase().contains(text)
            {
                continue;
            }
            matches.push(TaskMatch {
                path: path.clone(),
                line: task.position.line,
                status: state,
                status_char: task.status.to_string(),
                text: task.content.clone(),
                priority,
                due: task.due_date.map(iso),
                scheduled: task.scheduled_date.map(iso),
                start: task.start_date.map(iso),
                done: task.done_date.map(iso),
                cancelled: task.cancelled_date.map(iso),
                created: task.created_date.map(iso),
                tags: task.tags.clone(),
                recurrence: task.recurrence.clone(),
                block_ref: task.block_ref.clone(),
            });
        }
    }
    matches.sort_by(|a, b| {
        (
            a.due.is_none(),
            &a.due,
            std::cmp::Reverse(a.priority),
            &a.path,
            a.line,
        )
            .cmp(&(
                b.due.is_none(),
                &b.due,
                std::cmp::Reverse(b.priority),
                &b.path,
                b.line,
            ))
    });
    Ok(matches)
}

/// Whether `date` falls on or after `after` and on or before `before`. A
/// task with no such date fails either bound.
fn within(date: Option<NaiveDate>, after: Option<NaiveDate>, before: Option<NaiveDate>) -> bool {
    if after.is_none() && before.is_none() {
        return true;
    }
    let Some(date) = date else { return false };
    after.is_none_or(|after| date >= after) && before.is_none_or(|before| date <= before)
}

fn iso(date: NaiveDate) -> String {
    date.format("%Y-%m-%d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use turbovault_core::{ConfigProfile, VaultConfig};

    const WORK: &str = "# Work\n\n- [ ] Ship release 📅 2026-10-20 ⏫ #work\n- [x] Write notes ✅ 2026-10-01\n- [/] Review PR [due:: 2026-10-18] #work/client\n- [-] Old idea\n- [ ] Someday\n";
    const HOME: &str = "# Home\n\n- [ ] Buy milk 📅 2026-10-18 🔽\n- [ ] Plan trip ⏳ 2026-11-01\n";

    async fn manager() -> (TempDir, Arc<VaultManager>) {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("projects")).unwrap();
        std::fs::write(dir.path().join("projects/Work.md"), WORK).unwrap();
        std::fs::write(dir.path().join("Home.md"), HOME).unwrap();
        let mut config = ConfigProfile::Development.create_config();
        config.vaults = vec![VaultConfig::builder("v", dir.path()).build().unwrap()];
        let manager = Arc::new(VaultManager::new(config).unwrap());
        manager.initialize().await.unwrap();
        (dir, manager)
    }

    fn texts(matches: &[TaskMatch]) -> Vec<&str> {
        matches.iter().map(|m| m.text.as_str()).collect()
    }

    fn date(s: &str) -> Option<NaiveDate> {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
    }

    #[tokio::test]
    async fn orders_by_due_date_then_priority() {
        let (_dir, manager) = manager().await;
        let all = query_tasks(&manager, &TaskQuery::default()).await.unwrap();
        assert_eq!(
            texts(&all),
            [
                "Review PR",
                "Buy milk",
                "Ship release",
                "Plan trip",
                "Write notes",
                "Old idea",
                "Someday",
            ]
        );
        let review = &all[0];
        assert_eq!(review.status, TaskState::InProgress);
        assert_eq!(review.due.as_deref(), Some("2026-10-18"));
        assert_eq!(review.path, "projects/Work.md");
        assert_eq!(review.line, 5);
    }

    #[tokio::test]
    async fn filters_combine() {
        let (_dir, manager) = manager().await;
        let open_due_soon = query_tasks(
            &manager,
            &TaskQuery {
                status: vec![TaskState::Todo, TaskState::InProgress],
                due_before: date("2026-10-19"),
                ..TaskQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(texts(&open_due_soon), ["Review PR", "Buy milk"]);

        let tagged = query_tasks(
            &manager,
            &TaskQuery {
                tag: Some("#work".into()),
                ..TaskQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(texts(&tagged), ["Review PR", "Ship release"]);

        let in_folder = query_tasks(
            &manager,
            &TaskQuery {
                path: Some("projects/".into()),
                priority: vec![Priority::High],
                ..TaskQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(texts(&in_folder), ["Ship release"]);

        let scheduled = query_tasks(
            &manager,
            &TaskQuery {
                scheduled_after: date("2026-10-31"),
                text: Some("TRIP".into()),
                ..TaskQuery::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(texts(&scheduled), ["Plan trip"]);
    }
}
