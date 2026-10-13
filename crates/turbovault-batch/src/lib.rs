//! # Batch Operation Types
//!
//! Serde/schema types + intra-batch validation for a batch of vault file
//! operations. write-substrate-layering M4e (design §6.9): this crate
//! SHRANK to types + validation only — it no longer executes anything.
//! [`BatchOperation`]s are translated into one `turbovault_core::ChangePlan`
//! and applied through `VaultManager::apply_changes` (see
//! `turbovault_tools::BatchTools`, the crate that owns that translation +
//! the manager-routed execution).
//!
//! ## Core Types
//!
//! ### BatchOperation
//!
//! Individual operations describable in a batch:
//! - [`BatchOperation::CreateNote`] - Create a new note
//! - [`BatchOperation::WriteNote`] - Write or overwrite a note
//! - [`BatchOperation::DeleteNote`] - Delete a note
//! - [`BatchOperation::MoveNote`] - Move or rename a note
//! - [`BatchOperation::UpdateLinks`] - Update link references
//! - [`BatchOperation::EditNote`] / [`BatchOperation::UpdateFrontmatter`] /
//!   [`BatchOperation::ManageTags`] / [`BatchOperation::CreateFromTemplate`] -
//!   the link-aware / metadata / template ops (translated via the domain
//!   tools' `compute_*` helpers, per design §5.4)
//!
//! ### BatchResult
//!
//! [`BatchResult`] describes an execution outcome:
//! - Overall success/failure status
//! - Count of executed operations
//! - First failure point (if any)
//! - List of changes made
//! - List of errors encountered
//! - Individual operation records
//! - Unique transaction ID
//! - Execution duration
//!
//! ## Conflict Detection
//!
//! [`BatchOperation::conflicts_with`] flags operations whose affected files
//! overlap:
//! - Write + Delete on same file = conflict
//! - Move + Write on same file = conflict
//! - Multiple reads (UpdateLinks) = allowed
//!
//! Example:
//! ```
//! use turbovault_batch::BatchOperation;
//!
//! let write = BatchOperation::WriteNote {
//!     path: "file.md".to_string(),
//!     content: "content".to_string(),
//!     expected_hash: None,
//! };
//!
//! let delete = BatchOperation::DeleteNote {
//!     path: "file.md".to_string(),
//!     expected_hash: None,
//!     on_backlinks: None,
//! };
//!
//! assert!(write.conflicts_with(&delete));
//! ```

use serde::{Deserialize, Serialize};

