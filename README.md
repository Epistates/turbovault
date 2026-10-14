# TurboVault

[![Crates.io](https://img.shields.io/crates/v/turbovault.svg)](https://crates.io/crates/turbovault)
[![Docs.rs](https://docs.rs/turbovault/badge.svg)](https://docs.rs/turbovault)
[![License](https://img.shields.io/crates/l/turbovault.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)
[![Rust 1.90+](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/Epistates/turbovault)

- MCP Registry name: `mcp-name: io.github.epistates/turbovault`

TurboVault is an MCP server that gives AI agents safe, fast access to Obsidian vaults, and a set of Rust crates for working with Obsidian Flavored Markdown. It's a single binary with no runtime dependencies.

- **Writes you can trust.** Every change goes through one write path: atomic renames with hash preconditions on a plain folder, or one Git commit per write (or per batch) on a Git-backed vault. Moving a note rewrites the wikilinks, Markdown links, reference links and canvas cards that point at it in the same change.
- **An index that can't miss an edit.** Before serving search, graph or similarity results, TurboVault compares the vault against what it last indexed and applies what changed, so edits from Obsidian, a sync client or `git pull` are always seen, even on network and cloud-synced vaults where file watchers report nothing.
- **Knowledge-aware tools.** Full-text search, link graph analysis, similarity, health scoring, Obsidian Bases, canvases, tasks and periodic notes, plus SQL over frontmatter.
- **Built for agents.** Every tool has parameter descriptions, annotations, a typed output schema, paged results and errors that say what to do next. Notes are MCP resources you can subscribe to, and destructive calls ask the user first when the client supports it.
- **Safe by default.** Vault registration is confined, protected folders (`.obsidian`, `.git`) are unreachable, and network transports require authentication.

## Install

**Prebuilt binaries.** Each [GitHub release](https://github.com/Epistates/turbovault/releases) has archives for Linux (x86_64 and aarch64, glibc and musl), macOS (Intel and Apple silicon) and Windows (x86_64), built with every feature. Each archive has a `.sha256` file and a signed build provenance attestation:

```bash
gh attestation verify turbovault-x86_64-unknown-linux-gnu.tar.gz --repo Epistates/turbovault
```

**From crates.io:**

```bash
cargo install turbovault                       # stdio only, all you need for Claude Desktop and Claude Code
cargo install turbovault --features release    # what the prebuilt binaries have
```

Other features: `http`, `websocket`, `tcp`, `full` (all cross-platform transports), `unix`, `sql` (SQL over frontmatter) and `vector-search` (the vector search plugin).

**Docker:** `ghcr.io/epistates/turbovault:<version>` (amd64 and arm64). Mount the vault at `/var/obsidian-vault`:

```bash
docker run -i --rm -v "$HOME/Documents/Vault:/var/obsidian-vault" ghcr.io/epistates/turbovault:4.0.0
```

**MCP Registry:** listed as `io.github.epistates/turbovault`, with cargo and OCI packages.

## Quick start

**Claude Code:**

```bash
claude mcp add turbovault -- turbovault --vault ~/Documents/Vault
```

**Claude Desktop** (`~/Library/Application Support/Claude/claude_desktop_config.json` on macOS, `%APPDATA%\Claude\claude_desktop_config.json` on Windows):

```json
{
  "mcpServers": {
    "turbovault": {
      "command": "/path/to/turbovault",
      "args": ["--vault", "/path/to/your/vault", "--profile", "production"]
    }
  }
}
```

Leave out `--vault` to start with no vault and let the agent register one with `add_vault`. Runtime-registered vaults are remembered across restarts. For several vaults, a Git backend or per-vault settings, use a config file: see [`config.example.yaml`](config.example.yaml) and the [configuration guide](docs/configuration/index.md).

A good first call is `get_vault_context`: it returns the registered vaults, the active vault's counts and health, and the tools grouped by task.

## Tools

`tools/list` shows the core tools by default. The ones marked *(advanced)* are callable by name all the same, and listed with `--show-advanced-tools` (see the [configuration guide](docs/configuration/index.md#tool-visibility)). Every tool that works inside a vault takes an optional `vault` argument naming the registered vault to run against; without it, the active vault. Coming from 3.x, [docs/migration/4.0.md](docs/migration/4.0.md) maps every old tool name to its replacement.

### Notes (9)
- `get_vault_context`: Start here: vault status, health, the markdown dialect and the tools by task
- `read_note`: Note content (whole, or by lines and sections) with the hash for conflict detection
- `write_note`: Create, overwrite, append or prepend (auto-creates directories)
- `edit_note`: Surgical edits via SEARCH/REPLACE blocks
- `delete_note`: Delete a note, refusing while other notes link to it (or striking their links through)
- `move_note`: Rename or relocate a note; Git-backed vaults atomically rewrite incoming wikilinks, Markdown links and canvas references
- `get_notes_info`: Existence, size, modified time, chosen frontmatter values and tags for several notes at once
- `move_file` *(advanced)*: Move or rename any file, such as an image or PDF
- `diff_notes` *(advanced)*: Unified diff between two notes, or between a note and an audited version

### Batch & Git Fanout (3)
- `batch_execute`: One all-or-nothing commit with `write_backend: git`; direct stays sequential
- `fanout` *(advanced)*: `begin`, `commit` or `abandon` an isolated Git worktree for long or parallel work
- `list_orphan_fanouts` *(advanced)*: Worktrees left behind by interrupted sessions

### Links & Graph (5)
- `get_links`: A note's backlinks (`incoming`, canvases included) or outgoing links
- `get_related_notes`: Multi-hop graph traversal
- `graph_report`: One list about the graph: `hubs`, `centrality`, `dead_ends`, `isolated_clusters`, `cycles` or `broken_links`
- `vault_health`: 0-100 health score with counts; `depth: full` adds dead-end and hub counts
- `explain_vault`: Holistic overview of structure, health, hubs and next actions

### Search & Similarity (6)
- `search`: BM25 search by keyword, narrowed by tags, frontmatter and folders, or `related_to` a note
- `find_similar`: Notes about a topic or like a given note, by TF-IDF meaning rather than exact words
- `query_metadata`: One frontmatter condition (`equals`, `contains`, `>`, `<`), with each match's frontmatter
- `find_duplicates` *(advanced)*: Near-duplicate pairs across the vault
- `compare_notes` *(advanced)*: How alike two notes are and how strongly the graph links them
- `suggest_links` *(advanced)*: Notes a note could link to, with reasons

### Frontmatter & Tags (4)
- `update_frontmatter`: Patch frontmatter fields (merge or replace)
- `manage_tags`: Add or remove frontmatter tags
- `inspect_frontmatter` *(advanced)*: Frontmatter as SQL columns (feature: `sql`)
- `query_frontmatter_sql` *(advanced)*: Read-only SQL over frontmatter, tags and links via GlueSQL (feature: `sql`)

### Canvases, Bases, Tasks & Periodic Notes (5)
- `read_canvas`: Read a `.canvas` (JSON Canvas 1.0): nodes, edges and the notes it points at
- `write_canvas`: Create or replace a canvas, validated against the spec and written the way Obsidian writes it
- `query_base`: Run a `.base` file's filters, formulas, sort, grouping and limit over the vault's notes
- `query_tasks`: Find tasks by status, due and scheduled dates, tag, folder, priority and text (Tasks plugin emoji and Dataview fields)
- `get_periodic_note`: Find or create the daily, weekly, monthly, quarterly or yearly note, using the vault's Daily notes or Periodic Notes settings

### Quality & Open Knowledge Format (8)
- `validate_vault`: Problems Obsidian would trip on (pre-1.9 `tags`/`aliases`, bad frontmatter, broken links), by note and severity, for one note or the vault
- `note_quality` *(advanced)*: Quality score for one note, or the vault-wide report
- `find_stale_notes` *(advanced)*: Notes not modified within a threshold of days
- `analyze_grounding` *(advanced)*: A note's claims and citations for an external judge, or the vault's uncited notes
- `okf_validate` *(advanced)*: Validate the vault as an [OKF](https://github.com/GoogleCloudPlatform/knowledge-catalog) v0.1 bundle; usable as a CI gate
- `generate_index` *(advanced)*: Generate or refresh `index.md` files for progressive disclosure (idempotent)
- `append_log_entry` *(advanced)*: Append a dated entry to a folder's `log.md` (OKF section 7)
- `visualize` *(advanced)*: Render the graph as a self-contained HTML file

### Templates & OFM (4)
- `list_templates`: Available templates with their fields, or one by `template_id`
- `create_from_template`: Render and write a templated note
- `find_notes_from_template` *(advanced)*: Notes created from a template
- `get_ofm_guide`: The Obsidian Flavored Markdown `quick_ref`, full `syntax` guide or an `example` note

### Vaults (4)
- `list_vaults`: Registered vaults and the active one; with `name`, that vault's full configuration
- `add_vault`: Register a vault folder at runtime, or `create: true` to create it first
- `set_active_vault`: The vault tools use when a call names none
- `remove_vault` *(advanced)*: Unregister a vault (its files are untouched)

### Audit & Export (4)
- `audit_log` *(advanced)*: Change log with operation ids, or `summary: true` for statistics and the hash-chain check
- `rollback_preview` *(advanced)*: What a rollback would change (read-only)
- `rollback_note` *(advanced)*: Undo a change by operation id (audited, itself undoable)
- `export` *(advanced)*: A `health`, `broken_links`, `stats` or `analysis` report as JSON or CSV

## Resources, prompts and confirmation

- **Notes as resources.** Every note is `obsidian://note/{vault}/{path}`: listed, read as markdown, and subscribable. Subscribers hear when a note changes, whether TurboVault or another editor changed it.
- **Prompts.** `triage_vault`, `repair_broken_links`, `review_period`, `summarize_note` and `ground_draft`, with completion for their arguments.
- **Progress and cancellation.** Slow tools report progress, and a cancelled call stops before it writes anything.
- **Confirmation.** `delete_note`, a `move_note` that replaces a note and a forced `rollback_note` ask the user to confirm when the client supports elicitation. `--no-confirm-destructive` turns this off.

## Writes and Git

On the default (direct) backend, each write is a temp file and an atomic rename, held to the note's `expected_hash` so an edit made since the last read is never lost. A batch applies in order and reports exactly what landed if it stops partway.

On a Git-backed vault (`write_backend: git`), every write is one commit and every batch is one all-or-nothing commit, with compare-and-swap on the branch so concurrent processes can't interleave. TurboVault refuses to overwrite files with uncommitted changes, and if writing out a commit is interrupted, the next write or the next start finishes it. `fanout` gives an agent a private worktree for long or parallel work, merged back or thrown away as a unit.

Every change is recorded in a hash-chained audit trail under `.turbovault/` (or as Git history), and `rollback_note` undoes one, refusing to discard a later edit unless forced.

## Security

- **Vault registration is confined.** `add_vault` never accepts a filesystem root, the home directory, hidden or application-data directories under it, or system directories. `--allowed-vault-roots` limits it to chosen directories; on network transports it's off unless roots are set.
- **Protected folders.** `.obsidian`, `.git`, `node_modules` and TurboVault's own `.turbovault` are unreachable through the note tools on both backends. A vault can protect more folders, never fewer.
- **Network transports.** HTTP takes bearer tokens or OAuth JWTs (`--auth-mode`), with per-client rate limiting, and won't listen beyond loopback without authentication. WebSocket and TCP stay on loopback. The Unix socket lives in a private directory.
- **Read-only.** `--profile read-only` refuses every mutating tool; `allowed_operations: read` makes one vault read-only.
- **Bounds.** Files over 10 MiB (configurable) aren't read, written or scanned; every result count is capped; `query_frontmatter_sql` runs only `SELECT`, for at most 10 seconds and 1000 rows.
- No shell execution anywhere.

Report vulnerabilities through [GitHub private vulnerability reporting](SECURITY.md).

## Observability

Logs go to stderr as JSON on stdio (`RUST_LOG` overrides the profile's level). `--otlp-endpoint` exports traces over OTLP, `--metrics-port` serves Prometheus metrics on 127.0.0.1, and every tool call is a span with the tool, vault, outcome and duration, never its arguments or note content.

## Performance

Measured on synthetic vaults with a release build over MCP stdio:

| | 10k notes | 50k notes |
|---|---|---|
| Startup | 0.5 s | 2.5 s |
| Memory after startup | 213 MB | 0.93 GB |
| `read_note` | 0.5 ms | 0.9 ms |
| `search` (warm) | 3.5-8 ms | 3.5 ms |
| Search after a restart | 61 ms | 258 ms |
| `vault_health` | 1 ms | 16 ms |
| `explain_vault` | 30 ms | 179 ms |
| `write_note` | 14-31 ms | |

The search index persists in TurboVault's cache directory, so a restart re-indexes only notes that changed.

## Obsidian Flavored Markdown

Wikilinks (`[[note]]`, `[[note|alias]]`, `[[note#heading]]`, `[[note#^block]]`, and the table-escaped `\|`), embeds, Markdown links (inline, reference-style and percent-encoded), tags (`#tag`, `#parent/child`), tasks (including Tasks plugin emoji and Dataview fields), callouts, YAML frontmatter as Obsidian 1.9 reads it, properties with links, headings, JSON Canvas 1.0 and Bases.

## Use the crates

| Crate | Purpose |
|---|---|
| [`turbovault-core`](crates/turbovault-core) | Models, configuration, errors, multi-vault management |
| [`turbovault-parser`](crates/turbovault-parser) | Obsidian Flavored Markdown and JSON Canvas parsing |
| [`turbovault-graph`](crates/turbovault-graph) | Link graph, health and centrality |
| [`turbovault-vault`](crates/turbovault-vault) | `VaultManager`: the note cache, the freshness gate and the write path |
| [`turbovault-git`](crates/turbovault-git) | The Git write substrate |
| [`turbovault-audit`](crates/turbovault-audit) | Audit trail, snapshots, rollback |
| [`turbovault-batch`](crates/turbovault-batch) | Batch operation types and validation |
| [`turbovault-export`](crates/turbovault-export) | JSON and CSV reports |
| [`turbovault-sql`](crates/turbovault-sql) | SQL over frontmatter (GlueSQL) |
| [`turbovault-tools`](crates/turbovault-tools) | The logic behind the MCP tools |
| [`turbovault-plugin-api`](crates/turbovault-plugin-api) | The contract for compiled-in plugins |
| [`turbovault-vector`](crates/turbovault-vector) | Hybrid dense and BM25 search |
| [`turbovault`](crates/turbovault) | The MCP server |

## Build from source

```bash
git clone https://github.com/epistates/turbovault.git
cd turbovault
cargo build --release          # ./target/release/turbovault
cargo test --workspace --all-features
```

Rust 1.90 builds the crates; the test suite needs 1.93.1 or later. See [CONTRIBUTING.md](CONTRIBUTING.md) and the [development guide](docs/development/index.md).

## Migrating from 3.x

4.0 consolidates 79 tools into 52, adds the per-call `vault` argument and hides the advanced tools from `tools/list` by default. [docs/migration/4.0.md](docs/migration/4.0.md) maps every old tool and parameter to its replacement, and the [changelog](CHANGELOG.md) lists every library API change.

## License

MIT. See [LICENSE](LICENSE).

## Links

- [Documentation](docs/README.md)
- [Changelog](CHANGELOG.md)
- [Issues](https://github.com/epistates/turbovault/issues)
- [TurboMCP](https://github.com/epistates/turbomcp), the framework TurboVault is built on
- [Model Context Protocol](https://modelcontextprotocol.io)
