# TurboVault

[![Crates.io](https://img.shields.io/crates/v/turbovault.svg)](https://crates.io/crates/turbovault)
[![Docs.rs](https://docs.rs/turbovault/badge.svg)](https://docs.rs/turbovault)
[![License](https://img.shields.io/crates/l/turbovault.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)
[![Rust 1.90+](https://img.shields.io/badge/rust-1.90%2B-orange.svg)](https://www.rust-lang.org/)
[![Ask DeepWiki](https://deepwiki.com/badge.svg)](https://deepwiki.com/Epistates/turbovault)

**The ultimate Rust SDK and high-performance MCP server for Obsidian-flavored Markdown (.ofm) and standard .md vaults.**

TurboVault is a dual-purpose toolkit designed for both developers and users. It provides a robust, modular **Rust SDK** for building applications that consume markdown directories, and a **full-featured MCP server** that works out of the box with Claude and other AI agents.

---

## Two Ways to Use TurboVault

### 1. As a Rust SDK (For Developers)
Build your own applications, search engines, or custom MCP servers using our modular crates. TurboVault handles the heavy lifting of parsing `.md` and `.ofm` files, building knowledge graphs, and managing multi-vault environments.

- **Modular Architecture**: Use only what you need (Parser, Graph, Search, etc.).
- **High Performance**: Sub-100ms operations for most tasks.
- **Extensible**: Easily build your own specialized MCP servers on top of our core logic.
- **SOTA Standards**: Fully supports Obsidian-flavored Markdown (wikilinks, embeds, callouts).

### 2. As a Ready-to-Use MCP Server (For Users)
Transform your Obsidian vault into an intelligent knowledge system immediately. Connect TurboVault to Claude Desktop or any MCP-compatible client to gain **47 specialized tools** for your notes.

- **Zero Coding Required**: Install the binary and point it at your vault.
- **47 Specialized Tools**: Searching, link analysis, SQL frontmatter queries, health checks, and more.
- **Multi-Vault Support**: Switch between personal and work notes seamlessly at runtime.

---

## Core Crates (The SDK)

TurboVault is a modular system composed of specialized crates. You can depend on individual components to build your own tools:

| Crate | Purpose | Docs |
|-------|---------|------|
| **[turbovault-core](crates/turbovault-core)** | Core models, MultiVault management & types | [![Docs.rs](https://docs.rs/turbovault-core/badge.svg)](https://docs.rs/turbovault-core) |
| **[turbovault-parser](crates/turbovault-parser)** | High-speed .md & .ofm parser | [![Docs.rs](https://docs.rs/turbovault-parser/badge.svg)](https://docs.rs/turbovault-parser) |
| **[turbovault-graph](crates/turbovault-graph)** | Link graph analysis & relationship discovery | [![Docs.rs](https://docs.rs/turbovault-graph/badge.svg)](https://docs.rs/turbovault-graph) |
| **[turbovault-vault](crates/turbovault-vault)** | Vault management, file I/O & atomic writes | [![Docs.rs](https://docs.rs/turbovault-vault/badge.svg)](https://docs.rs/turbovault-vault) |
| **[turbovault-tools](crates/turbovault-tools)** | 47 MCP tool implementations | [![Docs.rs](https://docs.rs/turbovault-tools/badge.svg)](https://docs.rs/turbovault-tools) |
| **[turbovault-sql](crates/turbovault-sql)** | SQL frontmatter queries (GlueSQL) | [![Docs.rs](https://docs.rs/turbovault-sql/badge.svg)](https://docs.rs/turbovault-sql) |
| **[turbovault-batch](crates/turbovault-batch)** | Atomic batch operations | [![Docs.rs](https://docs.rs/turbovault-batch/badge.svg)](https://docs.rs/turbovault-batch) |
| **[turbovault-export](crates/turbovault-export)** | Export & reporting (JSON/CSV/MD) | [![Docs.rs](https://docs.rs/turbovault-export/badge.svg)](https://docs.rs/turbovault-export) |
| **[turbovault](crates/turbovault)** | Main MCP server binary / SDK orchestrator | [![Docs.rs](https://docs.rs/turbovault/badge.svg)](https://docs.rs/turbovault) |

## Why TurboVault?

Unlike basic note readers, TurboVault understands your vault's **knowledge structure**:

- **Full-text search** across all notes with BM25 ranking
- **Link graph analysis** to discover relationships, hubs, orphans, and cycles
- **Vault intelligence** with health scoring and automated recommendations
- **Atomic batch operations** for safe, transactional multi-file edits
- **Multi-vault support** with instant context switching
- **Runtime vault addition** — no vault required at startup, add them as needed

### Powered by TurboMCP

TurboVault is built on **[TurboMCP](https://github.com/epistates/turbomcp)**, a Rust framework for building production-grade MCP servers. TurboMCP provides:

- **Type-safe tool definitions** — Macro-driven MCP tool implementation
- **Standardized request/response handling** — Consistent envelope format
- **Transport abstraction** — HTTP, WebSocket, TCP, Unix sockets (configurable features)
- **Middleware support** — Logging, metrics, error handling
- **Zero-copy streaming** — Efficient large payload handling

This means TurboVault gets battle-tested reliability and extensibility out of the box. Want to add custom tools? TurboMCP's ergonomic macros make it straightforward.

## Quick Start

### Installation

**From crates.io**

```bash
# Minimal install (7.0 MB, STDIO only - perfect for Claude Desktop)
cargo install turbovault

# With HTTP server (~8.2 MB)
cargo install turbovault --features http

# With all cross-platform transports (~8.8 MB)
# Includes: STDIO, HTTP, WebSocket, TCP (Unix sockets only on Unix/macOS/Linux)
cargo install turbovault --features full

# With SQL frontmatter queries (adds GlueSQL-powered query_frontmatter_sql tool)
cargo install turbovault --features sql

# Binary installed to: ~/.cargo/bin/turbovault
```

**From source:**

```bash
git clone https://github.com/epistates/turbovault.git
cd turbovault
make release
# Binary: ./target/release/turbovault
```

### Option 1: Static Vault (Recommended for Single Vault)

```bash
turbovault --vault /path/to/your/vault --profile production
```

Then add to `~/.config/claude/claude_desktop_config.json`:

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

### Option 2: Runtime Vault Addition (Recommended for Multiple Vaults)

Start the server without a vault:

```bash
turbovault --profile production
```

Then add vaults dynamically:

```json
{
  "mcpServers": {
    "turbovault": {
      "command": "/path/to/turbovault",
      "args": ["--profile", "production"]
    }
  }
}
```

Once connected to Claude:

```
You: "Add my vault at ~/Documents/Notes"
Claude: [Calls add_vault("personal", "~/Documents/Notes")]

You: "Search for machine learning notes"
Claude: [Uses search() across the indexed vault]

You: "What are my most important notes?"
Claude: [Uses get_hub_notes() to find key concepts]
```

## What Can Claude Do?

### Search & Discovery
```
You: "Find all notes about async Rust and show how they connect"
Claude: search() -> recommend_related() -> get_related_notes() -> explain relationships
```

### Vault Intelligence
```
You: "What's the health of my vault? Any issues I should fix?"
Claude: quick_health_check() -> full_health_analysis() -> get_broken_links() -> generate fixes
```

### Knowledge Graph Navigation
```
You: "What are my most important notes? Which ones are isolated?"
Claude: get_hub_notes() -> get_isolated_clusters() -> suggest connections
```

### Structured Note Creation
```
You: "Create a project note for the TurboVault launch with status tracking"
Claude: list_templates() -> create_from_template() -> write auto-formatted note
```

### Batch Content Operations
```
You: "Move my 'MLOps' note to 'AI/Operations' and update all links"
Claude: move_note() + batch operations -> atomic multi-file update
```

### Link Suggestions
```
You: "Based on my vault, what notes should I link this to?"
Claude: suggest_links() -> get_link_strength() -> recommend cross-references
```

## 47 MCP Tools Organized by Category

### File Operations (5)
- `read_note` — Get note content with hash for conflict detection
- `write_note` — Create/overwrite notes (auto-creates directories)
- `edit_note` — Surgical edits via SEARCH/REPLACE blocks
- `delete_note` — Safe deletion with link tracking
- `move_note` — Rename/relocate with automatic wikilink updates

### Link Analysis (6)
- `get_backlinks` — All notes that link TO this note
- `get_forward_links` — All notes this note links TO
- `get_related_notes` — Multi-hop graph traversal (find non-obvious connections)
- `get_hub_notes` — Top 10 most connected notes (key concepts)
- `get_dead_end_notes` — Notes with incoming but no outgoing links
- `get_isolated_clusters` — Disconnected subgraphs in your vault

### Vault Health & Analysis (5)
- `quick_health_check` — Fast 0-100 health score (<100ms)
- `full_health_analysis` — Comprehensive vault audit with recommendations
- `get_broken_links` — All links pointing to non-existent notes
- `detect_cycles` — Circular reference chains (sometimes intentional)
- `explain_vault` — Holistic overview replacing 5+ separate calls

### Full-Text Search (8)
- `search` — BM25-ranked search across all notes (<500ms on 100k notes)
- `advanced_search` — Search with tag, frontmatter, path, and limit filters
- `search_by_frontmatter` — Find notes by frontmatter key-value pair
- `recommend_related` — ML-powered recommendations based on content similarity
- `find_notes_from_template` — Find all notes using a specific template
- `query_metadata` — Frontmatter pattern queries
- `inspect_frontmatter` — Schema inspection for SQL queries (feature: `sql`)
- `query_frontmatter_sql` — Arbitrary SQL against frontmatter via GlueSQL (feature: `sql`)

### Templates (4)
- `list_templates` — Discover available templates
- `get_template` — Template details and required fields
- `create_from_template` — Render and write templated notes
- `get_ofm_examples` — See all Obsidian Flavored Markdown features

### Vault Lifecycle (7)
- `create_vault` — Programmatically create a new vault
- `add_vault` — Register and auto-initialize a vault at runtime
- `remove_vault` — Unregister vault (safe, doesn't delete files)
- `list_vaults` — All registered vaults with status
- `get_vault_config` — Inspect vault settings
- `set_active_vault` — Switch context between multiple vaults
- `get_active_vault` — Current active vault

### Advanced Features (12)
- `batch_execute` — Atomic multi-file operations (all-or-nothing transactions)
- `export_health_report` — Export vault health as JSON/CSV
- `export_broken_links` — Export broken links with fix suggestions
- `export_vault_stats` — Statistics and metrics export
- `export_analysis_report` — Complete audit trail
- `get_metadata_value` — Extract frontmatter values (dot notation support)
- `suggest_links` — AI-powered link suggestions for a note
- `get_link_strength` — Connection strength between notes (0.0–1.0)
- `get_centrality_ranking` — Graph centrality metrics (betweenness, closeness, eigenvector)
- `get_ofm_syntax_guide` — Complete Obsidian Flavored Markdown reference
- `get_ofm_quick_ref` — Quick OFM cheat sheet
- `get_vault_context` — Meta-tool: single call returns vault status, available tools, OFM guide

## Real-World Workflows

### Initialize Without a Vault

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

### Multi-Vault Workflow

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

### Vault Maintenance & Repair

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

### Content Discovery

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

| Profile | Use Case |
|---------|----------|
| `development` | Local dev with verbose logging |
| `production` | Production with security auditing and optimized logging |
| `readonly` | Read-only access for safe exploration |
| `high-performance` | Large vaults (10k+ notes) with aggressive caching |

## SDK and Server Implementation

TurboVault is designed for two primary audiences: developers building on top of the **Rust SDK** and users looking for a **standalone MCP server**.

### As a Standalone MCP Server

The quickest way to get started is using the pre-built binary. It's fully self-contained and optimized for performance:
- **Link-time optimization** (LTO) for maximum speed
- **Configurable transports** (STDIO, HTTP, WebSocket, TCP)
- **Zero external dependencies** (just point it at your vault)

```bash
# Build the optimized binary
cargo build --release --features full

# Run it
./target/release/turbovault --vault /path/to/vault --profile production
```

### As a Rust SDK (Library)

The core of TurboVault is a collection of modular crates. Use them to build your own search engines, knowledge management tools, or even **your own specialized MCP servers**.

```rust
// Use in your own Rust projects
use turbovault_core::MultiVaultManager;
use turbovault_vault::VaultManager;
use turbovault_tools::SearchEngine;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 1. Initialize the MultiVault manager
    let manager = MultiVaultManager::new();
    
    // 2. Add and initialize a vault (scans files, builds graph)
    manager.add_vault("notes", "/home/user/notes").await?;
    
    // 3. Perform high-level operations
    let vault = manager.get_vault("notes")?;
    let results = vault.search("machine learning")?;
    
    // 4. Use these components to build your own custom MCP server
    // or integrate into existing Rust applications.
    Ok(())
}
```

Each crate is published to crates.io, so you can depend on individual components or the full stack.

## Architecture

Built as a modular Rust workspace:

```
turbovault-core        — Core types, MultiVaultManager, configuration
turbovault-parser      — OFM (Obsidian Flavored Markdown) parsing
turbovault-graph       — Link graph analysis with petgraph
turbovault-vault       — Vault operations, file I/O, atomic writes
turbovault-batch       — Transactional batch operations
turbovault-export      — JSON/CSV/Markdown export
turbovault-sql         — SQL frontmatter queries (GlueSQL, feature-gated)
turbovault-tools       — 47 MCP tool implementations
turbovault (binary)    — CLI and MCP server entry point
```

All crates are published to [crates.io](https://crates.io/crates/turbovault-core) for public use.

## Obsidian Flavored Markdown (OFM) Support

TurboVault fully understands Obsidian's syntax:

- **Wikilinks**: `[[note]]`, `[[note|alias]]`, `[[note#section]]`, `[[note#^block]]`
- **Embeds**: `![[image.png]]`, `![[note]]`, `![[note#section]]`
- **Tags**: `#tag`, `#parent/child/tag`
- **Tasks**: `- [ ] Task`, `- [x] Done`
- **Callouts**: `> [!type] Title`
- **Frontmatter**: YAML metadata with automatic parsing
- **Headings**: Hierarchical structure extraction

## Security

- **Path traversal protection** — No access outside vault boundaries
- **Type-safe deserialization** — Rust's type system prevents injection
- **Atomic writes** — Temp file → atomic rename (never corrupts on failure)
- **Hash-based conflict detection** — `edit_note` detects concurrent modifications
- **File size limits** — Default 5MB per file (configurable)
- **No shell execution** — Zero command injection risk
- **Security auditing** — Detailed logs in production mode

## System Requirements

- **Rust**: 1.90.0 or later
- **OS**: Linux, macOS, Windows
- **Memory**: 100MB base + ~80MB per 10k notes
- **Disk**: Negligible (index is in-memory)

## Building from Source

```bash
git clone https://github.com/epistates/turbovault.git
cd turbovault

# Development build
cargo build

# Production build (optimized)
cargo build --release

# Run tests
cargo test --all
```

Or use the Makefile:

```bash
make build       # Debug build
make release     # Production build
make test        # Run tests
make clean       # Clean build artifacts
```

## Documentation

[Docs](./docs/README.md)

## Examples

### Example 1: Search-Driven Organization

```
You: "What topics do I have the most notes on?"
Claude:
  1. get_hub_notes() -> [AI, Project Management, Rust, Python]
  2. For each hub:
     - get_related_notes() -> related topics
     - get_backlinks() -> importance/connectivity
  3. Report: "Your core topics are AI (23 notes) and Rust (18 notes)"
```

### Example 2: Vault Health Improvement

```
You: "My vault feels disorganized. Help me improve it."
Claude:
  1. quick_health_check() -> Health: 42/100
  2. full_health_analysis() -> Issues: 12 broken links, 8 orphaned notes
  3. get_broken_links() -> List of specific broken links
  4. suggest_links() -> AI-powered link recommendations
  5. batch_execute() -> Atomic fixes
  6. explain_vault() -> New health: 78/100
```

### Example 3: Template-Based Content Creation

```
You: "Create project notes for Q4 initiatives"
Claude:
  1. list_templates() -> "project", "task", "meeting"
  2. create_from_template("project", {
       "title": "Q4 Planning",
       "status": "In Progress",
       "deadline": "2024-12-31"
     })
  3. Creates structured note with auto-formatting
  4. Returns path for follow-up edits
```

## Benchmarks

M1 MacBook Pro, 10k notes, production build:

- **File read**: <10ms
- **File write**: <20ms
- **Simple search**: <50ms
- **Graph analysis**: <200ms
- **Vault initialization**: ~500ms
- **Memory usage**: ~80MB

## Roadmap

- [ ] Real-time vault watching (VaultWatcher framework ready)
- [ ] Cross-vault link resolution
- [ ] Encrypted vault support
- [ ] Collaborative locking
- [ ] WebSocket transport (beyond MCP stdio)

## Contributing

Contributions welcome! Please ensure:

- All tests pass: `cargo test --all`
- Code formats: `cargo fmt --all`
- No clippy warnings: `cargo clippy --all -- -D warnings`

## License

MIT License - See [LICENSE](LICENSE) for details

## Links

- **Repository**: https://github.com/epistates/turbovault
- **Issues**: https://github.com/epistates/turbovault/issues
- **MCP Protocol**: https://modelcontextprotocol.io
- **Obsidian**: https://obsidian.md
- **Related**: [TurboMCP](https://github.com/epistates/turbomcp)

---

**Get started now**: `./target/release/turbovault --profile production`
