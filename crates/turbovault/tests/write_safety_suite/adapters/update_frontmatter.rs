//! `update_frontmatter` adapter — an in-place op (design doc §4 default:
//! `ExpectExists`, dirty-gated). Single-path → [`SinglePathOp`] mold.
//!
//! `invoke` drives the aspirational `update_frontmatter` op on the
//! tools-layer surface, passing the [`Precondition`] directly; the tool layer
//! does not take one yet (cutover: qae.9.1). The dirty-gate / precond-vs-workdir
//! cells are `pending` (the nbl.8 burndown).

use std::collections::HashMap;

use super::{Case, SinglePathOp};
use crate::harness::backend::{
    Backend, BatchWorld, Layer, MSG, ToolsWorld, WireWorld, observe, observe_outcome,
};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P, sentinel};
use crate::harness::state::GitState as S;
use turbovault_tools::BatchOperation;
use turbovault_tools::MetadataTools;

const KEY: &str = "wss_touched";

#[derive(Clone, Copy)]
pub struct UpdateFrontmatter;

/// Shared OK-effect check for every layer's invoker (op-specific, layer-agnostic).
fn ok_check(observed: &Observed) -> Result<(), String> {
    if observed
        .after_content
        .as_deref()
        .is_some_and(|c| c.contains(KEY))
    {
        Ok(())
    } else {
        Err(format!(
            "OK effect: frontmatter key {KEY:?} not present: {:?}",
            observed.after_content
        ))
    }
}

impl SinglePathOp<ToolsWorld> for UpdateFrontmatter {
    fn name(&self) -> &'static str {
        "update_frontmatter"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let mut fm = serde_json::Map::new();
        fm.insert(KEY.to_string(), serde_json::json!(true));
        let res = MetadataTools::new(w.vault().manager().clone())
            .update_frontmatter(rel, fm, true, pc, MSG)
            .await
            .map(|_| ());
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

// Batch-layer invoker (qae.9.3): the frontmatter update as a ONE-op
// `UpdateFrontmatter` batch (`merge: true`, matching the standalone arm).
// `blob_token` carries `ExpectBlob`, else a bare update. Shares `CASES`.
impl SinglePathOp<BatchWorld> for UpdateFrontmatter {
    fn name(&self) -> &'static str {
        "update_frontmatter"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &BatchWorld, rel: &str, pc: Precondition) -> Observed {
        let fm = HashMap::from([(KEY.to_string(), serde_json::json!(true))]);
        let op = BatchOperation::UpdateFrontmatter {
            path: rel.to_string(),
            frontmatter: fm,
            merge: Some(true),
            expected_hash: BatchWorld::blob_token(&pc),
        };
        w.run_batch_of_one(op, rel).await
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

// Wire-layer invoker (nbl.12): the real `update_frontmatter` MCP handler
// in-process. The handler has NO `expected_hash` wire param yet (it hardcodes
// ExpectExists — "pre-cutover parity, M5.3"), so the sentinel is passed against
// the NEEDED wire API and is currently ignored → cells fail until the wire-decode
// commit adds the param + decodes it. Shares `CASES` + `ok_check`.
impl SinglePathOp<WireWorld> for UpdateFrontmatter {
    fn name(&self) -> &'static str {
        "update_frontmatter"
    }
    fn cases(&self) -> &'static [Case] {
        CASES
    }
    async fn invoke(&self, w: &WireWorld, rel: &str, pc: Precondition) -> Observed {
        let mut fm = serde_json::Map::new();
        fm.insert(KEY.to_string(), serde_json::json!(true));
        let params = serde_json::json!({
            "path": rel,
            "frontmatter": fm,
            "merge": true,
            "expected_hash": sentinel(&pc),
        });
        observe_outcome(
            w.call_tool("update_frontmatter", params).await,
            w.vault().read(rel),
        )
    }
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        ok_check(observed)
    }
}

/// The **full** update_frontmatter matrix. In-place op → precondition axis
/// {Exists, Head, Index, Workdir, Wrong}; desired outcomes are identical to
/// `edit_note`'s (same matrix rows). `pending` = a cell current code gets wrong
/// (the nbl.8 burndown), with a trial-name-derived reason; `--include-ignored` is
/// the source of truth. The `e---u`/Untracked cells split the git arm (burndown)
/// from the direct arm (already correct → active).
const CASES: &[Case] = &[
    // ── ExpectExists (in-place default, dirty-gated) ─────────────────────────
    Case::new(P::Exists, S::Absent, O::NoFile),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    Case::new(P::Exists, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::pending(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
    // ── ExpectBlob(HEAD) — defined iff committed ─────────────────────────────
    Case::new(P::Head, S::CleanCommitted, O::Ok),
    Case::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ── ExpectBlob(INDEX) — defined iff staged ───────────────────────────────
    Case::pending(P::Index, S::CommittedStaged, O::Ok),
    Case::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::pending(P::Index, S::NewStaged, O::Ok),
    Case::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ── ExpectBlob(WORKDIR) — proving on-disk bytes; SKIP where == HEAD/INDEX ─
    Case::pending(P::Workdir, S::CommittedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::IntentToAdd, O::Ok),
    Case::pending(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::Untracked, O::Ok).on(Backend::Git),
    Case::new(P::Workdir, S::Untracked, O::Ok).on(Backend::Direct),
    // ── ExpectBlob(WRONG) → refuse everywhere; NoFile on absent ──────────────
    Case::new(P::Wrong, S::Absent, O::NoFile),
    Case::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::new(P::Wrong, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
];
