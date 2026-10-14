//! Note diff tools for comparing vault notes
//!
//! Provides line-level and word-level diff capabilities using the `similar` crate.
//! Supports comparing two notes by path or comparing raw content strings
//! (reusable by audit trail for version comparison).

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use std::path::PathBuf;
use std::sync::Arc;
use turbovault_core::prelude::*;
use turbovault_vault::VaultManager;

/// Diff tools for comparing notes
#[derive(Clone)]
pub struct DiffTools {
    /// The vault the notes are read from.
    pub manager: Arc<VaultManager>,
}

/// Result of comparing two notes
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DiffResult {
    /// The note diffed from.
    pub left_path: String,
    /// The note diffed to.
    pub right_path: String,
    /// The line diff in unified format.
    pub unified_diff: String,
    /// Line counts and similarity.
    pub summary: DiffSummary,
    /// Lines that changed, with word-level detail.
    pub inline_changes: Vec<InlineChange>,
}

/// Summary statistics for a diff
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct DiffSummary {
    /// Lines only on the right, not counting changed lines.
    pub lines_added: usize,
    /// Lines only on the left, not counting changed lines.
    pub lines_removed: usize,
    /// Lines replaced by another line.
    pub lines_changed: usize,
    /// Lines the same on both sides.
    pub lines_unchanged: usize,
    /// Line-level similarity, 0.0 (nothing shared) to 1.0 (identical).
    pub similarity_ratio: f64,
}

/// A changed line with word-level detail
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct InlineChange {
    /// Line number in the left note.
    pub line_number: usize,
    /// The line as it was.
    pub old_text: String,
    /// The line as it is.
    pub new_text: String,
    /// The line split into equal, inserted and deleted words.
    pub changed_words: Vec<WordChange>,
}

/// A single word-level change within a line
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
pub struct WordChange {
    /// `insert`, `delete` or `equal`.
    pub operation: String,
    /// The words it applies to.
    pub text: String,
}

impl DiffTools {
    /// Diff tools over `manager`'s vault.
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    /// Compare two notes by path
    pub async fn diff_notes(&self, left_path: &str, right_path: &str) -> Result<DiffResult> {
        let left_content = self.manager.read_file(&PathBuf::from(left_path)).await?;
        let right_content = self.manager.read_file(&PathBuf::from(right_path)).await?;

        Ok(Self::diff_content(
            &left_content,
            &right_content,
            left_path,
            right_path,
        ))
    }

    /// Compare two content strings directly (reusable by audit trail)
    pub fn diff_content(
        left: &str,
        right: &str,
        left_label: &str,
        right_label: &str,
    ) -> DiffResult {
        let line_diff = TextDiff::from_lines(left, right);

        // Build unified diff output
        let unified_diff = line_diff
            .unified_diff()
            .header(left_label, right_label)
            .context_radius(3)
            .to_string();

        // Compute summary statistics
        let mut lines_added = 0usize;
        let mut lines_removed = 0usize;
        let mut lines_unchanged = 0usize;

        for change in line_diff.iter_all_changes() {
            match change.tag() {
                ChangeTag::Insert => lines_added += 1,
                ChangeTag::Delete => lines_removed += 1,
                ChangeTag::Equal => lines_unchanged += 1,
            }
        }

        let similarity_ratio = f64::from(line_diff.ratio());

        // Find changed line pairs and compute word-level diffs
        let mut inline_changes = compute_inline_changes(&line_diff);
        let lines_changed = inline_changes.len();
        // Cap inline changes to avoid huge output on very different files
        inline_changes.truncate(50);

        DiffResult {
            left_path: left_label.to_string(),
            right_path: right_label.to_string(),
            unified_diff,
            summary: DiffSummary {
                lines_added: lines_added.saturating_sub(lines_changed),
                lines_removed: lines_removed.saturating_sub(lines_changed),
                lines_changed,
                lines_unchanged,
                similarity_ratio,
            },
            inline_changes,
        }
    }
}

