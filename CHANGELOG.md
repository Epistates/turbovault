# Changelog

All notable changes to TurboVault will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Compiled-in plugin boundary** ([#34](https://github.com/Epistates/turbovault/issues/34)): Added the default-off `plugin-api` feature and the publishable `turbovault-plugin-api` crate. Plugins receive a curated CAS-only `VaultApi`, an object-safe tool provider contract, redacted request context, strict MCP namespaces, and a bounded hook bus with explicit lag/resync and close semantics.
- **Best-effort hook provenance** ([#33](https://github.com/Epistates/turbovault/issues/33)): Plugin writes can carry source and correlation metadata into advisory event envelopes. Attribution is explicitly not an authentication boundary, and uncorrelated events fail open as external-or-unknown.
- **Vault change feed covers every mutation** ([#43](https://github.com/Epistates/turbovault/issues/43)): Core MCP writes (`write_note`, `edit_note`, `delete_note`, `move_note`, `move_file`, frontmatter/tag updates, templates, batches, index generation, rollback) now publish to the hook bus, as do commits that arrive on a Git-backed vault's ref from outside the process. Previously only plugin writes did, so a subscriber saw almost nothing.
- **Per-plugin declared capabilities** ([#41](https://github.com/Epistates/turbovault/pull/41)): `Plugin::capabilities` declares the exact application-config paths a plugin reads, and `VaultApi::read_config` serves only those. The host builds one `VaultApi` per plugin, validates declarations at mount time, normalizes paths before matching, and rejects symlink escapes.
- **Plugin-private persistent storage** ([#42](https://github.com/Epistates/turbovault/issues/42)): `PluginContext::storage` is durable per-vault key/value storage namespaced under `<vault>/.turbovault/plugins/<id>/`. Isolation is structural rather than declared — the plugin id is baked into the store, so no argument reaches another plugin's data — and the directory is unreachable through the note APIs, so an index cannot be read as a note. Individual writes are atomic.
- **Affordable reconciliation** ([#43](https://github.com/Epistates/turbovault/issues/43)): `VaultApi::list_notes_detailed` returns each note's size and modification time from the stat the vault scan already performs, and `read_notes` batch-reads, resolving the vault once instead of once per note. Together these make the reconcile half of watch-and-reconcile cost one stat per note instead of one full read per note — without which a plugin maintaining an index would have to reach past the boundary to the filesystem.
- **Plugins contribute all three MCP primitives**: `PluginProvider` gained `resources`/`resource_templates`/`read_resource` and `prompts`/`get_prompt` alongside tools. Previously a plugin could only be reached by a tool call the model chose to make, with no way to offer state a user attaches as context or a workflow a user invokes by name. Prompts are namespaced like tools (`tasks_review`) and resources under the plugin's own URI scheme (`tasks://index/stats.json`); the host republishes returned content URIs into that namespace, so no plugin can serve content under a URI it does not own. Resource reads and prompt renders share the wall-clock budget and panic isolation already applied to tool calls.
- **Resource templates for URI spaces that track the vault**: a plugin's whole scheme routes to it, so a URI expanded from a template reaches `read_resource` without being enumerated. Template brace structure is validated when the plugin mounts, and a plugin may no longer claim a scheme the vault itself publishes under.
- **Argument completion** (`completion/complete`): `PluginProvider::complete` suggests values for prompt arguments and resource-template expressions, so a client can offer the paths that exist instead of asking a person to guess one. TurboMCP's `CompositeHandler` does not route completion, so the server dispatches it directly, resolving the public reference to the owning plugin and stripping the namespace first. The host enforces MCP's cap on a single response and sets `hasMore`, rather than trusting each plugin to remember it, and the capability is advertised only when a plugin contributes a prompt or a template.
- **Detectable event-feed discontinuity**: `HookBus::epoch` identifies a bus run and `VaultEventEnvelope` carries it, so the new `EventCursor` can answer whether a stored position still means anything. Sequence numbers restart with the process, so a consumer that persisted a bare sequence and compared it against the next run would silently skip everything that changed while it was down; `EventCursor::resumes_on` turns that into an explicit reconcile.
- **Plugin lifecycle and call isolation**: `PluginProvider::start` spawns background work on the host's runtime after mounting and before serving (`Plugin::build` is synchronous and may run outside a runtime), with `PluginContext::shutdown` as the cooperative stop signal. `PluginProvider::shutdown` runs during graceful shutdown, after the signal fires. Each plugin tool call is bounded by a wall-clock budget and isolated from panics so a misbehaving plugin fails one call instead of a request or the server.
- **Graceful shutdown is wired**: SIGTERM/SIGINT and normal transport exit now run fanout cleanup and close the hook bus. The cleanup path existed but nothing called it, so every interrupted session leaked its fanout worktrees.
- **`add_vault` selects a write backend**: Pass `write_backend: "git"` (or `"direct"`) when registering a vault at runtime. Previously only startup configuration could choose a backend, so runtime-registered vaults could never use the Git substrate. Unknown values are refused rather than silently defaulting, and `git` without a repository fails at registration instead of at the first write.

### Changed

- **Plugin contract review follow-up**: Plugin-local and fully namespaced tool names are centrally validated against MCP SEP-986, enabled namespaces are described during MCP initialization only when plugins are registered, feature-on tests are required for every vertical, and core/plugin complete-note writes share preparation and cache-finalization orchestration.
- **Plugin contract shape (`turbovault-plugin-api`, first release)**: Every `VaultApi` operation names its vault, and `WriteNoteRequest` carries a required `vault` field, so a concurrent `set_active_vault` produces a refusal instead of a write landing in a vault the plugin never read. Public types are now `#[non_exhaustive]` with constructors and builder setters, `PluginErrorCode` gained `PermissionDenied` and `Timeout`, `VaultEventEnvelope` gained a host-stamped `plugin_id` and an `epoch`, and `VaultDescriptor::write_backend` reports `direct` instead of `legacy`. Construct types via their `new` functions rather than struct literals.
- **Plugin write receipts read back the stored note** rather than hashing the request, so the returned CAS token always matches what a subsequent read returns.
- **Capabilities advertise only what is implemented**: `listChanged` is now reported as `false` for tools, resources, and prompts. TurboMCP derives `true` from a non-empty listing, but TurboVault's catalog is fixed when the server is assembled and it emits no `notifications/*/list_changed` — a client that trusted the derived claim would stop re-listing and wait for a message that never arrives.
- **Derived-cache invalidation is vault-scoped**: writes invalidate the caches of the vault that was written rather than whichever vault happens to be active, which could evict the wrong vault's caches — and always did for the background Git reindex drainer.

### Security

- **Protected directories are enforced at the access boundary**: `excluded_paths` (`.obsidian`, `.git`, `node_modules`, `.DS_Store` by default) and the non-configurable `.turbovault/` state directory were previously applied only when scanning for notes, so `read_note`/`write_note` could reach them by path on both backends. Writing `.obsidian/plugins/*/main.js` or `.git/hooks/*` is code execution by another name, and `.turbovault/` holds the audit trail. Both backends now refuse; the plugin config capability is the only sanctioned exception and is per-plugin and path-scoped.
- **`max_file_size` is enforced on reads and writes**, not only while scanning directories, so an explicit read can no longer pull an arbitrarily large file into memory.
- **`WritePrecondition::CreateOnly` is atomic on the direct backend** via exclusive file creation. It was a check-then-write, which let two concurrent create-only writers both proceed — the blind overwrite the precondition exists to prevent.

## [1.6.0] - 2026-07-17

### Added

- **Atomic Git-backed writes** ([#32](https://github.com/Epistates/turbovault/issues/32), [#37](https://github.com/Epistates/turbovault/pull/37)): New `turbovault-git` crate and opt-in `write_backend: git` mode. Each mutation builds an isolated Git tree and advances the branch with compare-and-swap; a multi-operation batch is one commit, so stale preconditions or later failures apply none of it.
- **Cross-process write coordination**: An advisory repository lock spans commit creation, ref advancement, and working-tree materialization. TurboVault refuses to overwrite dirty or untracked touched paths and refuses writes while the Git index contains staged changes.
- **Git worktree fanout tools**: `begin_fanout`, `commit_fanout`, `abandon_fanout`, and `list_orphan_fanouts` provide isolated workspaces for parallel agents, with fast-forward or merge-back support.
- **Commit-driven derived-state reindexing**: Git-backed writes enqueue changed commit ranges and reconcile the graph/search state before dependent reads. External ref advances such as `git pull` are detected and indexed.
- **Open Knowledge Format support**: Detect OKF bundles, validate OKF v0.1 conformance, resolve OKF cross-links, generate bundle indexes, surface grounding context, maintain change logs, and render an HTML vault viewer.
- **Configurable tool visibility** ([#25](https://github.com/Epistates/turbovault/pull/25)): Allow, hide, or disable tools by name or tag through YAML, environment variables, and CLI options.
- **Transport environment configuration** ([#26](https://github.com/Epistates/turbovault/pull/26)): Added the documented environment-variable surface for vault, config, tool visibility, and logging behavior.

### Changed

- **74 MCP tools**: The public tool surface grew from 70 to 74 with Git fanout, and is now implemented as focused providers composed through TurboMCP while preserving the existing flat tool names.
- **Git-aware mutation routing**: Note writes, edits, deletes, moves, templates, frontmatter/tag updates, binary moves, and batches use the selected write backend. Git-backed note moves update inbound wikilinks atomically by default.
- **Safer overwrite contract**: Existing-note overwrites require the hash returned by `read_note` unless `force=true` is explicitly supplied. Edit and batch preconditions are revalidated at the final write boundary.
- **Batch execution contract**: Git-backed batches are all-or-nothing with per-path CAS. The compatibility backend remains sequential/fail-fast and now reports that execution mode and warns when completed operations were not rolled back.
- **Performance**: Vault-wide metadata queries use validated cache-first scans, and Git repository handles and derived-state updates are reused or incrementally reconciled.
- **Dependencies**: Updated TurboMCP to 3.1.5 and Git bindings to `git2` 0.21 / libgit2 1.9.4, alongside current compatible transitive security updates.

### Fixed

- **Attachment graph pollution**: Non-Markdown attachment links are excluded consistently from note graph ingestion and broken-link analysis.
- **Edit TOCTOU race**: `edit_note` carries the validated pre-image hash into the final write instead of silently overwriting an intervening change.
- **Batch stale-write window**: Supplied write/delete/move hashes are checked before legacy execution, while Git-backed batches recheck them against the atomic commit base.
- **Provider contract drift**: Checked-in tool catalog, dispatch, schema, tag, visibility, and shared-state tests protect the decomposed server surface.

### Security

- **Working-tree protection**: Git-backed writes reject symlinks/non-regular touched paths, untracked collisions, dirty touched files, and staged-index state before materialization.
- **Dependency audit refresh**: Removed Git-specific RustSec warnings by upgrading `git2`; only two allowed transitive warnings remain in the audit output.

### Upgrade notes

- Existing installations keep the compatibility write backend. To enable atomic multi-file transactions, point TurboVault at an existing Git repository and set `write_backend: git` in that vault's YAML configuration.
- Git commits are the atomic source of truth. Working-tree paths are replaced atomically one file at a time; interrupted materialization is recoverable by idempotently resynchronizing from `HEAD`.
- Audit/rollback MCP tools continue to serve the compatibility backend. For Git-backed vaults, use Git history and restore operations; TurboVault returns an explicit backend-specific message instead of an unrelated legacy audit.

## [1.5.0] - 2026-05-01

### Added

- **Obsidian Tasks metadata parsing** ([#17](https://github.com/Epistates/turbovault/pull/17)): New `turbovault-core::task_parser` module (winnow-based) extracts trailing task metadata in both Obsidian Tasks emoji format and Dataview inline-field format, without requiring vault-level configuration. `TaskItem` now exposes `created_date`, `scheduled_date`, `start_date`, `due_date`, `done_date`, `cancelled_date`, `priority` (typed `TaskPriority`), `recurrence`, `on_completion`, `id`, `depends_on`, `tags`, `block_ref`, and a `metadata` map for custom Dataview fields.
- **`TaskPriority` enum**: Stable `SCREAMING_SNAKE_CASE` serde representation (e.g. `"HIGH"`), with emoji and `char` round-trip helpers.
- **Windows test compatibility** ([#16](https://github.com/Epistates/turbovault/pull/16)): `test_file_tools_delete_locked_file` and the atomic rollback test now compile and pass on Windows runners; rollback assertion is gated `not(windows)` so macOS still verifies it.

### Changed

- **BREAKING — `TaskItem` schema**: `due_date` changed from `Option<String>` to `Option<NaiveDate>`. New typed date fields (`created_date`, `scheduled_date`, etc.) replace ad-hoc string parsing. Legacy JSON without the new fields still deserializes (all new fields are `#[serde(default)]`); legacy JSON with `due_date` as a non-ISO-date string will now fail to parse.
- **Dependency refresh**: `turbomcp` 3.1.0 → 3.1.2, `tantivy` 0.26.0 → 0.26.1, `rustls` 0.23.38 → 0.23.40, `rustls-platform-verifier` 0.6.2 → 0.7.0, `reqwest` 0.13.2 → 0.13.3, plus assorted patch updates across `wasm-bindgen`, `metrics`, `rkyv`, `cc`, `libc`, and others.

### Security

- **RUSTSEC-2026-0104** (`rustls-webpki` reachable panic in CRL parsing): bumped to 0.103.13 transitively via `cargo update`.

## [1.4.1] - 2026-04-20

### Fixed

- **Inline code rendering in headings, blockquotes, and tables** ([#15](https://github.com/Epistates/turbovault/pull/15)): Backticks in headings no longer leak into the following paragraph; backticks in blockquotes and table cells are preserved for re-parse and downstream renderers.

### Changed

- **Dependency refresh**: Updated all workspace dependencies to latest versions, including major-version bumps: `thiserror` 1.0 → 2.0, `config` 0.14 → 0.15, `dashmap` 5.5 → 6.1, `notify` 6 → 8, `petgraph` 0.6 → 0.8, `tantivy` 0.22 → 0.26, `similar` 2.7 → 3.1, and `turbomcp` 3.0.11 → 3.1.0.
- **Tantivy 0.26 API migration**: `TopDocs::with_limit(n)` now requires `.order_by_score()` to produce a `Collector`.
- **Similar 3.1 API migration**: `TextDiff` dropped the third lifetime parameter.
- **turbomcp hoisted to workspace dependency**: Unified `turbovault-tools` and `turbovault` binary on turbomcp 3.1.0 (previously mismatched at 3.0.14 and 3.1.0).

### Added

- **Regression tests for PR #15**: Cover inline code in headings, blockquotes, and table cells, plus a `ParseEngine` outline test.

## [1.4.0] - 2026-04-08

### Added

- **`turbovault-sql` crate**: New dedicated crate providing a GlueSQL-powered SQL query engine for vault frontmatter. Builds three in-memory tables (`files`, `tags`, `links`) and supports arbitrary SQL including JOINs, aggregations, and subqueries. Feature-gated behind `--features sql`.
- **`inspect_frontmatter` MCP tool**: Schema inspection for the SQL engine — shows column names, types, nullability, and counts across all vault notes. Always available; returns a helpful error when `sql` feature is not enabled.
- **`query_frontmatter_sql` MCP tool**: Execute arbitrary SQL against vault frontmatter via GlueSQL. Supports `files` (schemaless frontmatter), `tags` (unnested tag pairs), and `links` (from vault link graph) tables.
- **`search_by_frontmatter` MCP tool**: Dedicated tool for single frontmatter key-value queries, returning up to 100 ranked results.
- **`advanced_search` new parameters**: `frontmatter_filters` (array of `{key, value}` pairs with AND logic), `exclude_paths` (path prefix exclusions), and `limit` (configurable result count, default 10).
- **SQL session support**: `FrontmatterSqlEngine::session()` builds tables once for multiple queries, avoiding per-query rebuilds in multi-step LLM workflows.
- **11 SQL engine tests**: Covering schemaless roundtrips, aggregations, JOIN across files/tags/links tables, and GlueSQL value conversion.

### Fixed

- **`query_metadata` returned 0 results** ([#12](https://github.com/Epistates/turbovault/issues/12)): `Path::ends_with(".md")` does Rust path-component matching (always false for `.md`). Fixed to use string suffix check.

## [1.3.2] - 2026-04-07

### Fixed

- **`edit_note` hang on non-matching SEARCH blocks** ([#10](https://github.com/Epistates/turbovault/issues/10)): Replaced O(n·m³) brute-force sliding window Levenshtein with O(n·m) semi-global alignment DP. Non-matching edits now return an error in <100ms instead of hanging for minutes.

### Changed

- **`EditEngine::apply_edits` return type**: Now returns `(EditResult, String)` to provide the new content alongside metadata, eliminating redundant `apply_blocks()` calls that doubled computation in all code paths.

### Removed

- **`strsim` dependency**: No longer needed; fuzzy matching uses inline semi-global alignment DP instead of per-window Levenshtein calls.

## [1.3.1] - 2026-04-05

### Changed

- **BatchOperationSchema**: Removed `BatchOperationInput` wrapper and derived `JsonSchema` directly on `BatchOperation` in `turbovault-batch` to fix MCP schema.
- **batch_execute MCP tool**: Fixed schema generation by using the correctly typed input model (`Vec<BatchOperation>`).

## [1.3.0] - 2026-03-31

### Added

- **`TaskStatus` enum**: Parser now distinguishes `Pending`, `Done`, `InProgress`, and `Cancelled` task states, supporting Obsidian's `[/]` and `[-]` checkbox markers.
- **Path-suffix index**: O(1) resolution of folder-qualified wikilinks like `[[Folder/Note]]`, replacing O(N) full-graph scan.
- **SearchEngine caching**: Tantivy index is now built once per vault and cached, with automatic invalidation on writes. Eliminates full re-index per query.
- **CSV injection protection**: All CSV export functions use RFC 4180 quoting with formula-prefix escaping (`=`, `+`, `-`, `@`).
- **CI security audit**: Added `cargo audit` (rustsec) and MSRV verification (Rust 1.90.0) jobs to CI pipeline.
- **67 new tests**: Comprehensive coverage for graph algorithms, edit engine strategies, batch execution, VaultCache persistence, TaskStatus, csv_escape, and manager file lifecycle operations. Total: 716 tests.

### Fixed

- **Graph `connected_components` used SCC instead of weakly connected**: Replaced Tarjan's SCC with UnionFind-based weakly connected components. Previously, `A→B→C` produced 3 singleton components; now correctly produces 1.
- **`HeadingRef` and `BlockRef` links dropped from graph**: Links like `[[Note#Heading]]` and `[[Note#^blockid]]` are now included in graph edge construction, fixing missing backlinks and broken link detection.
- **Same-document anchors flagged as broken**: `[[#Heading]]` links are now correctly skipped instead of being added to unresolved links.
- **`related_notes` used DFS instead of BFS**: Changed from `Vec::pop()` to `VecDeque::pop_front()` for correct breadth-first traversal.
- **Health score saturation cascade**: Penalties are now computed independently and summed, preventing early floor at 0.
- **Isolated cluster detection**: Uses largest-component exclusion instead of hardcoded `len < 5` threshold.
- **Batch `DeleteNote`/`MoveNote` bypassed VaultManager**: Now routed through `VaultManager::delete_file`/`move_file` for proper audit trails, graph updates, and cache invalidation.
- **`fuzzy_find_whitespace` always returned `None`**: Implemented line-based whitespace-normalized matching (Strategy 2 in the edit cascade).
- **Levenshtein DoS vector**: Added 10M character-comparison budget cap to prevent CPU exhaustion.
- **Temp file leak in `batch_execute`**: Removed `.keep()` call and unused `temp_dir` field from `BatchExecutor`.
- **Temp file orphaned on rename failure**: `write_file` now cleans up the temp file if atomic rename fails.
- **Graph update errors silently swallowed**: All `let _ = graph.*` calls replaced with `log::warn!`.
- **Tag regex rejected digit-first tags**: `#2024` and `#1password` now correctly parsed by both engine and deprecated parsers.
- **Deprecated tag parser matched URL fragments**: Added word-boundary guard to prevent `https://example.com#section` from producing a tag.
- **Deprecated frontmatter regex failed without trailing newline**: Now accepts `(?:\n|$)` at closing `---`.
- **CRLF offset tracking**: Callout parser accounts for `\r\n` line endings.
- **Tantivy field lookups used `.unwrap()`**: Field handles stored as struct fields, eliminating runtime panics.
- **`partial_cmp().unwrap()` on f64 sorts**: Replaced with panic-free `total_cmp()`.
- **Thundering herd on first vault access**: Double-checked locking prevents redundant initialization.

### Changed

- **Weakly connected components algorithm**: `connected_components()` now uses `petgraph::unionfind::UnionFind` for O(V + E·α(V)) performance.
- **`max_hops` capped at 5**: `get_related_notes` tool enforces a maximum traversal depth.
- **`max_vaults` limit**: `MultiVaultManager::add_vault` enforces a 50-vault cap.
- **Docker image pinned**: Builder uses `rust:1.90-bookworm`, removed semantically meaningless `HEALTHCHECK`.
- **docker-compose**: Removed deprecated `version: '3.8'` key.
- **CI**: `--all-features` added to clippy and test steps; `actions/checkout@v4` pinned.
- **Unused dependencies removed**: `nom`, `lazy_static`, `env_logger`, `insta` removed from workspace.
- **Export crate**: Removed phantom `turbovault-vault` dependency.

### Removed

- **`BatchExecutor.temp_dir` field**: Was unused dead code.
- **Hardcoded version in justfile**: Now reads dynamically from `Cargo.toml`.

## [1.2.11] - 2026-03-26

### Added

- **Multi-version MCP protocol support**: TurboVault now accepts clients requesting either MCP `2025-06-18` or `2025-11-25` specification versions. The server negotiates the protocol version during the `initialize` handshake and filters responses through a version adapter that strips fields not present in the older spec (icons, execution, outputSchema, tasks capability). Powered by TurboMCP v3.0.10's `ProtocolConfig::multi_version()`.
- **MCP session lifecycle enforcement**: All line-based transports (STDIO, TCP, Unix) enforce the MCP initialization lifecycle — requests before a successful `initialize` are rejected, and duplicate `initialize` requests are rejected. WebSocket and HTTP transports also enforce per-connection/session version tracking.
- **Auto-create missing vault directories**: `VaultConfig::validate()` and the `add_vault` MCP tool now create missing vault directories with `create_dir_all` instead of returning an error, enabling seamless first-run setup.

### Changed

- **Upgraded TurboMCP to v3.0.10**: Full migration from TurboMCP v3.0.0 to v3.0.10, adopting the `ProtocolVersion` enum, version adapter layer, `route_request_versioned()` API, and builder pattern with `ProtocolConfig::multi_version()` for spec-compliant multi-version response filtering.
- **Server startup uses builder pattern**: Replaced `server.run_stdio()` with `server.builder().with_protocol(ProtocolConfig::multi_version()).serve()` across all transports, enabling runtime protocol configuration.
- **Removed phantom `turbomcp-server` dependency**: The direct `turbomcp-server` dep (which existed only to activate the STDIO feature via default feature resolution) was replaced with an explicit `features = ["stdio", "telemetry"]` on the `turbomcp` dependency, making intent clear and preventing accidental feature loss.

### Fixed

- **Tilde expansion in vault paths**: Vault paths from CLI `--vault` arguments and `VaultConfigBuilder::build()` now expand `~` and `$ENV_VARS` via `shellexpand` before validation. Previously, `--vault ~/work/vault` created a literal `~/work/vault` directory relative to the CWD instead of resolving to the home directory.

## [1.2.9] - 2026-03-22

### Added

- **14 new MCP tools** (44 → 58 total), covering 5 major capability areas:

#### Semantic Similarity Search
- **`semantic_search`**: Find notes by meaning using TF-IDF cosine similarity — discovers conceptual matches beyond exact keyword overlap, with explainable shared-term reporting
- **`find_similar_notes`**: Find notes most similar in content to a given note, useful for discovering link candidates and thematic clusters

#### Content Quality Evaluation
- **`evaluate_note_quality`**: Score individual notes across readability (Flesch-Kincaid), structure (heading hierarchy, frontmatter, tags), completeness (word count, link density), and staleness (modification recency) dimensions
- **`vault_quality_report`**: Vault-wide quality metrics with score distribution, dimension averages, lowest/highest quality notes, and actionable recommendations
- **`find_stale_notes`**: Find notes not updated within a configurable threshold, sorted by staleness

#### Operation Audit Trail & Rollback
- **`audit_log`**: Query operation history with filters by path, operation type (CREATE/UPDATE/DELETE/MOVE), and result limit
- **`rollback_preview`**: Dry-run preview of what a rollback would change, including unified diff
- **`rollback_note`**: Restore a note to its state before a specific operation, with the rollback itself recorded in the audit trail
- **`audit_stats`**: Operation counts by type, total snapshot storage, and time range of recorded operations

#### Duplicate Detection
- **`find_duplicates`**: Two-stage near-duplicate detection using SimHash fingerprinting for fast candidate filtering followed by TF-IDF cosine similarity verification
- **`compare_notes`**: Detailed pairwise comparison with similarity score, shared terms, diff summary, and actionable recommendation (merge/link/keep)

#### Note Diff Tools
- **`diff_notes`**: Line-level and word-level diff between two notes with unified diff output and similarity ratio
- **`diff_note_version`**: Compare current note content with a previous version from the audit trail

- **New `turbovault-audit` crate**: Append-only JSONL operation log, content-addressed snapshot storage (SHA-256 dedup), and rollback engine with atomic file restoration
- **Optimistic concurrency control**: `write_note`, `delete_note`, `move_note`, and `move_file` now accept optional `expected_hash` parameter — if the file was modified since the caller's last `read_note`, the write fails with `ConcurrencyError` instead of silently overwriting. Enables safe multi-agent concurrent vault access.
- **UUID-based temp files**: Concurrent writes to the same file no longer collide on the temp path

### Changed

- **`VaultManager::write_file`** signature now includes `expected_hash: Option<&str>` for optimistic concurrency control. Internal callers pass `None` for backward compatibility.
- **`VaultManager::delete_file`** and **`VaultManager::move_file`** now handle audit trail recording, link graph cleanup, and optimistic concurrency checking — `FileTools` delegates to these instead of performing raw I/O
- **`AuditLog` uses `tokio::sync::Mutex`** for write serialization, preventing interleaved JSONL entries from concurrent MCP tool calls
- **Similarity engine cache invalidation**: All 9 mutating MCP tools (`write_note`, `edit_note`, `delete_note`, `move_note`, `move_file`, `batch_execute`, `update_frontmatter`, `manage_tags`, `create_from_template`) invalidate the cached TF-IDF vectors so subsequent similarity queries reflect current vault state

### Fixed

- **Heading hierarchy validator** now correctly flags documents starting with H2+ (no H1) as invalid, instead of awarding the hierarchy bonus
- **Diff summary accuracy**: `lines_changed` count now reflects the true number of changed line pairs, not the display-capped count (truncation to 50 inline changes only affects the detail list)
- **Staleness penalty integer truncation**: `linked_notes_newer` is now clamped before casting to `u8`, preventing silent wrap-around for hub notes with 256+ newer linked notes
- **`find_duplicates` verification accuracy**: Precise TF-IDF verification now queries the full document set instead of `limit=1`, eliminating false negatives when the candidate pair isn't the single most-similar result

## [1.2.8] - 2026-03-19

### Added

- **`get_notes_info` tool**: Bulk note metadata retrieval — returns `exists`, `size_bytes`, `modified_at`, and `has_frontmatter` for a list of paths without reading full file content, enabling efficient batch filesystem inspection.
- **`write_file_with_mode` tool**: Append and prepend support for file writes. Accepts a `mode` parameter (`overwrite`, `append`, `prepend`) and correctly handles frontmatter boundaries when prepending to YAML-frontmatter files.
- **Cross-filesystem move support**: `move_file` now handles `CrossesDevices` errors by falling back to a copy-then-delete strategy, maintaining atomicity guarantees across filesystem boundaries.
- **`AnalysisConfig` for health analysis**: Configurable `hub_notes_limit` (default 10) replaces the previously hardcoded cap of 5 in `HealthAnalyzer::analyze()`.
- **`LinkGraph::unresolved_link_count()` helper**: Convenience method returning the total count of unresolved links across all source files.

### Changed

- **`write_file` delegates to `write_file_with_mode`**: Existing `write_file` calls are fully backward-compatible and default to `WriteMode::Overwrite`.
- **`resolve_path` is now `pub`**: `VaultManager::resolve_path` is now publicly visible so tool layers (`FileTools`, `DataTools`) can reuse the battle-tested `path_trav`-backed security check without duplicating logic.
- **`read_file` always reads from disk**: Removed the in-memory `VaultFile` cache path from `read_file` — the cache stores parsed content with frontmatter stripped, so bypassing it ensures callers always receive the complete raw file including frontmatter.
- **Updated installation instructions and usage documentation in README**: Clarified TurboVault as both a Rust SDK and an MCP server with two distinct usage modes.
- **Two-pass vault initialization**: `VaultManager::initialize()` now adds all files to the graph index first, then resolves links in a second pass. This eliminates scan-order-dependent resolution failures where files scanned later were not in the index when earlier files resolved links to them.
- **Case-insensitive link resolution**: `LinkGraph::resolve_link` now lowercases all index keys at insertion and lookup time, matching Obsidian's case-insensitive wikilink behaviour.
- **`file_index` and `alias_index` handle stem collisions**: Changed from single-value to multi-value maps (`HashMap<String, Vec<NodeIndex>>`), so files with the same lowercased stem on case-sensitive filesystems are all indexed rather than silently overwriting each other.
- **Health score uses saturating arithmetic**: `HealthReport::calculate_score` now uses `saturating_sub` to prevent `u8` underflow when penalty values are large.
- **Updated TurboMCP to v3.0.6** and all workspace dependencies to latest compatible versions.

### Fixed

- **Broken link detection was non-functional**: `get_broken_links`, `quick_health_check`, and `full_health_analysis` always reported zero broken links because `HealthAnalyzer::new()` (graph-only mode) was used but unresolved links never entered the graph. Unresolved links are now tracked in `LinkGraph.unresolved_links` and wired into `HealthAnalyzer::with_files()`. (PR #6 by @AntttMan)
- **Petgraph swap-remove index corruption**: `remove_file` now correctly updates `path_index`, `file_index`, and `alias_index` after `remove_node`, which uses swap-remove internally and moves the last node into the removed slot. Previously, all external index maps for the swapped node became stale, causing wrong edges, self-loops, or panics on subsequent operations.
- **Path-suffix fallback ignored `.md` extension**: The `resolve_link` path-suffix fallback (for `[[folder/Note]]`-style wikilinks) now strips `.md` from path components before comparison, so multi-segment wikilinks without extensions resolve correctly.
- **Duplicate alias accumulation on re-add**: `add_file` called repeatedly (e.g. on every `write_file`) no longer pushes duplicate entries to `alias_index`.
- **Path traversal protection unified**: `delete_file`, `move_file`, `copy_file`, and `get_notes_info` now all go through `VaultManager::resolve_path` (backed by the `path_trav` crate) instead of ad-hoc `starts_with` checks, closing potential bypass vectors.
- **Stale `#[allow(dead_code)]` annotations**: `is_cache_expired` and `is_file_modified_since` are kept for future use and annotated with `#[allow(dead_code)]` to silence compiler warnings.

## [1.2.7] - 2026-03-04

### Changed

- **Upgraded TurboMCP to v3.0.0**: Full migration to TurboMCP v3 with `TelemetryConfig`-based observability, `#[turbomcp::server]` macro, and `McpHandlerExt` transport abstraction
- **Standardized response serialization**: All tools now use `StandardResponse::to_json()` consistently instead of mixed serialization patterns
- **Removed stale workspace dependencies**: Dropped unused `opentelemetry`, `tracing-opentelemetry`, and `opentelemetry-otlp` workspace deps (v0.28) that were superseded by turbomcp-telemetry (v0.31)

### Added

- **Cross-platform prebuilt binaries**: Release workflow now builds binaries for 7 targets (Linux glibc/musl x86_64/ARM64, macOS x86_64/ARM64, Windows x86_64) with macOS code signing/notarization, SHA256 checksums, and GitHub Releases
- **CI workflow modernized**: Bumped to `actions/checkout@v5`, stable Rust toolchain, `CARGO_TERM_COLOR`

### Fixed

- **Stale cache on external file modifications**: `read_note` now validates cache entries against the file's modification time on disk, so externally modified files (git sync, direct writes, other processes) are always read fresh instead of serving stale/empty cached content (fixes #5)
- **Server version mismatch**: MCP server macro now correctly advertises the current crate version to clients (was hardcoded to 1.1.6)
- **Repository metadata on crates.io**: All 8 workspace crates now set `repository.workspace = true`, so every crate on crates.io links back to the GitHub repo (fixes #4)
- **Removed unused variable** in `explain_vault` tool

### Improved

- **`get_hub_notes` now accepts `top_n` parameter**: Previously hardcoded to 10, now configurable with `top_n: Option<usize>` (default 10)

## [1.2.6] - 2025-12-16

### Added

- **Line offset tracking for inline elements**: `Link` and `Image` variants in `InlineElement` now include optional `line_offset` field that tracks the relative line position within nested list items. This enables precise positioning of inline elements for consumers that need line-level granularity.
- **Comprehensive nested inline element collection**: New `collect_inline_elements()` function recursively traverses nested blocks (paragraphs, lists, blockquotes, details) to gather all inline elements and populate parent list items' inline field. This ensures links and images from all nesting levels are discoverable.
- **Enhanced list parsing for nested items**: Improved handling of nested list structures with proper line offset tracking, indentation preservation, and task checkbox support across all nesting depths.

### Changed

- **List item inline field now complete**: Parent list items' `inline` field now contains links and images from all nested children, enabling comprehensive inline element discovery without manual traversal.

## [1.2.5] - 2025-12-12

### Changed

- **Optimized frontmatter parsing**: Removed redundant regex-based frontmatter extraction in favor of pulldown-cmark's byte offset tracking, eliminating a duplicate parse pass
- **Deprecated `extract_frontmatter`**: Function marked deprecated in favor of `ParseEngine` with `frontmatter_end_offset` for better performance

## [1.2.4] - 2025-12-12

### Added

- **Plain text extraction**: New `to_plain_text()` API for extracting visible text from markdown content, stripping all syntax. Useful for:
  - Search indexing (index only searchable text)
  - Accurate match counts (fixes treemd search mismatch where `[Overview](#overview)` counted URL chars)
  - Word counts
  - Accessibility text extraction
- `InlineElement::to_plain_text(&self) -> &str` - Extract text from inline elements (links return link text, images return alt text)
- `ListItem::to_plain_text(&self) -> String` - Extract text from list items including nested blocks
- `ContentBlock::to_plain_text(&self) -> String` - Extract text from any content block recursively
- `to_plain_text(markdown: &str) -> String` - Standalone function to parse and extract plain text in one call
- Exported `to_plain_text` from `turbovault_parser` crate and prelude
- **Search result metrics**: `SearchResultInfo` now includes `word_count` and `char_count` fields for content size estimation
- **Export readability metrics**: `VaultStatsRecord` now includes `total_words`, `total_readable_chars`, and `avg_words_per_note`

### Changed

- **Search engine uses plain text**: Tantivy index now indexes plain text content instead of raw markdown, improving search relevance
- **Keyword extraction uses plain text**: `find_related()` now extracts keywords from visible text only, excluding URLs and markdown syntax
- **Search previews use plain text**: Search result previews and snippets now show human-readable text without markdown formatting

## [1.2.3] - 2025-12-10

### Fixed

- Updated turbomcp dependency to 2.3.3 for compatibility with latest MCP server framework

## [1.2.2] - 2025-12-09

### Added

- Dependency version bump to turbomcp 2.3.2

### Changed

- Updated all workspace dependencies to latest compatible versions

### Fixed

- Optimized binary search in excluded ranges for improved performance
- Removed unused dependencies to reduce binary size

## [1.2.0] - 2024-12-08

### Added

- **`Anchor` LinkType variant**: Distinguishes same-document anchors (`#section`) from cross-file heading references (`file.md#section`). This is a breaking change for exhaustive match statements on `LinkType`.
- **`BlockRef` detection**: Wikilinks with block references (`[[Note#^blockid]]`) now correctly return `LinkType::BlockRef` instead of `LinkType::HeadingRef`.
- **Block-level parsing**: New `parse_blocks()` function for full markdown AST parsing, including:
  - `ContentBlock` enum: Heading, Paragraph, Code, List, Blockquote, Table, Image, HorizontalRule, Details
  - `InlineElement` enum: Text, Strong, Emphasis, Code, Link, Image, Strikethrough
  - `ListItem` struct with task checkbox support
  - `TableAlignment` enum for table column alignment
- **Shared link utilities**: New `parsers::link_utils` module with `classify_url()` and `classify_wikilink()` functions for consistent link type classification.
- **Re-exported core types from turbovault-parser**: `ContentBlock`, `InlineElement`, `LinkType`, `ListItem`, `TableAlignment`, `LineIndex`, `SourcePosition` are now directly accessible from `turbovault_parser`, eliminating the need for consumers to depend on `turbovault-core` separately.

### Changed

- **Heading anchor generation**: Now uses improved `slugify()` function that properly collapses consecutive hyphens and handles edge cases per Obsidian's behavior.
- **Consolidated duplicate code**: Removed duplicate `classify_url()` implementations from engine.rs and markdown_links.rs in favor of shared utility.

### Fixed

- **Code block awareness**: Patterns inside fenced code blocks, inline code, and HTML blocks are no longer incorrectly extracted as links/tags/embeds.
- **Image parsing in blocks**: Fixed bug where inline images inside paragraphs were causing empty blocks.

## [1.1.8] - 2024-12-07

### Added

- Regression tests for CLI vault deduplication (PR #3)

### Fixed

- Skip CLI vault addition when vault already exists from cache recovery

## [1.1.0] - 2024-12-01

### Added

- Initial public release
- 44 MCP tools for Obsidian vault management
- Multi-vault support with runtime vault addition
- Unified ParseEngine with pulldown-cmark integration
- Link graph analysis with petgraph
- Atomic file operations with rollback support
- Configuration profiles (development, production, readonly, high-performance)

[1.3.0]: https://github.com/epistates/turbovault/compare/v1.2.11...v1.3.0
[1.2.11]: https://github.com/epistates/turbovault/compare/v1.2.10...v1.2.11
[1.2.10]: https://github.com/epistates/turbovault/compare/v1.2.9...v1.2.10
[1.2.9]: https://github.com/epistates/turbovault/compare/v1.2.8...v1.2.9
[1.2.8]: https://github.com/epistates/turbovault/compare/v1.2.7...v1.2.8
[1.2.7]: https://github.com/epistates/turbovault/compare/v1.2.6...v1.2.7
[1.2.6]: https://github.com/epistates/turbovault/compare/v1.2.5...v1.2.6
[1.2.5]: https://github.com/epistates/turbovault/compare/v1.2.4...v1.2.5
[1.2.4]: https://github.com/epistates/turbovault/compare/v1.2.3...v1.2.4
[1.2.3]: https://github.com/epistates/turbovault/compare/v1.2.2...v1.2.3
[1.2.2]: https://github.com/epistates/turbovault/compare/v1.2.1...v1.2.2
[1.2.0]: https://github.com/epistates/turbovault/compare/v1.1.8...v1.2.0
[1.1.8]: https://github.com/epistates/turbovault/compare/v1.1.0...v1.1.8
[1.1.0]: https://github.com/epistates/turbovault/releases/tag/v1.1.0
