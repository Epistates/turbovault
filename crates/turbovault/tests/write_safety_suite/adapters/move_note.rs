//! `move_note` adapter — **dual-path** (design doc §4: source `ExpectExists`,
//! destination `ExpectAbsent` = the clobber protection). Two precondition axes.
//!
//! `invoke` drives the aspirational `move_note` op on the tools-layer surface
//! with BOTH a source and a destination [`Precondition`] — the dual-path move
//! (9n6) the pre-relayering tool surface couldn't express. The tool layer does not
//! take a destination precondition yet (cutover: qae.9.1), so the dest sweep and
//! the source-side dirty/precond-vs-workdir cells stay `pending` on the burndown
//! (9n6/nbl.8); the clean source cells run live.
//!
//! The matrix's two row groups are two independent 1-D sweeps:
//! - **source** sweep: vary the source state × source precondition, destination
//!   held ABSENT with `to = ExpectAbsent`. Same shape as delete/edit's in-place
//!   rows (the source is the removed target).
//! - **dest** sweep: vary the destination state × destination precondition,
//!   source held present with `from = ExpectExists`. The destination is the write
//!   target — `ExpectAbsent` on it is the clobber protection.

use libtest_mimic::Trial;

use super::{cell_trial, present_state};
use crate::harness::backend::{Backend, Layer, MSG, ManagerWorld, ToolsWorld, observe};
use crate::harness::outcome::{ObservedError, Outcome as O};
use crate::harness::precondition::{Precondition, PreconditionKind as P};
use crate::harness::state::GitState as S;
use turbovault_tools::FileTools;

const SRC: &str = "from.md";
const DEST: &str = "to.md";

/// Which layer performs the dual-path move. Both drive the SAME
/// `move_file(from, to, src_pc, dest_pc, msg)` signature: the tools arm through
/// `FileTools` (a thin delegator), the manager arm on `VaultManager` directly.
#[derive(Clone, Copy)]
enum MoveVia {
    Tools,
    Manager,
}

/// One cell of a single move sweep: the varied path's state × precondition →
/// desired outcome. `pending` marks a cell whose desired behavior isn't wired
/// yet (the burndown) — an `ignored` trial, exactly like [`super::Case`]. `only`
/// scopes a cell to one backend (the git/direct split for the `e---u` state,
/// where git is a burndown gap but direct already behaves correctly).
#[derive(Clone, Copy)]
struct Cell {
    precond: P,
    state: S,
    expected: O,
    pending: bool,
    only: Option<Backend>,
}

impl Cell {
    const fn new(precond: P, state: S, expected: O) -> Self {
        Self {
            precond,
            state,
            expected,
            pending: false,
            only: None,
        }
    }

    const fn pending(precond: P, state: S, expected: O) -> Self {
        Self {
            precond,
            state,
            expected,
            pending: true,
            only: None,
        }
    }

    const fn on(mut self, backend: Backend) -> Self {
        self.only = Some(backend);
        self
    }
}

