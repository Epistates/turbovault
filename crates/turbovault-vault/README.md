# turbovault-vault

[![Crates.io](https://img.shields.io/crates/v/turbovault-vault.svg)](https://crates.io/crates/turbovault-vault)
[![Docs.rs](https://docs.rs/turbovault-vault/badge.svg)](https://docs.rs/turbovault-vault)
[![License](https://img.shields.io/crates/l/turbovault-vault.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

`VaultManager`, the heart of [TurboVault](https://github.com/epistates/turbovault): a parsed note cache, the link graph, the write path, and the freshness gate that keeps them true to the files on disk.

## What it does

- **Reads** come from a cache of parsed notes (`VaultFile`s shared as `Arc`), filled at startup by parsing the vault in parallel.
- **Writes** all go through `apply_changes`: a `ChangePlan` is checked against the vault's `allowed_operations` and protected folders (`.obsidian`, `.git`, `.turbovault`, ...), then handed to the vault's write substrate:
  - **Direct**: temp file, fsync and atomic rename per file, held to each file's precondition, under a cross-process lock.
  - **Git** (via turbovault-git): one commit per plan, all or nothing, with compare-and-swap on the branch.
- **The freshness gate** (`ensure_fresh`) compares a `(size, mtime)` scan against what each cache entry recorded and applies the difference, so edits made in Obsidian, by a sync client or by `git pull` reach the cache, the graph and every listener. It isn't a file watcher and can't miss an event; it's debounced so a burst of calls costs one scan.
- **Paths** are resolved inside the vault, symlinks included; anything outside it, or in a protected folder, is refused.
- **`EditEngine`** applies SEARCH/REPLACE blocks with exact, whitespace-insensitive, indentation-preserving and bounded fuzzy matching.

## Example

```rust
use std::path::Path;
use turbovault_core::{ChangePlan, Precondition, ServerConfig, VaultConfig};
use turbovault_vault::VaultManager;

#[tokio::main]
async fn main() -> turbovault_core::Result<()> {
    let vault = VaultConfig::builder("notes", "/path/to/vault").as_default().build()?;
    let manager = VaultManager::new(ServerConfig::with_vaults(vec![vault]))?;
    manager.initialize().await?;

    // Create a note only if it doesn't exist yet.
    let content = "# Idea\n";
    manager
        .write_file(Path::new("inbox/idea.md"), content, Precondition::ExpectAbsent, "add idea")
        .await?;

    // A plan applies as a unit (one commit on a Git-backed vault), and each
    // change is held to the content it expects, here the note just written.
    let token = manager.hash_bytes(content.as_bytes())?;
    let plan = ChangePlan::new("archive the idea").rename("inbox/idea.md", "archive/idea.md", token);
    manager.apply_changes(&plan).await?;

    println!("{} note(s) link to it", manager.get_backlinks(Path::new("archive/idea.md")).await?.len());
    Ok(())
}
```

Anything that serves derived state (search, the graph, statistics) should call `ensure_fresh` first; the manager's own graph queries already do.

## License

MIT
