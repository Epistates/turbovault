//! WSS write-safety matrix — per-cell named runner (turbovault-nbl.2).
//!
//! `harness = false`: each matrix cell is its own `libtest-mimic` trial, so it
//! shows as a named `cargo test` entry — filterable (`cargo test --test
//! wss_matrix git::write_note::WORKDIR`), individually runnable, and counted one
//! by one in the output. Cells run on both backends (`git::…` / `direct::…`).
//! `pending` cells are `ignored` trials (the burndown): run them with
//! `--ignored`; a pass there is the signal to un-pend.
//!
//! The harness's own unit tests (the meta tests) live in the default-harness
//! `write_safety_suite` target.

#[path = "write_safety_suite/harness/mod.rs"]
mod harness;

#[path = "write_safety_suite/adapters/mod.rs"]
mod adapters;

use adapters::single_path_trials;
use harness::backend::{Backend, BatchWorld, ManagerWorld, ToolsWorld};
use libtest_mimic::{Arguments, Trial};

fn main() {
    let args = Arguments::from_args();

    let mut tests: Vec<Trial> = Vec::new();
    // Every op runs against both write backends (Direct's git-only states are
    // filtered out by the runner). Trial names are backend-prefixed.
    for backend in [Backend::Git, Backend::Direct] {
        // Single-path ops (the SinglePathOp mold generates their full grids).
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::write_note::WriteNote,
            backend,
        ));
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::edit_note::EditNote,
            backend,
        ));
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::delete_note::DeleteNote,
            backend,
        ));
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::update_frontmatter::UpdateFrontmatter,
            backend,
        ));
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::manage_tags::ManageTags,
            backend,
        ));
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::create_from_template::CreateFromTemplate,
            backend,
        ));
        // Op-specific one-offs.
        tests.extend(adapters::edit_note::extra_trials(backend));
        // Dual-path move as two ordinary SinglePathOp ops: a source sweep
        // (vary the source, dest held absent) and a dest sweep (vary the dest,
        // source held present). Same mold, same shared tables, every world.
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::move_note::MoveSrc,
            backend,
        ));
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::move_note::MoveDest,
            backend,
        ));

        // ── Manager layer (qae.9.2) ──────────────────────────────────────────
        // The enforcement/SDK surface directly: the write/edit/delete/move ops
        // with a native `VaultManager` mutator (uf/mt/template have none). Same
        // `cases()` tables as the tools arm — tools are thin delegators to the
        // manager, so per-cell results (and thus pending flags) coincide.
        tests.extend(single_path_trials::<ManagerWorld, _>(
            adapters::write_note::WriteNote,
            backend,
        ));
        tests.extend(single_path_trials::<ManagerWorld, _>(
            adapters::edit_note::EditNote,
            backend,
        ));
        tests.extend(single_path_trials::<ManagerWorld, _>(
            adapters::delete_note::DeleteNote,
            backend,
        ));
        tests.extend(single_path_trials::<ManagerWorld, _>(
            adapters::move_note::MoveSrc,
            backend,
        ));
        tests.extend(single_path_trials::<ManagerWorld, _>(
            adapters::move_note::MoveDest,
            backend,
        ));

        // ── Batch layer (qae.9.3) ────────────────────────────────────────────
        // Per-op isolation: batch-of-one == standalone. Every op gets a
        // `SinglePathOp<BatchWorld>` invoker reusing the op's SHARED `cases()`
        // table — the same cells the Tools/Manager arms run. A batch that
        // diverges from the standalone outcome FAILS that cell (the divergence
        // is the finding); we never fork a per-world table.
        // Multi-op transaction-integrity (atomicity/rollback/collision/empty) is
        // a different axis from per-write clobber-safety — extracted out of WSS
        // scope (turbovault-nbl.17); BatchWorld keeps only this isolation arm.
        tests.extend(single_path_trials::<BatchWorld, _>(
            adapters::write_note::WriteNote,
            backend,
        ));
        tests.extend(single_path_trials::<BatchWorld, _>(
            adapters::edit_note::EditNote,
            backend,
        ));
        tests.extend(single_path_trials::<BatchWorld, _>(
            adapters::delete_note::DeleteNote,
            backend,
        ));
        tests.extend(single_path_trials::<BatchWorld, _>(
            adapters::update_frontmatter::UpdateFrontmatter,
            backend,
        ));
        tests.extend(single_path_trials::<BatchWorld, _>(
            adapters::manage_tags::ManageTags,
            backend,
        ));
        tests.extend(single_path_trials::<BatchWorld, _>(
            adapters::create_from_template::CreateFromTemplate,
            backend,
        ));
        // Dual-path move through the batch surface. The batch invoker uses the
        // API we NEED (a MoveNote carrying src+dest preconditions), so it does
        // not compile until qae.6.4 builds it — the intentional WSS signal.
        tests.extend(single_path_trials::<BatchWorld, _>(
            adapters::move_note::MoveSrc,
            backend,
        ));
        tests.extend(single_path_trials::<BatchWorld, _>(
            adapters::move_note::MoveDest,
            backend,
        ));
    }

    libtest_mimic::run(&args, tests).exit();
}
