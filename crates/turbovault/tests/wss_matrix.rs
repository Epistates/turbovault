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
        // Op-specific one-offs + the odd shapes (dual-path, multi-op).
        tests.extend(adapters::edit_note::extra_trials(backend));
        tests.extend(adapters::move_note::trials(backend));
        tests.extend(adapters::batch_execute::trials(backend));

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
        tests.extend(adapters::move_note::manager_trials(backend));

        // ── Batch layer (qae.9.3) ────────────────────────────────────────────
        // Per-op isolation: batch-of-one == standalone. Every single-path op
        // expressible as a `BatchOperation` gets a `SinglePathOp<BatchWorld>`
        // invoker reusing its `cases()` table (delete uses BATCH_CASES — batch
        // delete-of-absent is an idempotent OK the standalone op still refuses).
        // The multi-op atomicity scenarios ride `batch_execute::trials` above.
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
    }

    libtest_mimic::run(&args, tests).exit();
}
