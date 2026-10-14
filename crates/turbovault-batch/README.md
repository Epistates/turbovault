# turbovault-batch

[![Crates.io](https://img.shields.io/crates/v/turbovault-batch.svg)](https://crates.io/crates/turbovault-batch)
[![Docs.rs](https://docs.rs/turbovault-batch/badge.svg)](https://docs.rs/turbovault-batch)
[![License](https://img.shields.io/crates/l/turbovault-batch.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

The operation and result types behind [TurboVault](https://github.com/epistates/turbovault)'s `batch_execute` tool, and the checks that keep one batch from conflicting with itself. This crate executes nothing: `turbovault-tools` (`BatchTools`) turns a batch into one `ChangePlan` and applies it through `VaultManager`.

## Operations

`BatchOperation` is tagged by `type`:

| Operation | Fields |
|---|---|
| `CreateNote` | `path`, `content`, `force` |
| `WriteNote` | `path`, `content`, `expected_hash` |
| `EditNote` | `path`, `edits` (SEARCH/REPLACE blocks), `expected_hash` |
| `DeleteNote` | `path`, `expected_hash`, `on_backlinks` |
| `MoveNote` | `from`, `to`, `expected_hash`, `dest_expected_hash`, `update_backlinks` |
| `UpdateLinks` | `file`, `old_target`, `new_target`, `expected_hash` |
| `UpdateFrontmatter` | `path`, `frontmatter`, `merge`, `expected_hash` |
| `ManageTags` | `path`, `operation`, `tags`, `expected_hash` |
| `CreateFromTemplate` | `template_id`, `path`, `fields`, `force` |

`expected_hash` holds an operation to the note's content when the batch was planned, so a note changed in the meantime fails the batch instead of being overwritten.

## Validation

`affected_files` names the paths an operation touches and `conflicts_with` tells whether two operations touch the same one, so a batch that, say, writes and deletes the same note is refused before anything runs.

## What a batch guarantees

- On a Git-backed vault the whole batch is one commit: every operation lands, or none does.
- On a direct vault operations apply in order. If one fails, the ones before it stay applied, and `BatchResult` reports exactly how far it got (`executed`, `failed_at`, the per-operation `records`).

## Example

```rust
use turbovault_batch::BatchOperation;

fn main() -> serde_json::Result<()> {
    let batch: Vec<BatchOperation> = serde_json::from_str(r##"[
        {"type": "CreateNote", "path": "notes/one.md", "content": "# One"},
        {"type": "MoveNote", "from": "inbox/two.md", "to": "notes/two.md"}
    ]"##)?;

    assert!(!batch[0].conflicts_with(&batch[1]));
    Ok(())
}
```

## License

MIT
