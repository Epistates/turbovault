//! `move_note` adapter — **dual-path** (design doc §4: source `ExpectExists`,
//! destination `ExpectAbsent` = the clobber protection), expressed as **two
//! ordinary [`SinglePathOp`] ops** so it rides the same mold as every other op:
//!
//! - [`MoveSrc`] — the varied path (`REL`) is the **source**; the destination is
//!   held absent (`to = ExpectAbsent`). Same shape as delete/edit's in-place rows.
//! - [`MoveDest`] — the varied path (`REL`) is the **destination**; the source is
//!   held present (`from = ExpectExists`). `ExpectAbsent` on the dest is the
//!   clobber guard.
//!
//! Each op implements `SinglePathOp<W>` for **every** world (Tools, Manager,
//! Batch) over ONE shared `Case` table per op — the required behavior is
//! layer-invariant, so all worlds run the exact same cells. A world that can't
//! meet a cell FAILS it (that divergence is the finding); we never fork a
//! per-world table.
//!
//! **The batch arm is written against the API we NEED, not the API we HAVE**
//! (WSS is aspirational): a one-op `MoveNote` batch that carries first-class
//! source AND destination `Precondition`s. `BatchOperation::MoveNote` has only a
//! source `expected_hash` today and hardcodes `expect_absent(to)`, so
//! [`move_batch`] does **not compile** until that API lands (turbovault-qae.6.4).
//! That compilation failure is the intentional signal — the batch surface must
//! gain dual preconditions for move to be clobber-safe on the wire.

use std::sync::Arc;

use super::{Case, SinglePathOp, present_state};
use crate::harness::backend::{
    Backend, BatchWorld, Layer, MSG, ManagerWorld, ToolsWorld, WireWorld, observe, observe_outcome,
};
use crate::harness::outcome::{Observed, ObservedError, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P, sentinel};
use crate::harness::state::GitState as S;
use turbovault_tools::{BatchOperation, BatchTools, FileTools};
use turbovault_vault::VaultManager;

/// The path held fixed while `REL` is the varied path: the destination for the
/// [`MoveSrc`] sweep, the source for the [`MoveDest`] sweep.
const OTHER: &str = "other.md";

// ── The per-layer move call (the only layer-specific part) ───────────────────
// Each is `move(from, to, from_pc, to_pc)`. Tools/Manager drive the real
// `move_file`; Batch drives a one-op `MoveNote` batch through `plan`/
// `apply_changes` (never `batch_execute`, whose soft envelope would stringify
// the error kind the Outcome assertions need).

async fn move_tools(
    mgr: Arc<VaultManager>,
    from: &str,
    to: &str,
    from_pc: Precondition,
    to_pc: Precondition,
) -> Result<(), turbovault_core::Error> {
    FileTools::new(mgr)
        .move_file(from, to, from_pc, to_pc, MSG)
        .await
}

async fn move_manager(
    mgr: Arc<VaultManager>,
    from: &str,
    to: &str,
    from_pc: Precondition,
    to_pc: Precondition,
) -> Result<(), turbovault_core::Error> {
    mgr.move_file(
        std::path::Path::new(from),
        std::path::Path::new(to),
        from_pc,
        to_pc,
        MSG,
    )
    .await
}

async fn move_batch(
    mgr: Arc<VaultManager>,
    from: &str,
    to: &str,
    from_pc: Precondition,
    to_pc: Precondition,
) -> Result<(), turbovault_core::Error> {
    // NEEDED API (turbovault-qae.6.4): batch `MoveNote` gains a `dest_expected_hash`
    // parallel to the source `expected_hash`, both `Option<String>` (sentinel|oid),
    // matching every other batch op's shape. The source keeps the existing
    // `blob_token` idiom; the destination is sentinel-encoded so it can express
    // the full dest axis (absent/exists/blind/oid), replacing the hardcoded
    // `expect_absent(to)`. `dest_expected_hash` does NOT exist until qae.6.4 adds
    // it, so this won't compile — the intentional WSS signal.
    let op = BatchOperation::MoveNote {
        from: from.to_string(),
        to: to.to_string(),
        expected_hash: BatchWorld::blob_token(&from_pc),
        dest_expected_hash: sentinel(&to_pc),
        update_backlinks: None,
    };
    let mut plan = BatchTools::new(mgr.clone()).plan(&[op]).await?;
    plan.message = MSG.to_string();
    mgr.apply_changes(&plan).await.map(|_| ())
}

/// The wire move: drive the real `move_note` MCP handler in-process, encoding
/// BOTH preconditions as sentinel strings. `dest_expected_hash` is the NEEDED
/// wire param (the handler has only a source `expected_hash` today), so the dest
/// sweep is aspirational until the wire-decode commit adds it. `update_backlinks:
/// false` uses the plain move path (note.md has no linkers either way).
async fn move_wire(
    w: &WireWorld,
    from: &str,
    to: &str,
    from_pc: Precondition,
    to_pc: Precondition,
) -> Result<(), ObservedError> {
    let params = serde_json::json!({
        "from": from,
        "to": to,
        "expected_hash": sentinel(&from_pc),
        "dest_expected_hash": sentinel(&to_pc),
        "update_backlinks": false,
    });
    w.call_tool("move_note", params).await
}

