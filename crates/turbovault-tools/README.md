# turbovault-tools

[![Crates.io](https://img.shields.io/crates/v/turbovault-tools.svg)](https://crates.io/crates/turbovault-tools)
[![Docs.rs](https://docs.rs/turbovault-tools/badge.svg)](https://docs.rs/turbovault-tools)
[![License](https://img.shields.io/crates/l/turbovault-tools.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

The logic behind [TurboVault](https://github.com/epistates/turbovault)'s MCP tools, usable on its own. Each module works on a `VaultManager` (turbovault-vault); the `turbovault` server wraps them as MCP tools with descriptions, schemas and error mapping.

## Modules

| Module | What it does |
|---|---|
| `search_engine` | Tantivy BM25 search fed from the parsed note cache, with filters by tags and frontmatter. `SearchEngine::for_vault` persists the index under TurboVault's cache directory, so a restart re-indexes only notes that changed. |
| `similarity_engine` | TF-IDF similarity over interned terms, updated in place as notes change; backs `find_similar` and `compare_notes` |
| `duplicate_tools` | Near-duplicate detection from the similarity engine's fingerprints |
| `graph_tools`, `relationship_tools` | Hubs, centrality, dead ends, clusters, cycles, broken links; link suggestions and link strength |
| `file_tools`, `search_tools`, `metadata_tools` | Reading, writing and editing notes, links in and out, frontmatter and tags |
| `batch_tools` | Turns a `turbovault-batch` batch into one `ChangePlan`; link-aware move and delete |
| `wikilink_rewriter` | Rewrites wikilinks, embeds, Markdown and reference links to a moved note, in the form each was written, and marks links to a deleted note stale |
| `canvas_tools` | Reads and writes JSON Canvas files, and finds the canvases that reference a note |
| `bases` | Parses `.base` files and evaluates their filters, formulas, sort and grouping over the vault |
| `task_query` | Finds tasks by status, dates, tags, folder, priority and text |
| `periodic`, `moment_format` | Daily to yearly notes, named by the vault's Daily notes or Periodic Notes settings |
| `templates` | Built-in note templates (`doc`, `task`, `research`) with `{field}` placeholders |
| `quality_tools`, `grounding`, `okf_tools`, `validation_tools` | Note quality scoring, grounding analysis, Open Knowledge Format checks and indexes, Obsidian validation |
| `audit_tools`, `diff_tools` | Audit log queries, rollback, diffs |
| `export_tools`, `viewer` | JSON and CSV reports, the self-contained HTML graph view |
| `vault_lifecycle` | Registering and creating vaults under a `VaultRootPolicy` |
| `sql_engine` | SQL over frontmatter (feature `sql`; see turbovault-sql) |

## Example

```rust
use std::sync::Arc;
use turbovault_core::{ServerConfig, VaultConfig};
use turbovault_tools::{SearchEngine, SearchQuery};
use turbovault_vault::VaultManager;

#[tokio::main]
async fn main() -> turbovault_core::Result<()> {
    let vault = VaultConfig::builder("notes", "/path/to/vault").as_default().build()?;
    let manager = Arc::new(VaultManager::new(ServerConfig::with_vaults(vec![vault]))?);
    manager.initialize().await?;

    let search = SearchEngine::new(manager.clone()).await?;
    for hit in search.advanced_search(SearchQuery::new("project plan").limit(5)).await? {
        println!("{:.2}  {}", hit.score, hit.path);
    }
    Ok(())
}
```

## License

MIT