/// Individual batch operation to execute
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type")]
pub enum BatchOperation {
    /// Create a new note with content. Defaults to strict-create: the
    /// substrate adds `expect_absent`, so the loser of a concurrent-create
    /// race aborts the entire batch with `ConcurrencyError`
    /// (turbovault-947 / 6fo §6 reconsideration domino).
    ///
    /// `force: Some(true)` disables `expect_absent`, falling back to
    /// upsert semantics (caller-acknowledged blind create/overwrite —
    /// equivalent to `WriteNote { expected_hash: None }`).
    #[serde(rename = "CreateNote", alias = "CreateFile")]
    #[schemars(
        description = "Create a note. Fails the batch if a note is already at the path, unless force is true."
    )]
    CreateNote {
        #[schemars(
            description = "Vault-relative path of the new note, such as `projects/Plan.md`."
        )]
        path: String,
        #[schemars(description = "The note's markdown.")]
        content: String,
        /// Disable the implicit `expect_absent` precondition. Default
        /// false; pass true to acknowledge a blind create/overwrite.
        #[schemars(description = "Replace a note already at the path. Default false.")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },

    /// Write/overwrite a note. `expected_hash` (git blob OID hex on git
    /// backend, SHA-256 on direct) carries an `expect_blob` precondition;
    /// the whole batch aborts if the target file no longer matches the
    /// expected pre-image (turbovault-c0e).
    #[serde(rename = "WriteNote", alias = "WriteFile")]
    #[schemars(description = "Write a note's whole content, creating it or replacing it.")]
    WriteNote {
        #[schemars(description = "Vault-relative path of the note, such as `projects/Plan.md`.")]
        path: String,
        #[schemars(description = "The note's full markdown.")]
        content: String,
        /// Optional optimistic-concurrency precondition checked before any operation in the batch is applied.
        #[schemars(
            description = "The note's `hash` from read_note; the batch fails if the note changed since. Omit to write whatever is there."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// Delete a note. `expected_hash` guards the delete against a
    /// concurrent modification of the target.
    ///
    /// turbovault-0g4.7: on the **git backend**, `on_backlinks` controls inbound
    /// wikilinks (parity with the standalone `delete_note`):
    /// - `"refuse"` (default) — abort the batch if the note has inbound
    ///   backlinks (prevents silently shipping broken links);
    /// - `"rewrite-stale-callout"` — atomically `~~[[strikethrough]]~~` every
    ///   linker in the same commit;
    /// - `"force"` — delete and leave inbound links dangling (the pre-0g4.7
    ///   behavior).
    ///
    /// The direct backend ignores this field and always does a bare delete (no
    /// atomic multi-file primitive).
    #[serde(rename = "DeleteNote", alias = "DeleteFile")]
    #[schemars(
        description = "Delete a note. By default the batch fails if other notes link to it; on_backlinks changes that."
    )]
    DeleteNote {
        #[schemars(description = "Vault-relative path of the note to delete.")]
        path: String,
        /// Optional optimistic-concurrency precondition on the deleted file.
        #[schemars(
            description = "The note's `hash` from read_note; the batch fails if the note changed since."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
        #[schemars(
            description = "`refuse` (default) fails the batch if other notes link to it, `rewrite-stale-callout` strikes those links through, `force` leaves them broken."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_backlinks: Option<String>,
    },

    /// Move/rename a note. `expected_hash` guards the SOURCE against concurrent
    /// modification; `dest_expected_hash` guards the DESTINATION. Both are
    /// sentinel-or-oid strings (`<oid> | "absent" | "exists" | "blind"`), the
    /// same shape every other batch op's `expected_hash` uses. `dest_expected_hash`
    /// omitted = `"absent"` (the default clobber guard — refuse if the dest
    /// exists), preserving the pre-qae.6.4 behavior. (turbovault-qae.6.4)
    ///
    /// turbovault-0g4.6: on the **git backend**, `update_backlinks` (default
    /// true) atomically rewrites every inbound wikilink — `[[from]]`,
    /// `[[from|alias]]`, `[[from#section]]`, `[[from#^block]]`, `![[from]]` and
    /// path-prefix forms — to the new target in the SAME commit, with per-source
    /// `expect_blob` preconditions. Set it false for a rename-only move (inbound
    /// links dangle — the pre-0g4.6 behavior). The direct backend ignores this
    /// field and is always rename-only (it has no atomic multi-file primitive).
    #[serde(rename = "MoveNote", alias = "MoveFile")]
    #[schemars(
        description = "Move or rename a note, rewriting links to it in other notes in the same batch."
    )]
    MoveNote {
        #[schemars(description = "Vault-relative path of the note to move.")]
        from: String,
        #[schemars(description = "Vault-relative path to move it to.")]
        to: String,
        /// Optional optimistic-concurrency precondition on the source file
        /// (sentinel-or-oid string).
        #[schemars(
            description = "The note's `hash` from read_note; the batch fails if it changed since."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
        /// Optional optimistic-concurrency precondition on the destination
        /// (sentinel-or-oid string). Omitted = `"absent"` (clobber guard).
        #[schemars(
            description = "What must be at the destination. Default `absent`, so an existing note is never replaced; a `hash` replaces that version, `blind` anything."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dest_expected_hash: Option<String>,
        #[schemars(
            description = "Rewrite links to the note in other notes. Default true; false leaves them pointing at the old path."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update_backlinks: Option<bool>,
    },

    /// Update links in a note (find and replace link target).
    /// `expected_hash` guards the source file from concurrent
    /// modification — important when several batch ops update siblings
    /// of the same renamed page.
    #[serde(rename = "UpdateLinks")]
    #[schemars(description = "In one note, re-point every link to one target at another.")]
    UpdateLinks {
        #[schemars(description = "Vault-relative path of the note whose links to change.")]
        file: String,
        #[schemars(
            description = "The link target to replace, as written in the links, such as `Old Note`."
        )]
        old_target: String,
        #[schemars(description = "The target to put instead, such as `New Note`.")]
        new_target: String,
        #[schemars(
            description = "The note's `hash` from read_note; the batch fails if it changed since."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// turbovault-0g4.1: apply SEARCH/REPLACE blocks to an existing note as
    /// part of the atomic batch commit (the batch equivalent of `edit_note`).
    /// `edits` uses the same block grammar as the tool; multiple blocks edit
    /// multiple locations in the one file. `expected_hash` (git blob OID hex)
    /// carries an `expect_blob` precondition — a stale pre-image aborts the
    /// whole batch. **Git backend only**; the direct executor refuses it.
    #[serde(rename = "EditNote", alias = "EditFile")]
    #[schemars(
        description = "Change parts of a note with SEARCH/REPLACE blocks, as edit_note does."
    )]
    EditNote {
        #[schemars(description = "Vault-relative path of the note to edit.")]
        path: String,
        #[schemars(
            description = "SEARCH/REPLACE blocks in edit_note's format: `<<<<<<< SEARCH`, text to find, `=======`, replacement, `>>>>>>> REPLACE`."
        )]
        edits: String,
        #[schemars(
            description = "The note's `hash` from read_note; the batch fails if it changed since."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// turbovault-0g4.2: set/merge frontmatter keys on a note as part of the
    /// atomic batch commit (the batch equivalent of `update_frontmatter`).
    /// `merge` defaults to true (deep-merge into existing frontmatter); false
    /// replaces the frontmatter wholesale. `expected_hash` (git blob OID hex)
    /// carries an `expect_blob` precondition. **Git backend only**.
    #[serde(rename = "UpdateFrontmatter")]
    #[schemars(description = "Change a note's frontmatter, as update_frontmatter does.")]
    UpdateFrontmatter {
        #[schemars(description = "Vault-relative path of the note.")]
        path: String,
        #[schemars(
            description = "The keys and values to write, such as `{\"status\": \"done\"}`."
        )]
        frontmatter: std::collections::HashMap<String, serde_json::Value>,
        #[schemars(
            description = "True (default) merges these keys into the existing frontmatter; false replaces it."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        merge: Option<bool>,
        #[schemars(
            description = "The note's `hash` from read_note; the batch fails if it changed since."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// turbovault-0g4.3: add or remove frontmatter tags on a note as part of
    /// the atomic batch commit (the batch equivalent of `manage_tags`
    /// add/remove). `operation` is `"add"` or `"remove"`; `"list"` is
    /// read-only and not a batch op (rejected). `expected_hash` carries an
    /// `expect_blob` precondition. **Git backend only**.
    #[serde(rename = "ManageTags")]
    #[schemars(description = "Add or remove tags in a note's frontmatter `tags` list.")]
    ManageTags {
        #[schemars(description = "Vault-relative path of the note.")]
        path: String,
        #[schemars(description = "`add` or `remove`.")]
        operation: String,
        #[schemars(description = "Tags without `#`, such as `[\"work\", \"project/alpha\"]`.")]
        tags: Vec<String>,
        #[schemars(
            description = "The note's `hash` from read_note; the batch fails if it changed since."
        )]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expected_hash: Option<String>,
    },

    /// turbovault-0g4.4: render a template and create a note as part of the
    /// atomic batch commit (the batch equivalent of `create_from_template`).
    /// `force` (default false) → strict create (`expect_absent`, aborts if the
    /// path exists); true → blind upsert. **Git backend only**.
    #[serde(rename = "CreateFromTemplate")]
    #[schemars(description = "Create a note from a template, as create_from_template does.")]
    CreateFromTemplate {
        #[schemars(description = "The template's id, as list_templates shows it.")]
        template_id: String,
        #[schemars(description = "Vault-relative path for the new note.")]
        path: String,
        #[schemars(
            description = "The template's field values, such as `{\"title\": \"Weekly sync\"}`."
        )]
        fields: std::collections::HashMap<String, String>,
        #[schemars(description = "Replace a note already at the path. Default false.")]
        #[serde(default, skip_serializing_if = "Option::is_none")]
        force: Option<bool>,
    },
}

