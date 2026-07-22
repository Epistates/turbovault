//! `edit_note` adapter — an in-place op (design doc §4 default: `ExpectExists`,
//! dirty-gated). Single-path, so it rides the [`SinglePathOp`] mold.
//!
//! It surfaces one new primitive need: the SEARCH-not-found case is an
//! [`Outcome::OpError`] — a refusal that is neither a concurrency conflict nor a
//! missing file. That's an op-specific one-off (it varies the SEARCH text, not
//! the precondition/state), so it's a bespoke trial ([`extra_trials`]) rather
//! than a grid cell.

use libtest_mimic::Trial;

use super::{Case, REL, SinglePathOp, cell_trial, present_state};
use crate::harness::backend::{Backend, Layer, MSG, ToolsWorld, observe};
use turbovault_tools::FileTools;
use crate::harness::outcome::{Observed, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;

/// The replacement text — `ok_effect` checks the edited file contains it.
const NEW: &str = "wss-edited";

#[derive(Clone, Copy)]
pub struct EditNote;

/// A whole-content SEARCH/REPLACE block: SEARCH the file's current bytes,
/// replace with [`NEW`]. Matches whatever generation the state left on disk.
fn edits_replacing(current: &str) -> String {
    format!("<<<<<<< SEARCH\n{current}=======\n{NEW}\n>>>>>>> REPLACE\n")
}

impl SinglePathOp<ToolsWorld> for EditNote {
    fn name(&self) -> &'static str {
        "edit_note"
    }

    fn cases(&self) -> &'static [Case] {
        CASES
    }

    async fn invoke(&self, w: &ToolsWorld, rel: &str, pc: Precondition) -> Observed {
        let current = w.vault().read(rel).unwrap_or_default();
        let edits = edits_replacing(&current);
        let res = FileTools::new(w.vault().manager().clone())
            .edit_file(rel, &edits, pc, false, MSG)
            .await
            .map(|_| ());
        observe(res, w.vault().read(rel))
    }

    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        if observed
            .after_content
            .as_deref()
            .is_some_and(|c| c.contains(NEW))
        {
            Ok(())
        } else {
            Err(format!(
                "OK effect: expected edited content containing {NEW:?}, got {:?}",
                observed.after_content
            ))
        }
    }
}

/// The **full** edit_note matrix, transcribed from the corrected CSV (edit_note
/// is the 2nd operation there). In-place op → precondition axis
/// {Exists, Head, Index, Workdir, Wrong} (no Blind/Absent). N/A cells (token
/// undefined for the state) and SKIP duplicates (WORKDIR == HEAD/INDEX) are
/// omitted. `pending` = a cell current code gets wrong (the burndown), set by
/// running the grid (design doc §6 empirical method).
const CASES: &[Case] = &[
    // ── ExpectExists (in-place default, dirty-gated) ─────────────────────────
    // Edit needs an existing file; clean file edits.
    Case::new(P::Exists, S::Absent, O::NoFile),
    Case::new(P::Exists, S::CleanCommitted, O::Ok),
    // No content proof on a dirty/uncommitted tree must refuse — today no dirty
    // gate, so the op edits the dirty bytes and commits.
    Case::pending(
        P::Exists,
        S::CommittedStaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(
        P::Exists,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(
        P::Exists,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(P::Exists, S::NewStaged, O::ConcurrencyError, DIRTY_GATE),
    Case::pending(P::Exists, S::IntentToAdd, O::ConcurrencyError, DIRTY_GATE),
    Case::pending(
        P::Exists,
        S::NewStagedUnstaged,
        O::ConcurrencyError,
        DIRTY_GATE,
    ),
    Case::pending(P::Exists, S::Untracked, O::ConcurrencyError, DIRTY_GATE),
    // ── ExpectBlob(HEAD) — defined iff committed ─────────────────────────────
    Case::new(P::Head, S::CleanCommitted, O::Ok),
    // HEAD token matches HEAD-tree, so it passes vs HEAD and edits dirty bytes.
    Case::pending(
        P::Head,
        S::CommittedStaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    Case::pending(
        P::Head,
        S::CommittedUnstaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    Case::pending(
        P::Head,
        S::CommittedStagedUnstaged,
        O::ConcurrencyError,
        HEAD_CLOBBER,
    ),
    // ── ExpectBlob(INDEX) — defined iff staged ───────────────────────────────
    // INDEX == workdir (no unstaged) → proving current bytes; today vs HEAD → refuse.
    Case::pending(P::Index, S::CommittedStaged, O::Ok, PRECOND_VS_HEAD),
    Case::pending(P::Index, S::NewStaged, O::Ok, PRECOND_VS_HEAD),
    // INDEX != workdir (unstaged on top) → correctly refuses today.
    Case::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Case::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ── ExpectBlob(WORKDIR) — proving on-disk bytes; SKIP where == HEAD/INDEX ─
    // All should be OK (you proved current bytes); today checked vs HEAD.
    Case::pending(P::Workdir, S::CommittedUnstaged, O::Ok, PRECOND_VS_HEAD),
    Case::pending(
        P::Workdir,
        S::CommittedStagedUnstaged,
        O::Ok,
        PRECOND_VS_HEAD,
    ),
    Case::pending(P::Workdir, S::IntentToAdd, O::Ok, PRECOND_VS_HEAD),
    Case::pending(P::Workdir, S::NewStagedUnstaged, O::Ok, PRECOND_VS_HEAD),
    Case::pending(P::Workdir, S::Untracked, O::Ok, PRECOND_VS_HEAD),
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

// Burndown reasons (shared by the cells that pin each defect).
const DIRTY_GATE: &str = "WSS: no dirty gate for in-place edit (edits uncommitted bytes)";
const HEAD_CLOBBER: &str =
    "WSS: dirty-tree clobber — HEAD token passes vs HEAD, edit applies to dirty bytes";
const PRECOND_VS_HEAD: &str = "WSS: precondition checked vs HEAD, not the working tree";

/// Op-specific one-offs (outside the precondition × state grid): a SEARCH that
/// matches nothing is an `OpError` — the op refuses, the working tree untouched.
pub fn extra_trials(backend: Backend) -> Vec<Trial> {
    vec![cell_trial(
        format!(
            "{}::{}::edit_note::one-off::search-not-found::OpError",
            ToolsWorld::LABEL,
            backend.code()
        ),
        None,
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
