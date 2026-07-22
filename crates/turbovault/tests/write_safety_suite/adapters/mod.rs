//! Per-operation adapters + the shared matrix-cell / trial machinery
//! (design doc §7).
//!
//! An op's shared data — its `Case` table + `ok_effect` — lives in its module,
//! layer-agnostic. The **invoker** is `impl SinglePathOp<W>`, one per
//! `(op, layer)`: it binds those cases to a layer-specific invocation. An op that
//! doesn't map to a layer simply has no invoker for that layer's World (e.g. the
//! compute ops have no `SinglePathOp<ManagerWorld>` impl — qae.9.2).
//!
//! Dual-path (`move_note`) and multi-op (`batch_execute`) adapters build their
//! own trials with [`cell_trial`]. Each matrix cell is a `libtest-mimic` trial →
//! its own named `cargo test` entry, prefixed `<layer>::<backend>::…`. `pending`
//! cells become **ignored** trials (the burndown).

pub mod batch_execute;
pub mod create_from_template;
pub mod delete_note;
pub mod edit_note;
pub mod manage_tags;
pub mod move_note;
pub mod update_frontmatter;
pub mod write_note;

use libtest_mimic::{Failed, Trial};

use crate::harness::backend::{Backend, Layer};
use crate::harness::outcome::{Observed, Outcome};
use crate::harness::precondition::{Precondition, PreconditionKind};
use crate::harness::state::GitState;

/// The relative path every single-path op targets.
pub const REL: &str = "note.md";

/// A "file exists with content" state the backend can build: the git arm uses a
/// clean commit; Direct has only the untracked-on-disk notion of "present".
pub fn present_state(backend: Backend) -> GitState {
    match backend {
        Backend::Git => GitState::CleanCommitted,
        Backend::Direct => GitState::Untracked,
    }
}

/// One single-path matrix cell: a precondition selector × a working-tree state →
/// the desired outcome. `pending` marks a cell whose desired behavior isn't
/// implemented yet.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub precondition: PreconditionKind,
    pub state: GitState,
    pub expected: Outcome,
    pub pending: Option<&'static str>,
}

impl Case {
    /// An active cell (its desired behavior must already hold).
    pub const fn new(precondition: PreconditionKind, state: GitState, expected: Outcome) -> Self {
        Self {
            precondition,
            state,
            expected,
            pending: None,
        }
    }

    /// A cell whose desired behavior is not yet implemented (burndown item).
    pub const fn pending(
        precondition: PreconditionKind,
        state: GitState,
        expected: Outcome,
        reason: &'static str,
    ) -> Self {
        Self {
            precondition,
            state,
            expected,
            pending: Some(reason),
        }
    }
}

/// A single-target op's invoker at layer `W` — one impl per `(op, layer)`. The
/// op-specific compute (edit string, frontmatter map, tags, template fields) and
/// the result normalization (`observe`) live in `invoke`, so the harness stays
/// agnostic to the op. Native `async fn` in trait + generic dispatch.
pub trait SinglePathOp<W: Layer> {
    fn name(&self) -> &'static str;

    fn cases(&self) -> &'static [Case];

    /// Perform the op against `world` on [`REL`] with `precondition`, returning
    /// the normalized observation.
    async fn invoke(&self, world: &W, rel: &str, precondition: Precondition) -> Observed;

    /// OK-effect check, run only when a cell expects [`Outcome::Ok`]: assert the
    /// op's *specific* successful effect (content == X, target deleted, …).
    /// Default: a successful op is enough.
    fn ok_effect(&self, _observed: &Observed) -> Result<(), String> {
        Ok(())
    }
}

/// Build a trial that runs one async cell on its own current-thread runtime.
/// `pending.is_some()` → the trial is marked `ignored` (the burndown).
pub fn cell_trial<F, Fut>(name: String, pending: Option<&'static str>, run: F) -> Trial
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), String>>,
{
    let mut trial = Trial::test(name, move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");
        rt.block_on(run()).map_err(Failed::from)
    });
    if pending.is_some() {
        trial = trial.with_ignored_flag(true);
    }
    trial
}

/// One named trial per case of a single-path op's invoker, on `backend`. Cells
/// whose state the backend can't represent are filtered out. Trial name is
/// `<layer>::<backend>::<op>::<PRECONDITION>::<state-code>::<expected>`.
pub fn single_path_trials<W, Op>(op: Op, backend: Backend) -> Vec<Trial>
where
    W: Layer + 'static,
    Op: SinglePathOp<W> + Copy + Send + 'static,
{
    op.cases()
        .iter()
        .filter(|case| backend.supports_state(case.state))
        .map(|&case| {
            let name = format!(
                "{}::{}::{}::{}::{}::{:?}",
                W::LABEL,
                backend.code(),
                op.name(),
                case.precondition.code(),
                case.state.code(),
                case.expected
            );
            cell_trial(name, case.pending, move || {
                run_single_cell::<W, Op>(op, case, backend)
            })
        })
        .collect()
}

async fn run_single_cell<W, Op>(op: Op, case: Case, backend: Backend) -> Result<(), String>
where
    W: Layer,
    Op: SinglePathOp<W>,
{
    let world = W::new(backend);
    let Some(oids) = world.vault().build_state(REL, case.state) else {
        return Err(format!(
            "state {} unsupported on {} — should have been filtered",
            case.state.code(),
            backend.code()
        ));
    };
    let Some(pc) = case.precondition.resolve(&oids) else {
        return Err(format!(
            "unexpected N/A: {} token undefined in state {} — remove this cell",
            case.precondition.code(),
            case.state.code()
        ));
    };
    let before = world.vault().read(REL);
    let observed = op.invoke(&world, REL, pc).await;
    case.expected.check(&observed, before.as_deref())?;
    if case.expected == Outcome::Ok {
        op.ok_effect(&observed)?;
    }
    Ok(())
}