impl BatchOperation {
    /// Get list of files affected by this operation
    pub fn affected_files(&self) -> Vec<String> {
        match self {
            Self::CreateNote { path, .. } => vec![path.clone()],
            Self::WriteNote { path, .. } => vec![path.clone()],
            Self::DeleteNote { path, .. } => vec![path.clone()],
            Self::MoveNote { from, to, .. } => vec![from.clone(), to.clone()],
            Self::UpdateLinks {
                file,
                old_target,
                new_target,
                ..
            } => {
                vec![file.clone(), old_target.clone(), new_target.clone()]
            }
            Self::EditNote { path, .. } => vec![path.clone()],
            Self::UpdateFrontmatter { path, .. } => vec![path.clone()],
            Self::ManageTags { path, .. } => vec![path.clone()],
            Self::CreateFromTemplate { path, .. } => vec![path.clone()],
        }
    }

    /// Return the path and expected content hash guarded by this operation.
    pub fn precondition(&self) -> Option<(&str, &str)> {
        match self {
            Self::WriteNote {
                path,
                expected_hash: Some(hash),
                ..
            }
            | Self::DeleteNote {
                path,
                expected_hash: Some(hash),
                ..
            } => Some((path, hash)),
            Self::MoveNote {
                from,
                expected_hash: Some(hash),
                ..
            } => Some((from, hash)),
            _ => None,
        }
    }

