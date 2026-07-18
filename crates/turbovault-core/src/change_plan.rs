//! `Change` + `ChangePlan` (write-substrate-layering design §5.2 / §6.1) — the
//! single, backend-agnostic mutation vocabulary.
//!
//! Today mutation is expressed three incompatible ways (VaultManager's
//! `path+content+hash` mutators, `BatchExecutor`'s `BatchOperation` list, and
//! `turbovault-git`'s `Changeset`). `ChangePlan` collapses all of them to one
//! representation: a single-op write is a one-change plan, a batch is an
//! N-change plan, and both the direct and git substrates implement exactly one
//! entry point, `apply(&ChangePlan)`.
//!
//! This type is git2-free by design (§6.1, §11.9): version tokens are hex
//! `String`s (see [`crate::Precondition`]); only `turbovault-git` ever parses a
//! token to an `Oid`, at the substrate boundary.

use crate::precondition::Precondition;

/// One file-level edit in a [`ChangePlan`].
///
/// `Rename` is a first-class variant (not remove+upsert) — the git substrate
/// reads `from`'s bytes at apply time, under the commit lock, and folds it to
/// remove+upsert itself; the direct substrate does an `fs::rename`. Neither
/// substrate needs the caller to pass the bytes being moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Add a new file or overwrite an existing one with `content`.
    Upsert { path: String, content: Vec<u8> },
    /// Remove a file.
    Remove { path: String },
    /// Move `from` to `to`. Content is read from `from` at apply time.
    Rename { from: String, to: String },
}

impl Change {
    /// The path a reader would look for this change under: `path` for
    /// `Upsert`/`Remove`, `from` for `Rename` (its source-side precondition).
    pub fn path(&self) -> &str {
        match self {
            Change::Upsert { path, .. } | Change::Remove { path } => path,
            Change::Rename { from, .. } => from,
        }
    }
}

/// A backend-agnostic description of one mutation: an ordered set of
/// [`Change`]s, a per-path [`Precondition`] for each, and a commit message.
/// The single plan type (§11.9) — `turbovault-git::Changeset` is deleted in
/// M2, replaced by this.
#[derive(Debug, Clone, Default)]
pub struct ChangePlan {
    pub message: String,
    pub changes: Vec<Change>,
    pub preconditions: Vec<(String, Precondition)>,
}

