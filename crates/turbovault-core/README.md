# turbovault-core

[![Crates.io](https://img.shields.io/crates/v/turbovault-core.svg)](https://crates.io/crates/turbovault-core)
[![Docs.rs](https://docs.rs/turbovault-core/badge.svg)](https://docs.rs/turbovault-core)
[![License](https://img.shields.io/crates/l/turbovault-core.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

The shared foundation of [TurboVault](https://github.com/epistates/turbovault): the data model for notes and links, configuration, errors, multi-vault management, and the small pieces every other crate builds on.

## What's in it

| Module | Contents |
|---|---|
| `models` | `VaultFile`, `Link` and `LinkType`, `Frontmatter`, `Heading`, `Tag`, `TaskItem`, `Callout`, `FileMetadata`, `SourcePosition`, `LineIndex` |
| `error` | `Error` (`#[non_exhaustive]`) and `ErrorCategory`, which says whether an error is not found, invalid input, a conflict, permission denied, unavailable or internal |
| `config` | `ServerConfig`, `VaultConfig` (and its builder), `VaultGitConfig`, `WriteBackend`, `SnapshotRetention` |
| `profiles` | `ConfigProfile`: the seven `--profile` presets |
| `multi_vault` | `MultiVaultManager`: registered vaults and the active one |
| `cache` | `VaultCache`: the vault registry persisted per project, merged under a file lock |
| `vault_roots` | `VaultRootPolicy`: which directories may be registered as a vault at runtime |
| `change_plan` | `ChangePlan`: a set of writes, removals and renames with their preconditions, the unit every write backend applies |
| `atomic_write` | `write_atomic`: temp file, fsync and rename, keeping permissions |
| `validation` | Frontmatter, link and content validators, including the Obsidian 1.9 property rules |
| `okf`, `task_parser`, `utils` | Open Knowledge Format helpers, Tasks-plugin parsing, path and hashing utilities |

## Example

```rust
use turbovault_core::{ConfigProfile, MultiVaultManager, VaultConfig};

#[tokio::main]
async fn main() -> turbovault_core::Result<()> {
    let config = ConfigProfile::Production.create_config();
    let vaults = MultiVaultManager::new(config)?;

    vaults
        .add_vault(VaultConfig::builder("notes", "/path/to/vault").as_default().build()?)
        .await?;

    for vault in vaults.list_vaults().await? {
        println!("{} -> {}", vault.name, vault.path.display());
    }
    Ok(())
}
```

Errors carry a category, so callers can react without matching every variant:

```rust
use turbovault_core::{Error, ErrorCategory};

fn describe(error: &Error) -> &'static str {
    match error.category() {
        ErrorCategory::NotFound => "missing",
        ErrorCategory::Conflict => "changed since it was read",
        ErrorCategory::PermissionDenied => "not allowed",
        _ => "failed",
    }
}
```

`ServerConfig`, `VaultConfig` and the other configuration types are `#[non_exhaustive]`: build them from `Default` (or `ConfigProfile::create_config`) and set fields, or use the builders.

## License

MIT
