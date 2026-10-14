# Architecture

[CLAUDE.md](../../CLAUDE.md) has the full guide; this page is the overview.

```
MCP client (Claude Desktop, Claude Code, an HTTP client)
        │  stdio, HTTP, WebSocket, TCP or Unix socket (TurboMCP)
        ▼
turbovault (binary)
  cli.rs, security.rs        transports, auth, vault root policy
  tools/providers.rs         one composite server: scoping (vault argument,
                             audit context), error mapping, visibility
  tools/providers/*.rs       the 52 tools, grouped by provider
  tools/note_resources.rs    notes as resources and subscriptions
  tools/core_prompts.rs      workflow prompts and completion
        │
        ▼
turbovault-tools             tool logic: search engine, similarity, link
                             rewriter, templates, Bases, tasks, canvases,
                             periodic notes, quality, OKF
        │
        ▼
turbovault-vault             VaultManager: parsed note cache, freshness gate,
                             apply_changes, path policy
  ├─ turbovault-parser       Obsidian Flavored Markdown, JSON Canvas
  ├─ turbovault-graph        link graph
  ├─ WriteSubstrate          Direct (atomic renames) or turbovault-git (commits)
  └─ turbovault-audit        audit trail, snapshots, rollback
        │
        ▼
the vault on disk
```

## Crates

| Crate | Responsibility |
|---|---|
| `turbovault-core` | Models, configuration, errors and `ErrorCategory`, `MultiVaultManager`, `VaultCache`, the vault root policy, `write_atomic`, validation |
| `turbovault-parser` | One pulldown-cmark engine for wikilinks, embeds, Markdown and reference links, tags, tasks, callouts, frontmatter, headings and blocks; JSON Canvas |
| `turbovault-graph` | `LinkGraph` on a `StableDiGraph`, health, centrality |
| `turbovault-vault` | `VaultManager` (`manager/`: state, scan, freshness, write, paths), `WriteSubstrate`, `EditEngine`, the Git reindex queue |
| `turbovault-git` | The Git substrate: commits with ref compare-and-swap, materialization, resync after an interrupted write, fanout worktrees |
| `turbovault-audit` | Hash-chained audit log, snapshot store, rollback |
| `turbovault-batch` | Batch operation types and intra-batch validation |
| `turbovault-export` | JSON and CSV reports |
| `turbovault-sql` | GlueSQL over frontmatter (feature `sql`) |
| `turbovault-tools` | The logic behind the tools |
| `turbovault-plugin-api` | The contract for compiled-in plugins (versioned on its own) |
| `turbovault-vector` and `plugins/turbovault-plugin-vector` | Hybrid dense and BM25 search, mounted as the `vector_search` plugin |
| `turbovault` | The server |

## Request flow

1. A tool call arrives; the composite server strips the optional `vault` argument and runs the call with that vault (or the active one) in scope, inside an audit context naming the tool and client.
2. The provider calls into `turbovault-tools`.
3. Reads use the parsed note cache. Derived reads (search, graph, similarity) pass `ensure_fresh` first, which applies any edit made outside TurboVault.
4. Writes become a `ChangePlan` for `VaultManager::apply_changes`, which checks `allowed_operations` and protected paths and dispatches to the vault's write substrate. The change listener then updates the cache, graph, search index, similarity engine and resource subscribers.
5. The result returns as `structuredContent` matching the tool's `outputSchema`, with a compact JSON copy in the text content. Errors carry an `ErrorCategory` and a hint.

## The provider facade

TurboMCP's composite handler prefixes mounted handlers internally; the facade maps them back to flat public names, so clients call `read_note`, not `files_read_note`. Compiled-in plugins keep a namespace, such as `vector_search_search`. See [Plugins](plugins.md).