impl ChangePlan {
    /// Start a plan with a commit message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            ..Default::default()
        }
    }

    // -------- Raw builders --------
    // Compose changes and preconditions independently. Use these when the
    // safe-by-default semantic builders below don't fit (e.g. an explicit
    // Blind write, or a precondition over a path the plan doesn't mutate).

    /// Add or overwrite a file, with no precondition of its own.
    pub fn upsert(mut self, path: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        self.changes.push(Change::Upsert {
            path: path.into(),
            content: content.into(),
        });
        self
    }

    /// Remove a file, with no precondition of its own.
    pub fn remove(mut self, path: impl Into<String>) -> Self {
        self.changes.push(Change::Remove { path: path.into() });
        self
    }

    /// Add an arbitrary [`Change`].
    pub fn with_change(mut self, change: Change) -> Self {
        self.changes.push(change);
        self
    }

    /// Require `path` to currently hold exactly `token` (the version token
    /// the caller read).
    pub fn expect_blob(mut self, path: impl Into<String>, token: impl Into<String>) -> Self {
        self.preconditions
            .push((path.into(), Precondition::ExpectBlob(token.into())));
        self
    }

    /// Require `path` to currently be absent (a create).
    pub fn expect_absent(mut self, path: impl Into<String>) -> Self {
        self.preconditions
            .push((path.into(), Precondition::ExpectAbsent));
        self
    }

    /// Add an arbitrary `(path, Precondition)` (e.g. over a path the plan
    /// reads but does not mutate, extending the multi-file CAS to the plan's
    /// read set).
    pub fn with_precondition(
        mut self,
        path: impl Into<String>,
        precondition: Precondition,
    ) -> Self {
        self.preconditions.push((path.into(), precondition));
        self
    }

    // -------- Semantic builders --------
    // Compose the raw primitives with the safe-by-default precondition
    // policy. Use these for the standard ops; reach for the raw builders only
    // when blindness is explicitly wanted.

    /// Create a new file. Precondition: `path` must currently be absent.
    pub fn create(mut self, path: impl Into<String>, content: impl Into<Vec<u8>>) -> Self {
        let path = path.into();
        self.changes.push(Change::Upsert {
            path: path.clone(),
            content: content.into(),
        });
        self.preconditions.push((path, Precondition::ExpectAbsent));
        self
    }

    /// Update an existing file. Precondition: `path` must currently hold
    /// `expected` (the version token the caller read).
    pub fn update(
        mut self,
        path: impl Into<String>,
        content: impl Into<Vec<u8>>,
        expected: impl Into<String>,
    ) -> Self {
        let path = path.into();
        self.changes.push(Change::Upsert {
            path: path.clone(),
            content: content.into(),
        });
        self.preconditions
            .push((path, Precondition::ExpectBlob(expected.into())));
        self
    }

    /// Delete an existing file. Precondition: it must currently hold
    /// `expected`.
    pub fn delete(mut self, path: impl Into<String>, expected: impl Into<String>) -> Self {
        let path = path.into();
        self.changes.push(Change::Remove { path: path.clone() });
        self.preconditions
            .push((path, Precondition::ExpectBlob(expected.into())));
        self
    }

    /// Move `from` to `to`, as one plan entry. Preconditions: `from` must
    /// currently hold `expected_from`; `to` must currently be absent (no
    /// clobbering the destination).
    pub fn rename(
        mut self,
        from: impl Into<String>,
        to: impl Into<String>,
        expected_from: impl Into<String>,
    ) -> Self {
        let from = from.into();
        let to = to.into();
        self.changes.push(Change::Rename {
            from: from.clone(),
            to: to.clone(),
        });
        self.preconditions
            .push((from, Precondition::ExpectBlob(expected_from.into())));
        self.preconditions.push((to, Precondition::ExpectAbsent));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_pushes_upsert_and_expect_absent() {
        let plan = ChangePlan::new("create a.md").create("a.md", "alpha");
        assert_eq!(
            plan.changes,
            vec![Change::Upsert {
                path: "a.md".into(),
                content: b"alpha".to_vec(),
            }]
        );
        assert_eq!(
            plan.preconditions,
            vec![("a.md".to_string(), Precondition::ExpectAbsent)]
        );
    }

    #[test]
    fn update_pushes_upsert_and_expect_blob() {
        let plan = ChangePlan::new("update a.md").update("a.md", "beta", "deadbeef");
        assert_eq!(
            plan.changes,
            vec![Change::Upsert {
                path: "a.md".into(),
                content: b"beta".to_vec(),
            }]
        );
        assert_eq!(
            plan.preconditions,
            vec![(
                "a.md".to_string(),
                Precondition::ExpectBlob("deadbeef".to_string())
            )]
        );
    }

    #[test]
    fn delete_pushes_remove_and_expect_blob() {
        let plan = ChangePlan::new("delete a.md").delete("a.md", "deadbeef");
        assert_eq!(
            plan.changes,
            vec![Change::Remove {
                path: "a.md".into()
            }]
        );
        assert_eq!(
            plan.preconditions,
            vec![(
                "a.md".to_string(),
                Precondition::ExpectBlob("deadbeef".to_string())
            )]
        );
    }

    #[test]
    fn rename_pushes_rename_change_and_both_preconditions() {
        let plan = ChangePlan::new("move a.md to b.md").rename("a.md", "b.md", "deadbeef");
        assert_eq!(
            plan.changes,
            vec![Change::Rename {
                from: "a.md".into(),
                to: "b.md".into(),
            }]
        );
        assert_eq!(
            plan.preconditions,
            vec![
                (
                    "a.md".to_string(),
                    Precondition::ExpectBlob("deadbeef".to_string())
                ),
                ("b.md".to_string(), Precondition::ExpectAbsent),
            ]
        );
    }

    #[test]
    fn raw_builders_compose_independently_of_semantic_ones() {
        let plan = ChangePlan::new("blind write")
            .upsert("a.md", "alpha")
            .with_precondition("b.md", Precondition::ExpectExists)
            .expect_blob("c.md", "cafebabe")
            .expect_absent("d.md")
            .with_change(Change::Remove {
                path: "e.md".into(),
            });

        assert_eq!(plan.changes.len(), 2);
        assert_eq!(plan.preconditions.len(), 3);
        assert!(
            plan.preconditions
                .contains(&("b.md".to_string(), Precondition::ExpectExists))
        );
        assert!(plan.preconditions.contains(&(
            "c.md".to_string(),
            Precondition::ExpectBlob("cafebabe".to_string())
        )));
        assert!(
            plan.preconditions
                .contains(&("d.md".to_string(), Precondition::ExpectAbsent))
        );
    }

    #[test]
    fn change_path_returns_the_precondition_side_for_each_variant() {
        assert_eq!(
            Change::Upsert {
                path: "a.md".into(),
                content: vec![],
            }
            .path(),
            "a.md"
        );
        assert_eq!(
            Change::Remove {
                path: "b.md".into()
            }
            .path(),
            "b.md"
        );
        assert_eq!(
            Change::Rename {
                from: "c.md".into(),
                to: "d.md".into(),
            }
            .path(),
            "c.md"
        );
    }

    #[test]
    fn message_and_new_default_empty() {
        let plan = ChangePlan::new("hello");
        assert_eq!(plan.message, "hello");
        assert!(plan.changes.is_empty());
        assert!(plan.preconditions.is_empty());
    }
}