    /// Check for conflicts with another operation
    pub fn conflicts_with(&self, other: &BatchOperation) -> bool {
        let self_files = self.affected_files();
        let other_files = other.affected_files();

        // Check if any files overlap
        for file in &self_files {
            if other_files.contains(file) {
                // Allow if both are reads (UpdateLinks on same file), but not if either is a write
                match (self, other) {
                    (Self::UpdateLinks { .. }, Self::UpdateLinks { .. }) => {
                        // Multiple reads are OK
                        continue;
                    }
                    _ => return true, // Write conflict
                }
            }
        }

        false
    }
}

/// Record of a single executed operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationRecord {
    /// Index in the batch
    pub operation_index: usize,
    /// The operation that was executed
    pub operation: String,
    /// Result of execution (success or error)
    pub success: bool,
    /// Error message if failed
    pub error: Option<String>,
    /// Files affected
    pub affected_files: Vec<String>,
}

/// Result of batch execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    /// Whether all operations succeeded
    pub success: bool,
    /// Number of operations executed
    pub executed: usize,
    /// Total operations in batch
    pub total: usize,
    /// Index where failure occurred (if any)
    pub failed_at: Option<usize>,
    /// Changes made to files
    pub changes: Vec<String>,
    /// Errors encountered
    pub errors: Vec<String>,
    /// Execution records for each operation
    pub records: Vec<OperationRecord>,
    /// Unique transaction ID
    pub transaction_id: String,
    /// Execution duration in milliseconds
    pub duration_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_operation_affected_files() {
        let op = BatchOperation::MoveNote {
            from: "a.md".to_string(),
            to: "b.md".to_string(),
            expected_hash: None,
            dest_expected_hash: None,
            update_backlinks: None,
        };
        let affected = op.affected_files();
        assert_eq!(affected.len(), 2);
        assert!(affected.contains(&"a.md".to_string()));
        assert!(affected.contains(&"b.md".to_string()));
    }

    #[test]
    fn test_conflict_detection() {
        let op1 = BatchOperation::WriteNote {
            path: "file.md".to_string(),
            content: "content".to_string(),
            expected_hash: None,
        };
        let op2 = BatchOperation::DeleteNote {
            path: "file.md".to_string(),
            expected_hash: None,
            on_backlinks: None,
        };

        assert!(op1.conflicts_with(&op2));
        assert!(op2.conflicts_with(&op1));
    }

    #[test]
    fn test_no_conflict_different_files() {
        let op1 = BatchOperation::WriteNote {
            path: "file1.md".to_string(),
            content: "content".to_string(),
            expected_hash: None,
        };
        let op2 = BatchOperation::WriteNote {
            path: "file2.md".to_string(),
            content: "content".to_string(),
            expected_hash: None,
        };

        assert!(!op1.conflicts_with(&op2));
    }
}
