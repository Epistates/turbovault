//! Portable op-machinery for the WSS matrix: the `Case` cell, the op trait pair,
//! and the trial/runner glue that turns a `(op, layer)` pair into libtest-mimic
//! trials (design doc §7).
//!
//! An op's shared data — its `Case` table + identity + `ok_effect` — lives in its
//! [`OpAdapterMeta`] impl, ONE per op, layer-agnostic. The per-layer **invoker** is
//! `impl Op<W>`, one per `(op, layer)`: it binds those cases to a layer-specific
//! invocation. An op that doesn't map to a layer simply has no `Op<W>` impl for
//! that layer's World (e.g. the compute ops have no `Op<ManagerWorld>` — qae.9.2).
//!
//! SEAM (turbovault-nbl.13): this module is the PORTABLE CORE — it knows nothing
//! about turbovault's concrete substrates, only the abstract [`Layer`]/`Vault` it
//! is generic over (defined in `backend.rs`, the turbovault glue). A later crate
//! extraction lifts this module + the outcome/precondition/state-signature
//! primitives and puts `backend.rs`'s glue behind a trait — a move, not a rewrite.
//!
//! Each matrix cell is a `libtest-mimic` trial → its own named `cargo test` entry,
//! prefixed `<layer>::<backend>::…`. `pending` cells become **ignored** trials (the
//! burndown). The dual-path `move_note` adapter builds its two roles as ordinary
//! `Op` impls over shared tables, same as every other op.

use libtest_mimic::{Failed, Trial};

use super::backend::{Backend, Layer};
use super::outcome::{Observed, Outcome};
use super::precondition::{Precondition, PreconditionKind};
use super::state::GitState;

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
///
/// `only` scopes a cell to a single backend, via [`Case::on`]. There are exactly two
/// legitimate reasons to split a cell, and they are not interchangeable:
///
/// 1. **Same requirement, one backend lags.** Both copies carry the SAME `expected`
///    and differ only in the `pending` flag, so the working backend is guarded
///    against regression while the lagging one stays out of scope.
/// 2. **Different requirement, because the backends genuinely disagree.** The copies
///    carry DIFFERENT `expected`. This is legal ONLY when the source-of-truth CSV
///    says so — it has a `backend` column, and `just wss-audit` checks both arms
///    against it. That is the guard that keeps this from becoming a licence to make
///    a red cell green: a divergence cannot be hand-written here, it has to be
///    written into a reviewed spec first.
///
/// The `e---u`/Untracked state is case 2, and the reason is that the state CODE means
/// two different things: on git it is a dirty untracked working tree, so an in-place
/// op refuses; on direct it is merely "the file exists" (`present_state(Direct)`), so
/// `ExpectExists` is SATISFIED and the op proceeds. Direct is git-blind — its version
/// token is the sha256 of the bytes, and staged/committed are invisible to it — so
/// there is no out-of-band change to lose and nothing to refuse.
///
/// Do NOT "unify" these two arms to one `expected`. They disagree because the spec
/// disagrees, not because one backend is behind.
#[derive(Clone, Copy, Debug)]
pub struct Case {
    pub precondition: PreconditionKind,
    pub state: GitState,
    pub expected: Outcome,
    pub pending: bool,
    pub only: Option<Backend>,
}

impl Case {
    /// An active cell (its desired behavior must already hold).
    pub const fn new(precondition: PreconditionKind, state: GitState, expected: Outcome) -> Self {
        Self {
            precondition,
            state,
            expected,
            pending: false,
            only: None,
        }
    }

    /// A cell whose desired behavior is not yet implemented (burndown item). A
    /// one-line mirror of [`Case::new`] — the cell's precondition/state/expected
    /// identify it; the "why" is derived from the trial name by
    /// `scripts/wss-report.py`, not stored per-cell.
    pub const fn pending(
        precondition: PreconditionKind,
        state: GitState,
        expected: Outcome,
    ) -> Self {
        Self {
            precondition,
            state,
            expected,
            pending: true,
            only: None,
        }
    }

    /// Scope this cell to a single backend (the git/direct split for `e---u`).
    pub const fn on(mut self, backend: Backend) -> Self {
        self.only = Some(backend);
        self
    }
}

