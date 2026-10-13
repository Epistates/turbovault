# MCP Tools Reference

TurboVault 4.0 serves 52 tools. At runtime `tools/list` is the authoritative catalog, with each tool's input schema, output schema and behavior annotations; [`tool_catalog.json`](../../crates/turbovault/src/tools/providers/tool_catalog.json) is the same catalog checked into the repository.

`tools/list` shows the core tools by default. Tools marked *(advanced)* are callable by name all the same, and listed with `--show-advanced-tools`. Every tool that works inside a vault takes an optional `vault` argument naming the registered vault to run against; without it, the active vault.

Coming from 3.x, [the 4.0 migration guide](../migration/4.0.md) maps every old tool name to its replacement.

## Notes (9)
- `get_vault_context`: Start here: vault status, health, the markdown dialect and the tools by task
- `read_note`: Note content (whole, or by lines and sections) with the hash for conflict detection
- `write_note`: Create, overwrite, append or prepend (auto-creates directories)
- `edit_note`: Surgical edits via SEARCH/REPLACE blocks
- `delete_note`: Delete a note, refusing while other notes link to it (or striking their links through)
- `move_note`: Rename or relocate a note; Git-backed vaults atomically rewrite incoming wikilinks, Markdown links and canvas references
- `get_notes_info`: Existence, size, modified time, chosen frontmatter values and tags for several notes at once
- `move_file` *(advanced)*: Move or rename any file, such as an image or PDF
- `diff_notes` *(advanced)*: Unified diff between two notes, or between a note and an audited version

## Batch & Git Fanout (3)
- `batch_execute`: One all-or-nothing commit with `write_backend: git`; direct stays sequential
- `fanout` *(advanced)*: `begin`, `commit` or `abandon` an isolated Git worktree for long or parallel work
- `list_orphan_fanouts` *(advanced)*: Worktrees left behind by interrupted sessions

## Links & Graph (5)
- `get_links`: A note's backlinks (`incoming`, canvases included) or outgoing links
- `get_related_notes`: Multi-hop graph traversal
- `graph_report`: One list about the graph: `hubs`, `centrality`, `dead_ends`, `isolated_clusters`, `cycles` or `broken_links`
- `vault_health`: 0-100 health score with counts; `depth: full` adds dead-end and hub counts
- `explain_vault`: Holistic overview of structure, health, hubs and next actions

## Search & Similarity (6)
- `search`: BM25 search by keyword, narrowed by tags, frontmatter and folders, or `related_to` a note
- `find_similar`: Notes about a topic or like a given note, by TF-IDF meaning rather than exact words
- `query_metadata`: One frontmatter condition (`equals`, `contains`, `>`, `<`), with each match's frontmatter
- `find_duplicates` *(advanced)*: Near-duplicate pairs across the vault
- `compare_notes` *(advanced)*: How alike two notes are and how strongly the graph links them
- `suggest_links` *(advanced)*: Notes a note could link to, with reasons

## Frontmatter & Tags (4)
- `update_frontmatter`: Patch frontmatter fields (merge or replace)
- `manage_tags`: Add or remove frontmatter tags
- `inspect_frontmatter` *(advanced)*: Frontmatter as SQL columns (feature: `sql`)
- `query_frontmatter_sql` *(advanced)*: Read-only SQL over frontmatter, tags and links via GlueSQL (feature: `sql`)

## Canvases, Bases, Tasks & Periodic Notes (5)
- `read_canvas`: Read a `.canvas` (JSON Canvas 1.0): nodes, edges and the notes it points at
- `write_canvas`: Create or replace a canvas, validated against the spec and written the way Obsidian writes it
- `query_base`: Run a `.base` file's filters, formulas, sort, grouping and limit over the vault's notes
- `query_tasks`: Find tasks by status, due and scheduled dates, tag, folder, priority and text (Tasks plugin emoji and Dataview fields)
- `get_periodic_note`: Find or create the daily, weekly, monthly, quarterly or yearly note, using the vault's Daily notes or Periodic Notes settings

