# The crates

TurboVault is a Rust workspace of 14 crates. The MCP server is `turbovault`; the others can be used on their own. Each crate's README covers its API, and docs.rs has the full reference.

| Crate | Purpose | Main types |
|---|---|---|
| [turbovault-core](../../crates/turbovault-core/README.md) | Shared types, configuration, errors, multi-vault management | `VaultFile`, `Link`, `Frontmatter`, `TaskItem`, `ServerConfig`, `VaultConfig`, `Error`, `ErrorCategory`, `MultiVaultManager`, `VaultCache`, `VaultRootPolicy`, `ChangePlan`, `write_atomic` |
| [turbovault-parser](../../crates/turbovault-parser/README.md) | Obsidian Flavored Markdown and JSON Canvas parsing | `Parser`, `ParseOptions`, `parse_blocks`, `parse_wikilinks`, `parse_reference_definitions`, the canvas model |
| [turbovault-graph](../../crates/turbovault-graph/README.md) | Link graph, health, centrality | `LinkGraph`, `HealthAnalyzer`, `HealthReport` |
| [turbovault-vault](../../crates/turbovault-vault/README.md) | The note cache, the freshness gate and the write path | `VaultManager`, `WriteSubstrate`, `EditEngine` |
| [turbovault-git](../../crates/turbovault-git/README.md) | Git write substrate: one commit per write or batch, fanout worktrees | `VaultRepo`, `CommitLocks`, `FanoutWorktree` |
| [turbovault-audit](../../crates/turbovault-audit/README.md) | Hash-chained audit log, snapshots, rollback | `AuditLog`, `AuditEntry`, `SnapshotStore`, `RollbackEngine` |
| [turbovault-batch](../../crates/turbovault-batch/README.md) | Batch operation types and validation | `BatchOperation`, `BatchResult` |
| [turbovault-export](../../crates/turbovault-export/README.md) | JSON and CSV reports | `HealthReportExporter`, `VaultStatsExporter`, `BrokenLinksExporter` |
| [turbovault-sql](../../crates/turbovault-sql/README.md) | SQL over frontmatter with GlueSQL | `FrontmatterSqlEngine` |
| [turbovault-tools](../../crates/turbovault-tools/README.md) | The logic behind the MCP tools | `SearchEngine`, `SimilarityEngine`, `GraphTools`, `BatchTools`, `TemplateEngine`, the Bases evaluator, task queries, periodic notes |
| [turbovault-plugin-api](../../crates/turbovault-plugin-api/README.md) | The contract for compiled-in plugins (versioned on its own) | `VaultApi`, `Plugin`, `PluginProvider`, `PluginDescriptor` |
| [turbovault-vector](../../crates/turbovault-vector/README.md) | Hybrid dense and BM25 search (versioned on its own) | `IndexEngine`, `DenseIndex`, `LexicalIndex`, `Model2VecEmbedder` |
| [turbovault-plugin-vector](../../crates/plugins/turbovault-plugin-vector/README.md) | The `vector_search` plugin | |
| [turbovault](../../crates/turbovault/README.md) | The MCP server | `ObsidianMcpServer` |

Dependencies run downward: `core` is at the bottom; `parser` and `graph` build on it; `vault` brings in the parser, graph, audit and git crates; `tools` builds on `vault`; `turbovault` builds on everything.