/// Op-level, WORLD-AGNOSTIC surface for an op: its identity, its shared `Case`
/// table, and its OK-effect check. Implemented ONCE per op (not per layer), so the
/// per-layer [`Op`] invokers carry only `invoke`.
///
/// `ok_effect` is REQUIRED — no default. A default `Ok(())` would silently pass an
/// `Ok` cell whose specific effect (content == X, target deleted, …) was never
/// checked: a false pass. Every op states its effect explicitly.
pub trait OpAdapterMeta {
    fn name(&self) -> &'static str;

    fn cases(&self) -> &'static [Case];

    /// OK-effect check, run only when a cell expects [`Outcome::Ok`]: assert the
    /// op's *specific* successful effect. Required (see the trait note above).
    fn ok_effect(&self, observed: &Observed) -> Result<(), String>;
}

/// A single-target op's invoker at layer `W` — one impl per `(op, layer)`. The
/// op-level `identity/cases/ok_effect` come from the [`OpAdapterMeta`] supertrait, so
/// a world's impl carries only `invoke`: the op-specific compute (edit string,
/// frontmatter map, tags, template fields), the layer's call, and `observe`. Native
/// `async fn` in trait + generic dispatch.
pub trait Op<W: Layer>: OpAdapterMeta {
    /// Perform the op against `world` on `rel` with `precondition`, returning the
    /// normalized observation.
    async fn invoke(&self, world: &W, rel: &str, precondition: Precondition) -> Observed;

    /// The OK-effect check the runner uses. Defaults to the op-level
    /// [`OpAdapterMeta::ok_effect`]; a world overrides this ONLY when its successful
    /// effect differs from the op's shared one.
    fn ok_effect(&self, observed: &Observed) -> Result<(), String> {
        OpAdapterMeta::ok_effect(self, observed)
    }
}

/// The OK-effect check shared by every op whose success is "the target now
/// contains this marker" (edit / `update_frontmatter` / `manage_tags` / template).
/// Their checks were four byte-identical copies differing only in the marker and
/// the message (dupehound's largest cluster in the suite: 42 duplicate lines).
///
/// Note what this deliberately does NOT do: it asserts the write LANDED, not that
/// the result is well-formed. Content-correctness is out of WSS scope — the suite
/// pins clobber-safety, i.e. that a write refuses-or-proceeds without silently
/// losing an out-of-band change.
pub fn content_contains(observed: &Observed, marker: &str) -> Result<(), String> {
    if observed
        .after_content
        .as_deref()
        .is_some_and(|c| c.contains(marker))
    {
        Ok(())
    } else {
        Err(format!(
            "OK effect: expected content containing {marker:?}, got {:?}",
            observed.after_content
        ))
    }
}

/// Build a trial that runs one async cell on its own current-thread runtime.
/// `pending` → the trial is marked `ignored` (the burndown).
pub fn cell_trial<F, Fut>(name: String, pending: bool, run: F) -> Trial
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
    if pending {
        trial = trial.with_ignored_flag(true);
    }
    trial
}

/// One named trial per case of an op's invoker, on `backend`. Cells whose state the
/// backend can't represent are filtered out. Trial name is
/// `<layer>::<backend>::<op>::<PRECONDITION>::<state-code>::<expected>`.
pub fn op_trials<W, A>(op: A, backend: Backend) -> Vec<Trial>
where
    W: Layer + 'static,
    A: Op<W> + Copy + Send + 'static,
{
    op.cases()
        .iter()
        .filter(|case| backend.supports_state(case.state))
        .filter(|case| case.only.is_none_or(|b| b == backend))
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
                run_single_cell::<W, A>(op, case, backend)
            })
        })
        .collect()
}

async fn run_single_cell<W, A>(op: A, case: Case, backend: Backend) -> Result<(), String>
where
    W: Layer,
    A: Op<W>,
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
        // Subtrait form: a world's `Op<W>::ok_effect` override wins; absent one it
        // delegates to `OpAdapterMeta::ok_effect`. Disambiguated because both traits
        // expose `ok_effect` (E0034 otherwise).
        <A as Op<W>>::ok_effect(&op, &observed)?;
    }
    Ok(())
}