## Quality & Open Knowledge Format (8)
- `validate_vault`: Problems Obsidian would trip on (pre-1.9 `tags`/`aliases`, bad frontmatter, broken links), by note and severity, for one note or the vault
- `note_quality` *(advanced)*: Quality score for one note, or the vault-wide report
- `find_stale_notes` *(advanced)*: Notes not modified within a threshold of days
- `analyze_grounding` *(advanced)*: A note's claims and citations for an external judge, or the vault's uncited notes
- `okf_validate` *(advanced)*: Validate the vault as an [OKF](https://github.com/GoogleCloudPlatform/knowledge-catalog) v0.1 bundle; usable as a CI gate
- `generate_index` *(advanced)*: Generate or refresh `index.md` files for progressive disclosure (idempotent)
- `append_log_entry` *(advanced)*: Append a dated entry to a folder's `log.md` (OKF section 7)
- `visualize` *(advanced)*: Render the graph as a self-contained HTML file

## Templates & OFM (4)
- `list_templates`: Available templates with their fields, or one by `template_id`
- `create_from_template`: Render and write a templated note
- `find_notes_from_template` *(advanced)*: Notes created from a template
- `get_ofm_guide`: The Obsidian Flavored Markdown `quick_ref`, full `syntax` guide or an `example` note

## Vaults (4)
- `list_vaults`: Registered vaults and the active one; with `name`, that vault's full configuration
- `add_vault`: Register a vault folder at runtime, or `create: true` to create it first
- `set_active_vault`: The vault tools use when a call names none
- `remove_vault` *(advanced)*: Unregister a vault (its files are untouched)

## Audit & Export (4)
- `audit_log` *(advanced)*: Change log with operation ids, or `summary: true` for statistics and the hash-chain check
- `rollback_preview` *(advanced)*: What a rollback would change (read-only)
- `rollback_note` *(advanced)*: Undo a change by operation id (audited, itself undoable)
- `export` *(advanced)*: A `health`, `broken_links`, `stats` or `analysis` report as JSON or CSV

## Real-World Workflows

## Initialize Without a Vault

```python
# Server starts with NO vault required
response = client.call("get_vault_context")
# Returns: "No vault registered. Call add_vault() to get started."

response = client.call("add_vault", {
    "name": "personal",
    "path": "~/Documents/Obsidian"
})
# Auto-initializes: scans files, builds link graph, indexes for search
```

## Multi-Vault Workflow

```python
# Add multiple vaults
client.call("add_vault", {"name": "work", "path": "/work/notes"})
client.call("add_vault", {"name": "personal", "path": "~/notes"})

# Switch context instantly
client.call("set_active_vault", {"name": "work"})
search_results = client.call("search", {"query": "Q4 goals"})

client.call("set_active_vault", {"name": "personal"})
recommendations = client.call("recommend_related", {"path": "AI/ML.md"})
```

## Vault Maintenance & Repair

```python
# Quick diagnostic
health = client.call("quick_health_check")
if health["data"]["score"] < 60:
    # Deep analysis if needed
    full_analysis = client.call("full_health_analysis")

# Find and fix issues
broken = client.call("get_broken_links")
# Process broken links...

# Atomic bulk repair
client.call("batch_execute", {
    "operations": [
        {"type": "DeleteNote", "path": "old/deprecated.md"},
        {"type": "MoveNote", "from": "old/notes.md", "to": "new/notes.md"},
        # ... more operations
    ]
})

# Verify improvement
client.call("explain_vault")  # Holistic view
```

## Content Discovery

```python
# Find what matters
hubs = client.call("get_hub_notes")  # Top concepts
orphans = client.call("get_dead_end_notes")  # Incomplete topics

# Deep search
results = client.call("search", {"query": "machine learning"})

# Explore relationships
related = client.call("get_related_notes", {
    "path": "AI/ML.md",
    "max_hops": 3
})

# Get suggestions
suggestions = client.call("suggest_links", {"path": "AI/ML.md"})
```

## Performance Profile

| Operation | Time | Notes |
|-----------|------|-------|
| `read_note` | <10ms | Instant with caching |
| `get_backlinks`, `get_forward_links` | <50ms | Graph lookup |
| `write_note` | <50ms | Includes graph update |
| `search` (10k notes) | <100ms | Tantivy BM25 |
| `quick_health_check` | <100ms | Heuristic score |
| `full_health_analysis` | 1–5s | Exhaustive, use sparingly |
| `explain_vault` | 1–5s | Aggregates 5+ analyses |
| Vault initialization | 100ms–5s | Depends on vault size |

**Key insight**: Fast operations (<100ms) for common tasks, slower operations (1–5s) for exhaustive analysis. Claude uses smart fallbacks.

## Configuration Profiles

`--profile` sets the log level (`RUST_LOG` overrides it) and the settings every vault starts from. An unknown name stops startup rather than falling back.

| Profile | Log level | Also |
|---------|-----------|------|
| `development` (default) | debug | |
| `production` | info | |
| `readonly` (or `read-only`) | warn | Every tool not annotated read-only is hidden and refused, the same gate as `--require-read-only-tools` |
| `high-performance` | warn | |
| `minimal` | error | Doesn't reconcile with edits made outside TurboVault |
| `multi-vault`, `collaboration` | info | |

See the [Quick Start](../getting-started/quick-start.md) for usage examples.