/// Extract paired delete/insert changes and compute word-level diffs
fn compute_inline_changes<'a>(line_diff: &TextDiff<'a, 'a, str>) -> Vec<InlineChange> {
    let mut inline_changes = Vec::new();
    let changes: Vec<_> = line_diff.iter_all_changes().collect();

    let mut i = 0;
    let mut line_number = 0usize;

    while i < changes.len() {
        let change = &changes[i];

        match change.tag() {
            ChangeTag::Equal => {
                line_number += 1;
                i += 1;
            }
            ChangeTag::Delete => {
                line_number += 1;
                // Look ahead for a matching Insert (changed line pair)
                if i + 1 < changes.len() && changes[i + 1].tag() == ChangeTag::Insert {
                    let old_text = change.to_string_lossy();
                    let new_text = changes[i + 1].to_string_lossy();

                    let word_diff = TextDiff::from_words(old_text.trim_end(), new_text.trim_end());
                    let changed_words: Vec<WordChange> = word_diff
                        .iter_all_changes()
                        .map(|wc| WordChange {
                            operation: match wc.tag() {
                                ChangeTag::Insert => "insert".to_string(),
                                ChangeTag::Delete => "delete".to_string(),
                                ChangeTag::Equal => "equal".to_string(),
                            },
                            text: wc.to_string_lossy().to_string(),
                        })
                        .collect();

                    inline_changes.push(InlineChange {
                        line_number,
                        old_text: old_text.trim_end().to_string(),
                        new_text: new_text.trim_end().to_string(),
                        changed_words,
                    });

                    i += 2; // skip the Insert
                } else {
                    i += 1; // pure deletion, no pair
                }
            }
            ChangeTag::Insert => {
                i += 1; // pure insertion (no preceding delete)
            }
        }
    }

    inline_changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_diff_identical_content() {
        let content = "# Hello\n\nThis is a test note.\n";
        let result = DiffTools::diff_content(content, content, "a.md", "b.md");

        assert_eq!(result.summary.lines_added, 0);
        assert_eq!(result.summary.lines_removed, 0);
        assert_eq!(result.summary.lines_changed, 0);
        assert!((result.summary.similarity_ratio - 1.0).abs() < f64::EPSILON);
        assert!(result.inline_changes.is_empty());
    }

    #[test]
    fn test_diff_completely_different() {
        let left = "Hello world\n";
        let right = "Goodbye universe\n";
        let result = DiffTools::diff_content(left, right, "a.md", "b.md");

        assert!(result.summary.similarity_ratio < 1.0);
        assert!(!result.unified_diff.is_empty());
    }

    #[test]
    fn test_diff_with_changes() {
        let left = "# Title\n\nLine one\nLine two\nLine three\n";
        let right = "# Title\n\nLine one\nLine modified\nLine three\n";
        let result = DiffTools::diff_content(left, right, "a.md", "b.md");

        assert_eq!(result.summary.lines_changed, 1);
        assert_eq!(result.summary.lines_unchanged, 4); // title, blank, line one, line three
        assert_eq!(result.inline_changes.len(), 1);
        assert_eq!(result.inline_changes[0].old_text, "Line two");
        assert_eq!(result.inline_changes[0].new_text, "Line modified");
    }

    #[test]
    fn test_diff_additions_only() {
        let left = "Line one\n";
        let right = "Line one\nLine two\nLine three\n";
        let result = DiffTools::diff_content(left, right, "a.md", "b.md");

        assert_eq!(result.summary.lines_added, 2);
        assert_eq!(result.summary.lines_removed, 0);
        assert_eq!(result.summary.lines_unchanged, 1);
    }

    #[test]
    fn test_diff_word_level_changes() {
        let left = "The quick brown fox\n";
        let right = "The slow brown dog\n";
        let result = DiffTools::diff_content(left, right, "a.md", "b.md");

        assert_eq!(result.inline_changes.len(), 1);
        let change = &result.inline_changes[0];
        // Should have word-level detail showing "quick" → "slow" and "fox" → "dog"
        assert!(
            change
                .changed_words
                .iter()
                .any(|w| w.operation == "delete" && w.text.contains("quick"))
        );
        assert!(
            change
                .changed_words
                .iter()
                .any(|w| w.operation == "insert" && w.text.contains("slow"))
        );
    }

    #[test]
    fn test_diff_empty_content() {
        let result = DiffTools::diff_content("", "", "a.md", "b.md");
        assert!((result.summary.similarity_ratio - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_diff_labels_in_output() {
        let result = DiffTools::diff_content("a\n", "b\n", "notes/a.md", "notes/b.md");
        assert!(result.left_path == "notes/a.md");
        assert!(result.right_path == "notes/b.md");
    }
}
