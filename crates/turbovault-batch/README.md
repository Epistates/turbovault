# turbovault-batch

[![Crates.io](https://img.shields.io/crates/v/turbovault-batch.svg)](https://crates.io/crates/turbovault-batch)
[![Docs.rs](https://docs.rs/turbovault-batch/badge.svg)](https://docs.rs/turbovault-batch)
[![License](https://img.shields.io/crates/l/turbovault-batch.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Validated, fail-fast batches of Obsidian vault file operations.

The crate groups several `VaultManager` operations behind one request. It
validates the batch for overlapping paths, executes operations sequentially,
stops at the first failure, and returns a detailed `BatchResult`.

## Important execution semantics

A batch is **not** an all-or-nothing transaction. Each individual file write
uses TurboVault's atomic replacement path, but operations completed before a
later failure remain applied. Callers must inspect:

- `success` — whether every operation completed;
- `executed` and `failed_at` — how far execution progressed;
- `changes` — successful operations already applied;
- `records` and `errors` — per-operation results and the failure reason.

If an all-or-nothing workflow is required, do not rely on `BatchExecutor` as a
transaction boundary. Use external version control or wait for a transactional
write backend.

## Supported operations

- `CreateNote { path, content }`
- `WriteNote { path, content }`
- `DeleteNote { path }`
- `MoveNote { from, to }`
- `UpdateLinks { file, old_target, new_target }`

`MoveNote` moves the file and refreshes TurboVault's graph/cache state. It does
not rewrite wikilinks in other notes. `UpdateLinks` only changes the explicitly
named file.

## Example

```no_run
use std::sync::Arc;
use turbovault_batch::{BatchExecutor, BatchOperation};
use turbovault_core::ServerConfig;
use turbovault_vault::VaultManager;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let manager = Arc::new(VaultManager::new(ServerConfig::default())?);
    let executor = BatchExecutor::from_manager(manager);

    let result = executor
        .execute(vec![
            BatchOperation::CreateNote {
                path: "notes/one.md".into(),
                content: "# One".into(),
            },
            BatchOperation::CreateNote {
                path: "notes/two.md".into(),
                content: "# Two".into(),
            },
        ])
        .await?;

    if !result.success {
        eprintln!(
            "batch stopped at {:?}; prior changes: {:?}",
            result.failed_at, result.changes
        );
    }

    Ok(())
}
```

## Validation

Empty batches are rejected. Operations whose affected paths overlap are also
rejected before execution. Validation prevents conflicts inside a single
batch, but it is not cross-process locking or optimistic concurrency control.
