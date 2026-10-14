# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

TurboVault is an MCP (Model Context Protocol) server that gives AI agents safe, fast access to Obsidian vaults. It's written in Rust and exposes 52 tools (30 listed by default, 22 advanced ones on request), note resources, workflow prompts and completions, for reading, writing, searching, analyzing and managing notes.

**Key design principle**: built on the TurboMCP framework (3.x; the exact version is in the workspace Cargo.toml), which provides typed tool definitions, transports, auth and telemetry. TurboVault adds the vault: write safety, an index that can't miss an outside edit, and the tools on top.

## Development Commands

### Build and test
```bash
just build            # cargo build
just release          # cargo build --release (fat LTO)
just test             # fmt-check, lint, test-all
just test-quick       # tests only
just test-one module::test_name

cargo test --workspace --all-features
cargo test --workspace --tests     # integration tests only
cargo test --workspace --lib       # unit tests only
```

### Code quality
```bash
cargo fmt --all
cargo clippy --workspace --all-features --all-targets -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features
cargo machete                      # unused dependencies (CI runs it)
cargo deny check && cargo audit --deny warnings
```

CI runs clippy on current stable; a newer clippy can flag code an older one passed, so keep the local toolchain current (`rustup update stable`).

### Snapshots, properties, fuzzing, benchmarks
```bash
# The tool catalog snapshot (names, descriptions, input and output schemas).
# Re-bless after changing any tool, then review the diff.
UPDATE_TOOL_CATALOG=1 cargo test -p turbovault --lib

# The parser contract snapshot.
UPDATE_PARSER_SNAPSHOT=1 cargo test -p turbovault-parser --test test_contract_snapshot

# Property tests run with the normal suite; raise the case count to dig.
PROPTEST_CASES=4096 cargo test -p turbovault-tools --test proptest_rewriter

# Fuzz targets (nightly, outside the workspace): parse_note, parse_blocks,
# frontmatter, edit_blocks, rewrite_links.
cd fuzz && cargo +nightly fuzz run parse_note -- -max_total_time=60

# Benchmarks, including a 10,000-note vault.
cargo bench -p turbovault --bench vault_10k
cargo bench -p turbovault-git --bench vault_size
```

CI also runs a semver check against the latest release tag. On pull requests it's advisory (a warning in the job summary); the release workflow enforces it, so a tag whose version is too small for the API changes is refused. See `docs/development/releasing.md`.

### Running the server
```bash
./target/release/turbovault --vault /path/to/vault --profile production
./target/release/turbovault                       # no vault: add one with add_vault
./target/release/turbovault --vault /path/to/vault --transport http --port 3000
./target/release/turbovault --config ~/.turbovault/config.yaml
```

## Architecture

The workspace has 14 members:

```
turbovault-core        Core types, MultiVaultManager, configuration, errors
  ├─ models            VaultFile, Link, Frontmatter, FileMetadata, TaskItem
  ├─ error             Error and ErrorCategory (see Error Handling)
  ├─ config            ServerConfig, VaultConfig, VaultGitConfig, SnapshotRetention
  ├─ profiles          ConfigProfile (7 profiles; see below)
  ├─ multi_vault       MultiVaultManager, VaultInfo
  ├─ cache             VaultCache: the persisted vault registry (not file content)
  ├─ vault_roots       VaultRootPolicy: which directories add_vault may register
  ├─ atomic_write      write_atomic, the one temp-then-rename implementation
  └─ validation        Content validators behind the validate_vault tool

turbovault-parser      Obsidian Flavored Markdown parsing (one pulldown-cmark
                       engine): wikilinks, embeds, Markdown and reference
                       links, tags, tasks, callouts, frontmatter, headings,
                       blocks, JSON Canvas

turbovault-graph       Link graph (petgraph StableDiGraph), health, centrality

turbovault-vault       VaultManager and the write path
  ├─ manager/          mod (struct, change listener, graph queries), state
  │                    (fingerprints, cache entries), scan (parsing, initialize,
  │                    sync_index), freshness (the freshness gate), write
  │                    (apply_through_substrate, allowed_operations), paths
  │                    (resolution, protected directories)
  ├─ substrate.rs      WriteSubstrate: Direct / Git dispatch
  ├─ edit.rs           EditEngine (SEARCH/REPLACE with fuzzy matching)
  └─ reindex.rs        Git reindex queue and HEAD watcher

turbovault-batch       Batch operation types and intra-batch validation (types only)
turbovault-export      JSON/CSV report export
turbovault-audit       Audit log (hash-chained), snapshots, rollback
turbovault-git         Git write substrate (every write is a commit), fanout worktrees
turbovault-sql         SQL over frontmatter (GlueSQL; feature `sql`)
turbovault-tools       Tool implementations as a library: search engine,
                       similarity, link rewriter, templates, Bases, tasks,
                       periodic notes, canvases, quality, OKF
turbovault-plugin-api  Stable facade for compiled-in plugins (versioned on its own)
turbovault-vector      Hybrid dense + BM25 search (versioned on its own)
plugins/turbovault-plugin-vector   The vector_search plugin (feature `vector-search`)
turbovault (binary)    CLI and MCP server
  ├─ cli.rs            Arguments, transports, startup
  ├─ security.rs       SecuritySettings: vault roots, HTTP auth, rate limits
  ├─ tool_visibility.rs  Allowed/hidden/disabled tools, ADVANCED_TOOLS
  ├─ tools.rs          ObsidianMcpServer, VaultRuntime, error mapping
  └─ tools/            providers/ (the MCP tools, one file per group, plus
                       tool_catalog.json), note_resources, core_prompts,
                       engine_slots, plugin_host
```

### Data flow

1. A client sends a tool call (stdio by default; HTTP, WebSocket, TCP or a Unix socket otherwise).
2. The composite server in `tools/providers.rs` dispatches it: it scopes the call (`AuditContext`, the optional `vault` argument), runs the provider method, and turns errors into MCP errors with a category and a hint.
3. The provider calls `turbovault-tools` and the domain crates.
4. Reads come from `VaultManager`'s parsed note cache; derived reads (search, graph, similarity) pass the freshness gate first.
5. Writes become a `ChangePlan` and go through `VaultManager::apply_changes` to the vault's write substrate.
6. The result goes back as `structuredContent` matching the tool's `outputSchema`, plus a compact JSON text copy.

### Multi-vault

`MultiVaultManager` (core) holds the registered vaults and the active one. `CoreToolHandler` in `tools.rs` keeps one `VaultRuntime` per vault (manager, audit log, snapshot store, Git lock registry), so `remove_vault` drops all of it at once. The search and similarity engines live in separate slots (`engine_slots.rs`) because the change listener holds them and must never hold the manager.

Every vault-scoped tool takes an optional `vault` argument: an explicit `vault` wins, otherwise the active vault is used, and naming a vault never changes the active one. The dispatcher strips the argument and runs the call with that vault in a task-local.

`VaultCache` persists the registry and the active vault per project (a hash of the working directory) under the cache root, so a fresh process per conversation remembers runtime-added vaults. It merges under a file lock, so processes sharing a project keep each other's vaults.

## Key Architectural Concepts

### 1. Write substrate
Every mutator translates its request into a `ChangePlan` for `VaultManager::apply_changes`, which checks the vault's `allowed_operations` and the protected-path policy, then hands it to `WriteSubstrate`:
- **Direct**: temp file plus atomic rename through `turbovault_core::write_atomic` (fsync, permissions kept, short `.tvtmp-<hex>` temp names). A plan applies in order and isn't rolled back; a partial apply is reported with what landed. Writes take a cross-process lock on `.turbovault/write.lock`.
- **Git** (turbovault-git): blobs, an isolated index, a tree, a commit and a ref compare-and-swap, then the working tree is written out. All or nothing. If writing out fails after the ref moved, a resync marker lets the next write (or the next start) finish it.