// ── MoveSrc: vary the source (REL), destination held absent ──────────────────

#[derive(Clone, Copy)]
pub struct MoveSrc;

/// OK effect for a source-sweep move: the source (the varied path) is **gone**.
fn src_ok(observed: &Observed) -> Result<(), String> {
    if observed.after_content.is_none() {
        Ok(())
    } else {
        Err(format!(
            "OK move: source still present after move: {:?}",
            observed.after_content
        ))
    }
}

impl SinglePathOp<ToolsWorld> for MoveSrc {
    fn name(&self) -> &'static str {
        "move_note::src"
    }
    fn cases(&self) -> &'static [Case] {
        SRC_CASES
    }
    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let res = move_tools(
            w.vault().manager().clone(),
            rel,
            OTHER,
            pc,
            Precondition::ExpectAbsent,
        )
        .await;
        observe(res, w.vault().read(rel))
    }
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        src_ok(observed)
    }
}

impl SinglePathOp<ManagerWorld> for MoveSrc {
    fn name(&self) -> &'static str {
        "move_note::src"
    }
    fn cases(&self) -> &'static [Case] {
        SRC_CASES
    }
    async fn invoke(&self, w: &ManagerWorld, rel: &str, pc: Precondition) -> Observed {
        let res = move_manager(
            w.vault().manager().clone(),
            rel,
            OTHER,
            pc,
            Precondition::ExpectAbsent,
        )
        .await;
        observe(res, w.vault().read(rel))
    }
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        src_ok(observed)
    }
}

impl SinglePathOp<BatchWorld> for MoveSrc {
    fn name(&self) -> &'static str {
        "move_note::src"
    }
    fn cases(&self) -> &'static [Case] {
        SRC_CASES
    }
    async fn invoke(&self, w: &BatchWorld, rel: &str, pc: Precondition) -> Observed {
        let res = move_batch(
            w.vault().manager().clone(),
            rel,
            OTHER,
            pc,
            Precondition::ExpectAbsent,
        )
        .await;
        observe(res, w.vault().read(rel))
    }
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        src_ok(observed)
    }
}

impl SinglePathOp<WireWorld> for MoveSrc {
    fn name(&self) -> &'static str {
        "move_note::src"
    }
    fn cases(&self) -> &'static [Case] {
        SRC_CASES
    }
    async fn invoke(&self, w: &WireWorld, rel: &str, pc: Precondition) -> Observed {
        let res = move_wire(w, rel, OTHER, pc, Precondition::ExpectAbsent).await;
        observe_outcome(res, w.vault().read(rel))
    }
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        src_ok(observed)
    }
}

// ── MoveDest: vary the destination (REL), source held present ────────────────

#[derive(Clone, Copy)]
pub struct MoveDest;

/// OK effect for a dest-sweep move: the destination (the varied path) is
/// **present** after the move.
fn dest_ok(observed: &Observed) -> Result<(), String> {
    if observed.after_content.is_some() {
        Ok(())
    } else {
        Err("OK move: destination missing after move".into())
    }
}

impl SinglePathOp<ToolsWorld> for MoveDest {
    fn name(&self) -> &'static str {
        "move_note::dest"
    }
    fn cases(&self) -> &'static [Case] {
        DEST_CASES
    }
    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        w.vault()
            .build_state(OTHER, present_state(w.vault().backend()));
        let res = move_tools(
            w.vault().manager().clone(),
            OTHER,
            rel,
            Precondition::ExpectExists,
            pc,
        )
        .await;
        observe(res, w.vault().read(rel))
    }
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        dest_ok(observed)
    }
}

impl SinglePathOp<ManagerWorld> for MoveDest {
    fn name(&self) -> &'static str {
        "move_note::dest"
    }
    fn cases(&self) -> &'static [Case] {
        DEST_CASES
    }
    async fn invoke(&self, w: &ManagerWorld, rel: &str, pc: Precondition) -> Observed {
        w.vault()
            .build_state(OTHER, present_state(w.vault().backend()));
        let res = move_manager(
            w.vault().manager().clone(),
            OTHER,
            rel,
            Precondition::ExpectExists,
            pc,
        )
        .await;
        observe(res, w.vault().read(rel))
    }
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        dest_ok(observed)
    }
}

