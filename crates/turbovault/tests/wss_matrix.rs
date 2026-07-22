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
use harness::backend::{Backend, ToolsWorld};
use libtest_mimic::{Arguments, Trial};

fn main() {
    let args = Arguments::from_args();

    let mut tests: Vec<Trial> = Vec::new();
    // Every op runs against both write backends (Direct's git-only states are
    // filtered out by the runner). Trial names are backend-prefixed.
    for backend in [Backend::Git, Backend::Direct] {
        // Single-path ops (the SinglePathOp mold generates their full grids).
        tests.extend(single_path_trials::<ToolsWorld, _>(adapters::write_note::WriteNote, backend));
        tests.extend(single_path_trials::<ToolsWorld, _>(adapters::edit_note::EditNote, backend));
        tests.extend(single_path_trials::<ToolsWorld, _>(adapters::delete_note::DeleteNote, backend));
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::update_frontmatter::UpdateFrontmatter,
            backend,
        ));
        tests.extend(single_path_trials::<ToolsWorld, _>(adapters::manage_tags::ManageTags, backend));
        tests.extend(single_path_trials::<ToolsWorld, _>(
            adapters::create_from_template::CreateFromTemplate,
            backend,
        ));
        // Op-specific one-offs + the odd shapes (dual-path, multi-op).
        tests.extend(adapters::edit_note::extra_trials(backend));
        tests.extend(adapters::move_note::trials(backend));
        tests.extend(adapters::batch_execute::trials(backend));
    }

    libtest_mimic::run(&args, tests).exit();
}
