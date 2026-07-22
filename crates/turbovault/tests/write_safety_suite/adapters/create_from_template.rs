//! `create_from_template` adapter — a **create** op (design doc §4 default:
//! `ExpectAbsent`), the same mold as `write_note`'s create path. Precondition
//! axis is {Blind, Absent} only (a create carries no in-place/blob token).
//!
//! `invoke` drives the aspirational `create_from_template` op on the
//! tools-layer surface, passing the [`Precondition`] directly; the tool layer
//! does not take one yet (cutover: qae.9.1). No fixture is needed: the built-in
//! `research` template is always registered by `TemplateEngine::default_templates`.
//! The `ExpectAbsent`-clobber cells on uncommitted-present state are `pending`.

use std::collections::HashMap;

use super::{Case, SinglePathOp};
use crate::harness::backend::{Layer, MSG, ToolsWorld, observe};
use turbovault_tools::TemplateEngine;
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;

/// Stable text the `research` template always renders — proves an OK created it.
const RENDERED_MARKER: &str = "Key Findings";

#[derive(Clone, Copy)]
pub struct CreateFromTemplate;

impl SinglePathOp<ToolsWorld> for CreateFromTemplate {
    fn name(&self) -> &'static str {
        "create_from_template"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let fields = HashMap::from([
            ("topic".to_string(), "wss".to_string()),
            ("date_researched".to_string(), "2026-01-01".to_string()),
        ]);
        let res = TemplateEngine::new(w.vault().manager().clone())
            .create_from_template("research", rel, fields, pc, MSG)
            .await
            .map(|_| ());
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed
            .after_content
            .as_deref()
            .is_some_and(|c| c.contains(RENDERED_MARKER))
        {
            Ok(())
        } else {
            Err(format!(
                "OK effect: rendered template not present: {:?}",
                observed.after_content
            ))
        }
    }
}

/// The **full** create_from_template matrix (Blind + Absent rows of the CSV).
/// Blind (force) overwrites/creates unconditionally → Ok everywhere. Absent is a
/// strict create → Ok on an absent target, else refuse. All `Case::new`: spec-
/// first asserts the target contract.
const CASES: &[Case] = &[
    // ── Blind (force overwrite/create) → OK in every state ───────────────────
    Case::new(P::Blind, S::Absent, O::Ok),
    Case::new(P::Blind, S::CleanCommitted, O::Ok),
    Case::new(P::Blind, S::CommittedStaged, O::Ok),
    Case::new(P::Blind, S::CommittedUnstaged, O::Ok),
    Case::new(P::Blind, S::CommittedStagedUnstaged, O::Ok),
    Case::new(P::Blind, S::NewStaged, O::Ok),
    Case::new(P::Blind, S::IntentToAdd, O::Ok),
    Case::new(P::Blind, S::NewStagedUnstaged, O::Ok),
    Case::new(P::Blind, S::Untracked, O::Ok),
    // ── ExpectAbsent (strict create) → OK on absent, else refuse ─────────────
    Case::new(P::Absent, S::Absent, O::Ok),
    Case::new(P::Absent, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // Uncommitted-but-present: HEAD has no entry, so expect_absent (checked vs
    // HEAD today) wrongly passes and the create clobbers rather than refuses.
    Case::pending(P::Absent, S::NewStaged, O::ConcurrencyError, ABSENT_CLOBBER),
    Case::pending(
        P::Absent,
        S::IntentToAdd,
        O::ConcurrencyError,
        ABSENT_CLOBBER,
    ),
    Case::pending(
        P::Absent,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        ABSENT_CLOBBER,
    ),
    Case::pending(P::Absent, S::Untracked, O::ConcurrencyError, ABSENT_CLOBBER),
];

// Burndown reason (nbl.8) — the aspirational behavior the cutover defers.
const ABSENT_CLOBBER: &str =
    "WSS: expect_absent checks HEAD, so an uncommitted-but-present file is clobbered, not refused";