Preconditions (`expected_hash`, `expect_absent`) give optimistic concurrency on both backends. A link-aware move writes the destination and link rewrites before removing the source, and always holds the source to the content it read.

### 2. Freshness gate (`VaultManager::ensure_fresh`)
Search, the link graph, similarity, vault stats, note resources and the plugin change feed are derived from the notes. `ensure_fresh` drains pending Git commits, compares a `(size, mtime)` scan against each cache entry's fingerprint, and applies the difference through the same change listener the write path uses.

- **Not a watcher.** Filesystem notifications drop events and report nothing on network and cloud-synced vaults; comparing state can't miss a change.
- **Racy-clean rule.** A file read within 2.1 s of its mtime is re-checked by content hash on the next pass, so a same-size edit inside a coarse mtime tick (FAT, HFS+, SMB) is still seen.
- **Debounced.** At least 500 ms between passes, stretched to 20x the last pass on a large vault. `ensure_fresh_now` skips the debounce (link-aware moves and deletes use it); `reconcile_now` forces a pass.
- **Where to call it.** Manager-internal derived reads already do. Above the vault layer: the search and similarity engine getters, plugin dispatch, and anything new that serves derived state. Read the graph through `link_graph_flushed()`.
- **Domain.** The note cache and the sweep both model markdown notes only (`turbovault_core::is_note`, case-insensitive `.md`), and the sweep uses the same `ScanSpec` as the walk, so a file one side includes is never republished on every pass.
- **Listener contract.** The change listener is awaited, not spawned, so it must never capture an `Arc<VaultManager>`.

### 3. Search, similarity and the graph
- **Search** is tantivy BM25, fed from the parsed note cache, with one long-lived writer per index. The index persists under `<cache root>/search/<16 hex of the vault path>/` (never inside the vault) with each note's content hash, so a restart re-indexes only what changed. `TURBOVAULT_SEARCH_INDEX=memory` keeps it in memory.
- **Similarity** (find_similar, find_duplicates, compare_notes) is TF-IDF over interned terms, built from the cache and updated in place.
- **The graph** is a `StableDiGraph`. Unresolved links are keyed by target, so adding a note re-resolves only the links that could name it. A bare link to one of several same-named notes resolves to the one in the fewest folders, then the first by path.

### 4. Edit engine
`EditEngine` (`turbovault-vault/src/edit.rs`) applies SEARCH/REPLACE blocks with a cascade: exact, whitespace-insensitive, indentation-preserving, then Levenshtein (85% similarity), bounded by a search length and work budget. Fuzzy matching runs on the blocking pool.

