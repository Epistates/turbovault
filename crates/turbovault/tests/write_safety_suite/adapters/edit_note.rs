//! `edit_note` adapter — an in-place op (design doc §4 default: `ExpectExists`,
//! dirty-gated). Single-path, so it rides the [`Op`] mold.
//!
//! It surfaces one new primitive need: the SEARCH-not-found case is an
//! [`Outcome::OpError`] — a refusal that is neither a concurrency conflict nor a
//! missing file. That's an op-specific one-off (it varies the SEARCH text, not
//! the precondition/state), so it's a bespoke trial ([`extra_trials`]) rather
//! than a grid cell.

use libtest_mimic::Trial;

use crate::harness::backend::{
    Backend, BatchWorld, Layer, MSG, ManagerWorld, ToolsWorld, WireWorld, observe, observe_outcome,
};
use crate::harness::op::{
    Case, Op, OpAdapterMeta, REL, cell_trial, content_contains, present_state,
};
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P, sentinel};
use crate::harness::state::GitState as S;
use turbovault_tools::BatchOperation;
use turbovault_tools::FileTools;

/// The replacement text — `ok_effect` checks the edited file contains it.
const NEW: &str = "wss-edited";

#[derive(Clone, Copy)]
pub struct EditNote;

/// A whole-content SEARCH/REPLACE block: SEARCH the file's current bytes,
/// replace with [`NEW`]. Matches whatever generation the state left on disk.
fn edits_replacing(current: &str) -> String {
    format!("<<<<<<< SEARCH\n{current}=======\n{NEW}\n>>>>>>> REPLACE\n")
}

// Op-level, layer-agnostic surface: identity + shared `CASES` + the OK-effect check,
// stated ONCE (the per-layer invokers below carry only `invoke`).
impl OpAdapterMeta for EditNote {
    fn name(&self) -> &'static str {
        "edit_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        content_contains(observed, NEW)
    }
}

impl Op<ToolsWorld> for EditNote {
    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let current = w.vault().read(rel).unwrap_or_default();
        let edits = edits_replacing(&current);
        let res = FileTools::new(w.vault().manager().clone())
            .edit_file(rel, &edits, pc, false, MSG)
            .await
            .map(|_| ());
        observe(res, w.vault().read(rel))
    }
}

// Manager-layer invoker (qae.9.2): call `VaultManager::edit_file` directly — the
// enforcement/SDK surface, one rung below the tools wrapper (which is a thin
// delegator to this same method). Shares `CASES` + `ok_check` via `OpAdapterMeta`.
impl Op<ManagerWorld> for EditNote {
    async fn invoke(&self, w: &ManagerWorld, rel: &str, pc: Precondition) -> Observed {
        let current = w.vault().read(rel).unwrap_or_default();
        let edits = edits_replacing(&current);
        let res = w
            .vault()
            .manager()
            .edit_file(std::path::Path::new(rel), &edits, pc, false, MSG)
            .await
            .map(|_| ());
        observe(res, w.vault().read(rel))
    }
}

// Batch-layer invoker (qae.9.3): the edit as a ONE-op `EditNote` batch. The SEARCH
// block is computed from the on-disk bytes exactly as the standalone arm;
// `blob_token` carries `ExpectBlob`, else a bare edit (the fold's read + the
// substrate dirty gate enforce existence). `observe(w.apply_op(..))` is the same
// shape as the Tools/Manager arms.
impl Op<BatchWorld> for EditNote {
    async fn invoke(&self, w: &BatchWorld, rel: &str, pc: Precondition) -> Observed {
        let current = w.vault().read(rel).unwrap_or_default();
        let edits = edits_replacing(&current);
        let op = BatchOperation::EditNote {
            path: rel.to_string(),
            edits,
            expected_hash: BatchWorld::blob_token(&pc),
        };
        observe(w.apply_op(op).await, w.vault().read(rel))
    }
}

// Wire-layer invoker (nbl.12): the real `edit_note` MCP handler in-process; the
// SEARCH block is computed from the on-disk bytes as elsewhere, the precondition
// encoded as the sentinel `expected_hash`. Shares `CASES` + `ok_check`.
impl Op<WireWorld> for EditNote {
    async fn invoke(&self, w: &WireWorld, rel: &str, pc: Precondition) -> Observed {
        let current = w.vault().read(rel).unwrap_or_default();
        let edits = edits_replacing(&current);
        let params = serde_json::json!({
            "path": rel,
            "edits": edits,
            "expected_hash": sentinel(&pc),
        });
        observe_outcome(w.call_tool("edit_note", params).await, w.vault().read(rel))
    }
}

/// The **full** `edit_note` matrix, transcribed from the corrected CSV (`edit_note`
/// is the 2nd operation there). In-place op → precondition axis
/// {Exists, Head, Index, Workdir, Wrong} (no Blind/Absent). N/A cells (token
/// undefined for the state) and SKIP duplicates (WORKDIR == HEAD/INDEX) are
/// omitted. `pending` = a cell current code gets wrong (the nbl.8 burndown), with
/// a trial-name-derived reason; `--include-ignored` is the source of truth. The
/// `e---u`/Untracked cell splits the two backends with DIFFERENT `expected`: on git
/// it is a dirty untracked tree (refuse), on direct it is merely a present file, so
/// `ExpectExists` is satisfied and the write proceeds. Both values come from the
/// CSV's `backend` column and `just wss-audit` enforces them.
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
    // DIFFERENT `expected` per backend, straight from the CSV's `backend` column:
    // on git `e---u` is a dirty untracked tree, so an in-place edit refuses; on
    // direct it is merely a present file (direct is git-blind), so `ExpectExists`
    // is satisfied and the edit proceeds. Not a backend lag — do not unify them.
    Case::new(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Case::new(P::Exists, S::Untracked, O::Ok).on(Backend::Direct),
    // ── ExpectBlob(HEAD) — defined iff committed (HEAD-token refusal already unified) ─
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
    // ── ExpectBlob(WRONG) → refuse everywhere; NoFile on absent (in-place) ────
    Case::new(P::Wrong, S::Absent, O::NoFile),
    Case::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Case::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Wrong, S::Untracked, O::ConcurrencyError),
];

/// Op-specific one-offs (outside the precondition × state grid): a SEARCH that
/// matches nothing is an `OpError` — the op refuses, the working tree untouched.
pub fn extra_trials(backend: Backend) -> Vec<Trial> {
    vec![cell_trial(
        format!(
            "{}::{}::edit_note::one-off::search-not-found::OpError",
            ToolsWorld::LABEL,
            backend.code()
        ),
        false,
        move || async move {
            let w = ToolsWorld::new(backend);
            let _ = w.vault().build_state(REL, present_state(backend));
            let before = w.vault().read(REL);

            let edits = "<<<<<<< SEARCH\nNONEXISTENT-TEXT\n=======\nx\n>>>>>>> REPLACE\n";
            let res = FileTools::new(w.vault().manager().clone())
                .edit_file(REL, edits, Precondition::ExpectExists, false, MSG)
                .await
                .map(|_| ());

            O::OpError.check(&observe(res, w.vault().read(REL)), before.as_deref())
        },
    )]
}