// ── SOURCE sweep — destination held absent (`to = ExpectAbsent`) ─────────────
// Same shape as delete/edit's in-place rows: the source must exist and match.
const SRC_CASES: &[Cell] = &[
    // ExpectExists (in-place default, dirty-gated) — the source is the removed
    // target; NoFile-on-absent must precede the precondition check.
    Cell::pending(P::Exists, S::Absent, O::NoFile),
    Cell::new(P::Exists, S::CleanCommitted, O::Ok),
    Cell::new(P::Exists, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::NewStaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::IntentToAdd, O::ConcurrencyError),
    Cell::new(P::Exists, S::NewStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Cell::pending(P::Exists, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
    // ExpectBlob(HEAD) — defined iff committed
    Cell::new(P::Head, S::CleanCommitted, O::Ok),
    Cell::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(INDEX) — defined iff staged
    Cell::pending(P::Index, S::CommittedStaged, O::Ok),
    Cell::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::pending(P::Index, S::NewStaged, O::Ok),
    Cell::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(WORKDIR) — SKIP where == HEAD/INDEX
    Cell::pending(P::Workdir, S::CommittedUnstaged, O::Ok),
    Cell::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Cell::pending(P::Workdir, S::IntentToAdd, O::Ok),
    Cell::pending(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Cell::pending(P::Workdir, S::Untracked, O::Ok).on(Backend::Git),
    Cell::new(P::Workdir, S::Untracked, O::Ok).on(Backend::Direct),
    // ExpectBlob(WRONG) → refuse everywhere; NoFile on absent
    Cell::pending(P::Wrong, S::Absent, O::NoFile),
    Cell::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Cell::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Cell::new(P::Wrong, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
];

// ── DEST sweep — source held clean committed (`from = ExpectExists`) ─────────
// The destination is the write target; `ExpectAbsent` on it is the clobber guard.
// The destination precondition is not expressible against today's `move_file`
// (it hardcodes `expect_absent(to)`), so every cell whose desired outcome
// diverges from that hardcoded behavior is `pending` on the dual-path move
// burndown (turbovault-9n6). Cells that happen to coincide with `expect_absent`
// stay active.
const DEST_CASES: &[Cell] = &[
    // Blind → overwrite the destination unconditionally.
    Cell::new(P::Blind, S::Absent, O::Ok),
    Cell::new(P::Blind, S::CleanCommitted, O::Ok),
    Cell::pending(P::Blind, S::CommittedStaged, O::Ok),
    Cell::pending(P::Blind, S::CommittedUnstaged, O::Ok),
    Cell::pending(P::Blind, S::CommittedStagedUnstaged, O::Ok),
    Cell::pending(P::Blind, S::NewStaged, O::Ok),
    Cell::pending(P::Blind, S::IntentToAdd, O::Ok),
    Cell::pending(P::Blind, S::NewStagedUnstaged, O::Ok),
    Cell::pending(P::Blind, S::Untracked, O::Ok).on(Backend::Git),
    Cell::new(P::Blind, S::Untracked, O::Ok).on(Backend::Direct),
    // ExpectAbsent (clobber protection) → OK on absent dest, else refuse.
    Cell::new(P::Absent, S::Absent, O::Ok),
    Cell::new(P::Absent, S::CleanCommitted, O::ConcurrencyError),
    Cell::new(P::Absent, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::NewStaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::IntentToAdd, O::ConcurrencyError),
    Cell::new(P::Absent, S::NewStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Absent, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Cell::new(P::Absent, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
    // ExpectBlob(HEAD) on the dest — defined iff dest committed
    Cell::new(P::Head, S::CleanCommitted, O::Ok),
    Cell::new(P::Head, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Head, S::CommittedStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(INDEX) on the dest — defined iff dest staged
    Cell::pending(P::Index, S::CommittedStaged, O::Ok),
    Cell::new(P::Index, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::pending(P::Index, S::NewStaged, O::Ok),
    Cell::new(P::Index, S::NewStagedUnstaged, O::ConcurrencyError),
    // ExpectBlob(WORKDIR) on the dest — SKIP where == HEAD/INDEX
    Cell::pending(P::Workdir, S::CommittedUnstaged, O::Ok),
    Cell::pending(P::Workdir, S::CommittedStagedUnstaged, O::Ok),
    Cell::pending(P::Workdir, S::IntentToAdd, O::Ok),
    Cell::pending(P::Workdir, S::NewStagedUnstaged, O::Ok),
    Cell::pending(P::Workdir, S::Untracked, O::Ok).on(Backend::Git),
    Cell::new(P::Workdir, S::Untracked, O::Ok).on(Backend::Direct),
    // ExpectBlob(WRONG) on the dest → refuse everywhere (incl. absent)
    Cell::new(P::Wrong, S::Absent, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CleanCommitted, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::CommittedStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::NewStaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::IntentToAdd, O::ConcurrencyError),
    Cell::new(P::Wrong, S::NewStagedUnstaged, O::ConcurrencyError),
    Cell::new(P::Wrong, S::Untracked, O::ConcurrencyError).on(Backend::Git),
    Cell::new(P::Wrong, S::Untracked, O::ConcurrencyError).on(Backend::Direct),
];

/// Tools-layer dual-path move trials (the exemplar arm).
pub fn trials(backend: Backend) -> Vec<Trial> {
    build_trials::<ToolsWorld>(backend, MoveVia::Tools)
}

/// Manager-layer dual-path move trials (qae.9.2): the SAME src/dest sweeps run
/// against `VaultManager::move_file` directly. Reuses `SRC_CASES`/`DEST_CASES`.
pub fn manager_trials(backend: Backend) -> Vec<Trial> {
    build_trials::<ManagerWorld>(backend, MoveVia::Manager)
}

fn build_trials<W: Layer + 'static>(backend: Backend, via: MoveVia) -> Vec<Trial> {
    let mut out = Vec::new();
    for &c in SRC_CASES {
        if !backend.supports_state(c.state) || c.only.is_some_and(|b| b != backend) {
            continue;
        }
        let name = format!(
            "{}::{}::move_note::src::{}::{}::{:?}",
            W::LABEL,
            backend.code(),
            c.precond.code(),
            c.state.code(),
            c.expected
        );
        out.push(cell_trial(name, c.pending, move || {
            run_src::<W>(c, backend, via)
        }));
    }
    for &c in DEST_CASES {
        if !backend.supports_state(c.state) || c.only.is_some_and(|b| b != backend) {
            continue;
        }
        let name = format!(
            "{}::{}::move_note::dest::{}::{}::{:?}",
            W::LABEL,
            backend.code(),
            c.precond.code(),
            c.state.code(),
            c.expected
        );
        out.push(cell_trial(name, c.pending, move || {
            run_dest::<W>(c, backend, via)
        }));
    }
    out
}

/// Source sweep: vary the source, hold the destination absent.
async fn run_src<W: Layer>(c: Cell, backend: Backend, via: MoveVia) -> Result<(), String> {
    let target = W::new(backend);
    let Some(src_oids) = target.vault().build_state(SRC, c.state) else {
        return Err(format!(
            "source state {} unsupported on {}",
            c.state.code(),
            backend.code()
        ));
    };
    let _ = target.vault().build_state(DEST, S::Absent);
    let Some(from_pc) = c.precond.resolve(&src_oids) else {
        return Err(format!(
            "unexpected N/A: source {} token undefined in state {}",
            c.precond.code(),
            c.state.code()
        ));
    };
    run_move(
        c.expected,
        &target,
        from_pc,
        Precondition::ExpectAbsent,
        via,
    )
    .await
}

/// Dest sweep: vary the destination, hold the source present.
async fn run_dest<W: Layer>(c: Cell, backend: Backend, via: MoveVia) -> Result<(), String> {
    let target = W::new(backend);
    let _ = target.vault().build_state(SRC, present_state(backend));
    let Some(dest_oids) = target.vault().build_state(DEST, c.state) else {
        return Err(format!(
            "dest state {} unsupported on {}",
            c.state.code(),
            backend.code()
        ));
    };
    let Some(to_pc) = c.precond.resolve(&dest_oids) else {
        return Err(format!(
            "unexpected N/A: dest {} token undefined in state {}",
            c.precond.code(),
            c.state.code()
        ));
    };
    run_move(c.expected, &target, Precondition::ExpectExists, to_pc, via).await
}

/// Invoke the dual-path move (tools wrapper or manager directly, per `via`) and
/// assert its outcome: an OK leaves the source gone and the destination present;
/// a refusal leaves BOTH paths byte-for-byte intact (no partial move, no dest
/// clobber).
async fn run_move<W: Layer>(
    expected: O,
    target: &W,
    from_pc: Precondition,
    to_pc: Precondition,
    via: MoveVia,
) -> Result<(), String> {
    let before_src = target.vault().read(SRC);
    let before_dest = target.vault().read(DEST);
    let mgr = target.vault().manager().clone();
    let res = match via {
        MoveVia::Tools => {
            FileTools::new(mgr)
                .move_file(SRC, DEST, from_pc, to_pc, MSG)
                .await
        }
        MoveVia::Manager => {
            mgr.move_file(
                std::path::Path::new(SRC),
                std::path::Path::new(DEST),
                from_pc,
                to_pc,
                MSG,
            )
            .await
        }
    };
    let obs = observe(res, target.vault().read(SRC));
    let after_src = target.vault().read(SRC);
    let after_dest = target.vault().read(DEST);

    match expected {
        O::Ok => {
            if !obs.succeeded {
                return Err(format!("expected Ok move, got failure {:?}", obs.error));
            }
            if after_src.is_some() {
                return Err(format!("OK move: source still present: {after_src:?}"));
            }
            if after_dest.is_none() {
                return Err("OK move: destination missing after move".into());
            }
        }
        O::ConcurrencyError => {
            if obs.succeeded {
                return Err("expected ConcurrencyError, move SUCCEEDED (a clobber/defect)".into());
            }
            if obs.error != Some(ObservedError::Concurrency) {
                return Err(format!(
                    "expected a concurrency refusal, got {:?}",
                    obs.error
                ));
            }
            if after_src != before_src {
                return Err("ConcurrencyError move: source changed (must be intact)".into());
            }
            if after_dest != before_dest {
                return Err("ConcurrencyError move: destination changed (a clobber)".into());
            }
        }
        O::NoFile => {
            if obs.succeeded {
                return Err("expected NoFile, move SUCCEEDED".into());
            }
            if obs.error != Some(ObservedError::NotFound) {
                return Err(format!("expected a not-found refusal, got {:?}", obs.error));
            }
            if after_src != before_src || after_dest != before_dest {
                return Err("NoFile move: a path changed".into());
            }
        }
        O::OpError => return Err("move_note has no OpError cells".into()),
    }
    Ok(())
}