### 5. Tool surface conventions
- **Descriptions and annotations.** Every parameter has a description written for an agent, and every tool has a title and all four hints. `tests/test_tool_catalog_quality.rs` enforces this, and that no read-only tool is destructive.
- **Typed output.** Every tool advertises an `outputSchema` (result types derive `JsonSchema` behind each crate's `schemars` feature). `tests/test_tool_output_schemas.rs` calls every tool on a real vault and validates the result, so schemas can't drift.
- **Catalog snapshot.** `tools/providers/tool_catalog.json` is the golden copy of the whole catalog; re-bless with `UPDATE_TOOL_CATALOG=1`.
- **Pagination.** List tools take `limit` (default 50, capped per tool) and `offset`, and report `meta.pagination` (`total`, `returned`, `truncated`, `next_offset`). Every paged list is totally ordered.
- **Errors** carry an `ErrorCategory` and a next step (see Error Handling). Paths in errors are vault-relative.
- **Visibility.** `tool_visibility::ADVANCED_TOOLS` (22 tools) are callable by name but listed only with `--show-advanced-tools`.

### 6. Resources, prompts, progress and confirmation
- Every note is a resource, `obsidian://note/{vault}/{+path}`, listed, read as markdown and subscribable. The change listener sends `notifications/resources/updated` to subscribers and `list_changed` when notes come or go; while anyone is subscribed, a 2 s sweep catches outside edits without a tool call.
- Five workflow prompts live in `tools/core_prompts.rs`, with completion for their arguments and the note template.
- Slow tools report progress when the client sends a progress token, and honor cancellation only before they write.
- `delete_note`, a `move_note` that replaces a note and a forced `rollback_note` ask the user to confirm through elicitation when the client supports it. `--no-confirm-destructive` turns this off.

### 7. Security
- **Vault registration.** `add_vault` is checked against `VaultRootPolicy`: a filesystem root, the home directory, hidden and application-data directories under home, and system directories are always refused; `--allowed-vault-roots` limits it further. With no roots configured, runtime registration is open on stdio and closed on network transports. Tool-supplied paths expand `~` only, never `$VAR`.
- **Network transports.** HTTP can require `bearer` or `jwt` auth (served as an OAuth protected resource with RFC 9728 metadata) and refuses to listen beyond loopback without it. WebSocket and TCP have no auth in TurboMCP, so they stay on loopback. The Unix socket defaults to a private directory under `$XDG_RUNTIME_DIR` or `~/.turbovault/run`. `--allow-unauthenticated-network` is the escape hatch for a proxy that authenticates.
- **Protected directories.** `excluded_paths` (`.obsidian`, `.git`, `.DS_Store`, `node_modules` by default) and the non-configurable `.turbovault/` are unreachable through the note tools on both backends. A vault's own list is added to the server's, never in place of it.
- **Bounds.** `max_file_size` (10 MiB) on reads, writes and scans; result counts capped at every tool; `query_frontmatter_sql` is SELECT-only, 10 s, 1000 rows.
- **Audit.** Each entry records the tool call, request and client that made it, chains to the previous entry by hash, and is fsynced. `audit_log` with `summary: true` reports whether the chain is intact. Rollback refuses to discard a later edit unless forced, and is itself undoable.

### 8. Configuration profiles
Seven profiles (`turbovault-core/src/profiles.rs`): `development` (debug logging), `production`, `multi-vault` and `collaboration` (info), `read-only` and `high-performance` (warn), and `minimal` (error, and no reconciling outside edits). A profile sets only those; `read-only` also refuses every mutating tool.

### 9. Observability
Logging is `tracing` throughout (stdio logs JSON to stderr; `RUST_LOG` overrides the profile's level). `--otlp-endpoint` exports traces, `--metrics-port` serves Prometheus on 127.0.0.1, and every tool call is a `tool_call` span with the tool, vault, outcome and duration, never its arguments or note content.

## Testing Conventions

Unit tests sit next to the code in `#[cfg(test)]` modules; integration tests are in each crate's `tests/`. Notable suites:
- `crates/turbovault/tests/test_mcp_wire_e2e.rs` and `test_mcp_surface_wire.rs` spawn the real binary over stdio.
- `wss_matrix.rs` and `write_safety_suite.rs` hold the clobber-safety matrix; CI ratchets the pending count against `docs/write-safety-suite/pending-baseline`.
- `test_external_edit_coherence.rs` checks search and the graph after outside edits.
- Proptests: `turbovault-parser/tests/proptest_parser.rs`, `turbovault-tools/tests/proptest_rewriter.rs`, `turbovault-vault/tests/proptest_edit.rs`.

Tests use real temp directories and real Git repositories, no mocks. On Windows a temp path may resolve with a `\\?\` prefix, so compare vault-relative paths.

## Build Features

```bash
cargo build --release --no-default-features      # stdio only
cargo build --release --features http            # + HTTP (and HTTP auth)
cargo build --release --features full            # stdio, HTTP, WebSocket, TCP
cargo build --release --features all-transports-unix   # + Unix socket (not Windows)
cargo build --release --features sql             # inspect_frontmatter, query_frontmatter_sql
cargo build --release --features vector-search   # the vector_search plugin (enable with --plugins)
cargo build --release --features release         # what the prebuilt binaries use
```

Without `sql`, the two SQL tools return a message saying how to enable them.

## Important Implementation Details

### Path resolution
`VaultManager::resolve_path` normalizes lexically, canonicalizes the root and the target (`soft_canonicalize`, so new paths work), checks the result stays inside the vault, then applies the protected-directory policy to the path as written and as resolved. `relative_path` renders a vault-relative `/` path for any form the root can take.

### Link graph updates
A write publishes a change through the change listener: the cache entry is replaced, the graph removes the note's old edges and adds the new ones, and search and similarity apply the same change. Deleting a note turns its inbound links into broken links at once.

### Batch operations
`turbovault-batch` defines the operation types and checks conflicts within a batch; it executes nothing. `batch_execute` translates a batch into one `ChangePlan`:
- On a Git vault, a batch is one commit: all operations land or none do.
- On a Direct vault, operations apply in order and a failure partway is reported with what already landed.

### Templates
Templates are built in (`doc`, `task`, `research` in `turbovault-tools/src/templates.rs`) with `{name}` placeholders filled in one pass; frontmatter is written with a YAML serializer. `create_from_template` records the template id in the note's frontmatter.

## Error Handling

`turbovault_core::Error` is `#[non_exhaustive]`, with variants including `Io`, `IoAt` (keeps the path), `FileNotFound`, `InvalidPath`, `PathTraversalAttempt`, `ProtectedPath`, `FileTooLarge`, `ParseError`, `ConfigError`, `ValidationError`, `ConcurrencyError`, `NotFound`, `OperationNotAllowed` and `Other`. `Error::category()` maps each to an `ErrorCategory` (`NotFound`, `InvalidInput`, `PermissionDenied`, `Conflict`, `Unavailable`, `Internal`), which decides the MCP error kind. Use `ConfigError` only for real configuration problems; bad tool input is a `ValidationError`.

`turbovault_core::Result<T>` = `Result<T, Error>`.

## Performance

Measured on synthetic vaults, release build, over MCP stdio:

| Operation | Measured | Notes |
|---|---|---|
| `read_note` | 0.5-2 ms | From the note cache |
| `get_links` (hub note) | 4 ms at 10k, 23 ms at 100k | Graph lookup |
| `write_note` | 14-31 ms | 21 ms with the similarity engine loaded |
| Git-backed write | 20 ms at 2,000 notes | Was 197 ms; no longer grows with the vault |
| `search` (warm) | 3.5-8 ms up to 100k | Tantivy BM25 |
| Search after a restart | 258 ms at 50k | Persisted index; was an 18.5 s rebuild |
| `vault_health` (quick) | 16 ms at 50k | Was 879 ms |
| `explain_vault` | 179 ms at 50k | Was 2.9 s |
| Startup | 2.5 s and 0.93 GB at 50k | Parallel parse |

## Common Development Patterns

### Adding a tool
1. Add the method to the right provider in `crates/turbovault/src/tools/providers/` with `#[tool(...)]`: a description that says when to use it rather than a sibling, `#[description]` on every parameter, a title and all four annotation hints.
2. Return a typed result (derive `JsonSchema` behind `schemars`) so it advertises an `outputSchema`; page lists with the `limit`/`offset` convention.
3. Put logic in `turbovault-tools` or a domain crate, not in the provider.
4. Add the tool to `test_tool_output_schemas.rs` fixtures, decide whether it belongs in `ADVANCED_TOOLS`, and re-bless the catalog.

### Adding a vault operation
1. Add the method to the right `turbovault-vault/src/manager/` module.
2. Build a `ChangePlan` and go through `apply_changes`; never write files directly.
3. Serve derived state only after `ensure_fresh`.

## Troubleshooting

- **Build issues**: Rust 1.90+ builds the crates; the test suite needs 1.93.1+ (`serial_test`). `cargo clean` if things look strange.
- **Test failures**: `--nocapture` for output; temp directories must be writable.
- **Runtime issues**: logs go to stderr on stdio; vault paths must exist; `add_vault` refusals name the policy that refused them.
- Do not include authorship info in commit messages.