impl SinglePathOp<BatchWorld> for MoveDest {
    fn name(&self) -> &'static str {
        "move_note::dest"
    }
    fn cases(&self) -> &'static [Case] {
        DEST_CASES
    }
    async fn invoke(&self, w: &BatchWorld, rel: &str, pc: Precondition) -> Observed {
        w.vault()
            .build_state(OTHER, present_state(w.vault().backend()));
        let res = move_batch(
            w.vault().manager().clone(),
            OTHER,
            rel,
            Precondition::ExpectExists,
            pc,
        )
        .await;
        observe(res, w.vault().read(rel))
    }
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        dest_ok(observed)
    }
}

impl SinglePathOp<WireWorld> for MoveDest {
    fn name(&self) -> &'static str {
        "move_note::dest"
    }
    fn cases(&self) -> &'static [Case] {
        DEST_CASES
    }
    async fn invoke(&self, w: &WireWorld, rel: &str, pc: Precondition) -> Observed {
        w.vault()
            .build_state(OTHER, present_state(w.vault().backend()));
        let res = move_wire(w, OTHER, rel, Precondition::ExpectExists, pc).await;
        observe_outcome(res, w.vault().read(rel))
    }
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        dest_ok(observed)
    }
}

// ── SOURCE sweep — destination held absent (`to = ExpectAbsent`) ─────────────
// Same shape as delete/edit's in-place rows: the source must exist and match.
// The desired outcome is layer-invariant; `pending` marks a cell the current
// code gets wrong (the nbl.8 / 9n6 burndown), shared across all worlds. The
// `e---u`/Untracked cells split the git arm (burndown) from the direct arm
// (already correct) via `.on()` — SAME expected, differing only in the flag.
const SRC_CASES: &[Case] = &[
    // ExpectExists (in-place default, dirty-gated) — the source is the removed
    // target; NoFile-on-absent must precede the precondition check.
    Case::pending(P::Exists, S::Absent, O::NoFile),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    Case::new(P::Exists, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Exists, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Exists, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::pending(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
    // ExpectBlob(HEAD) — defined iff committed
    Case::new(P::Head, S::CleanCommitted, O::Ok),
    Case::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(INDEX) — defined iff staged
    Case::pending(P::Index, S::CommittedStaged, O::Ok),
    Case::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::pending(P::Index, S::NewStaged, O::Ok),
    Case::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(WORKDIR) — SKIP where == HEAD/INDEX
    Case::pending(P::Workdir, S::CommittedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::IntentToAdd, O::Ok),
    Case::pending(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::Untracked, O::Ok).on(Backend::Git),
    Case::new(P::Workdir, S::Untracked, O::Ok).on(Backend::Direct),
    // ExpectBlob(WRONG) → refuse everywhere; NoFile on absent
    Case::pending(P::Wrong, S::Absent, O::NoFile),
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

// ── DEST sweep — source held clean present (`from = ExpectExists`) ───────────
// The destination is the write target; `ExpectAbsent` on it is the clobber
// guard. Today's `move_file` doesn't honor an arbitrary dest precondition (it
// hardcodes `expect_absent(to)`), so cells whose desired outcome diverges from
// that are `pending` on the dual-path move burndown (turbovault-9n6) — shared
// across all worlds; a world that can't meet an active cell FAILS it.
const DEST_CASES: &[Case] = &[
    // Blind → overwrite the destination unconditionally.
    Case::new(P::Blind, S::Absent, O::Ok),
    Case::new(P::Blind, S::CleanCommitted, O::Ok),
    Case::pending(P::Blind, S::CommittedStaged, O::Ok),
    Case::pending(P::Blind, S::CommittedUnstaged, O::Ok),
    Case::pending(P::Blind, S::CommittedStagedUnstaged, O::Ok),
    Case::pending(P::Blind, S::NewStaged, O::Ok),
    Case::pending(P::Blind, S::IntentToAdd, O::Ok),
    Case::pending(P::Blind, S::NewStagedUnstaged, O::Ok),
    Case::pending(P::Blind, S::Untracked, O::Ok).on(Backend::Git),
    Case::new(P::Blind, S::Untracked, O::Ok).on(Backend::Direct),
    // ExpectAbsent (clobber protection) → OK on absent dest, else refuse.
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
    // ExpectBlob(HEAD) on the dest — defined iff dest committed
    Case::new(P::Head, S::CleanCommitted, O::Ok),
    Case::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(INDEX) on the dest — defined iff dest staged
    Case::pending(P::Index, S::CommittedStaged, O::Ok),
    Case::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::pending(P::Index, S::NewStaged, O::Ok),
    Case::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(WORKDIR) on the dest — SKIP where == HEAD/INDEX
    Case::pending(P::Workdir, S::CommittedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::IntentToAdd, O::Ok),
    Case::pending(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Case::pending(P::Workdir, S::Untracked, O::Ok).on(Backend::Git),
    Case::new(P::Workdir, S::Untracked, O::Ok).on(Backend::Direct),
    // ExpectBlob(WRONG) on the dest → refuse everywhere (incl. absent)
    Case::new(P::Wrong, S::Absent, O::ConcurrencyError),
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
