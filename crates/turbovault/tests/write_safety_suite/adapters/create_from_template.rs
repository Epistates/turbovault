//! `create_from_template` adapter — a **create** op (design doc §4 default:
//! `ExpectAbsent`), the same mold as `write_note`'s create path. Precondition
//! axis is {Blind, Absent} only (a create carries no in-place/blob token).
//!
//! `invoke` passes the [`Precondition`] straight to the tools-layer surface, which
//! takes one since the qae.9.1 cutover. No fixture is needed: the built-in
//! `research` template is always registered by `TemplateEngine::default_templates`.
//! Every cell here is ACTIVE. A strict create refuses an existing target on both
//! backends, and on git a `Blind` create over a dirty path is refused too: the strict
//! dirty-tree default (`dirty-tree-policy.md`) lets only the force switch, not yet
//! built, waive the dirty gate.

use std::collections::HashMap;

use crate::harness::backend::{
    Backend, BatchWorld, Layer, MSG, ToolsWorld, WireWorld, observe, observe_outcome,
};
use crate::harness::op::{Case, Op, OpAdapterMeta, content_contains};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P, sentinel};
use crate::harness::state::GitState as S;
use turbovault_tools::BatchOperation;
use turbovault_tools::TemplateEngine;

/// Stable text the `research` template always renders — proves an OK created it.
const RENDERED_MARKER: &str = "Key Findings";

/// The `research` template's fields (shared by every layer's invoker).
fn fields() -> HashMap<String, String> {
    HashMap::from([
        ("topic".to_string(), "wss".to_string()),
        ("date_researched".to_string(), "2026-01-01".to_string()),
    ])
}

#[derive(Clone, Copy)]
pub struct CreateFromTemplate;

impl OpAdapterMeta for CreateFromTemplate {
    fn name(&self) -> &'static str {
        "create_from_template"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        content_contains(observed, RENDERED_MARKER)
    }
}

impl Op<ToolsWorld> for CreateFromTemplate {
    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let res = TemplateEngine::new(w.vault().manager().clone())
            .create_from_template("research", rel, fields(), pc, MSG)
            .await
            .map(|_| ());
        observe(res, w.vault().read(rel))
    }
}

// Batch-layer invoker (qae.9.3): render+create as a ONE-op `CreateFromTemplate`
// batch. The precondition maps to `force`: `Blind` → force (upsert), `Absent` →
// strict create. Shares `CASES` (only Blind/Absent rows exist for a create).
impl Op<BatchWorld> for CreateFromTemplate {
    async fn invoke(&self, w: &BatchWorld, rel: &str, pc: Precondition) -> Observed {
        let force = match pc {
            Precondition::Blind => Some(true),
            // ExpectAbsent (and any other) → strict create (expect_absent).
            _ => None,
        };
        let op = BatchOperation::CreateFromTemplate {
            template_id: "research".to_string(),
            path: rel.to_string(),
            fields: fields(),
            force,
        };
        observe(w.apply_op(op).await, w.vault().read(rel))
    }
}

// Wire-layer invoker (nbl.12): the real `create_from_template` MCP handler
// in-process. NOTE the wire shape: `file_path` (not `path`) + `fields` as a JSON
// STRING. No `expected_hash` wire param yet (hardcoded ExpectAbsent), so the
// sentinel is aspirational. Shares `CASES` + `ok_check`.
impl Op<WireWorld> for CreateFromTemplate {
    async fn invoke(&self, w: &WireWorld, rel: &str, pc: Precondition) -> Observed {
        let params = serde_json::json!({
            "template_id": "research",
            "file_path": rel,
            "fields": serde_json::to_string(&fields()).unwrap(),
            "expected_hash": sentinel(&pc),
        });
        observe_outcome(
            w.call_tool("create_from_template", params).await,
            w.vault().read(rel),
        )
    }
}

/// The **full** `create_from_template` matrix (Blind + Absent rows of the CSV).
/// Blind (force) overwrites/creates unconditionally on a clean or absent target, and
/// always on direct. On git a dirty path is refused whatever the precondition: Blind
/// is the force hatch, and the strict default keeps it off until the `allow_blind`
/// switch exists. Absent is a strict create → Ok on an absent target, else refuse.
/// The `e---u`/Untracked cells split the two backends with DIFFERENT `expected`,
/// straight from the CSV's `backend` column: on git it is a dirty untracked tree
/// (refuse), on direct it is merely a present file (direct is git-blind).
const CASES: &[Case] = &[
    // ── Blind (force overwrite/create) → OK on clean/absent; git dirty refuses ─
    Case::new(P::Blind, S::Absent, O::Ok),
    Case::new(P::Blind, S::CleanCommitted, O::Ok),
    Case::new(P::Blind, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Blind, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Blind, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Blind, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Blind, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Blind, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Blind, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::new(P::Blind, S::Untracked, O::Ok).on(Backend::Direct),
    // ── ExpectAbsent (strict create) → OK on absent, else refuse ─────────────
    Case::new(P::Absent, S::Absent, O::Ok),
    Case::new(P::Absent, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Absent, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Absent, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Absent, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Absent, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Absent, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::new(P::Absent, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
];
