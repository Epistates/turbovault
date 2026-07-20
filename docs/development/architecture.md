# Architecture Guide

Overview of TurboVault's modular architecture.

## System Design

```
┌─────────────────────────────────────────┐
│      Claude Desktop / MCP Client        │
└──────────────────┬──────────────────────┘
                   │ MCP Protocol
                   ▼
┌─────────────────────────────────────────┐
│  turbovault-server (MCP Server Binary)  │
│                                         │
│  ┌───────────────────────────────────┐ │
│  │  Flat core + plugin providers     │ │
│  └────────┬───────────────────────────┘ │
└───────────┼─────────────────────────────┘
            │
    ┌───────┴──────────────────┬──────────────────┬──────────┐
    ▼                          ▼                  ▼          ▼
┌────────────┐   ┌──────────────────┐  ┌──────────────┐  ┌───────────┐
│  Parser    │   │  Graph Analysis  │  │  Batch Ops   │  │  Export   │
│ (OFM)      │   │                  │  │              │  │  Utils    │
└────┬───────┘   └────────┬─────────┘  └────────┬─────┘  └────┬──────┘
     │                    │                     │             │
     └────────────────────┼─────────────────────┴─────────────┘
                          ▼
                   ┌─────────────────┐
                   │ Vault Manager   │
                   │ (File I/O)      │
                   └────────┬────────┘
                            │
                            ▼
                   /path/to/vault/*.md
```

## Core Crates

### turbovault (Binary)
- MCP server binary
- CLI interface
- Request routing

### turbovault-core
- Configuration
- Error handling
- Type definitions
- Metrics

### turbovault-parser
- OFM parsing
- Frontmatter extraction
- Metadata validation

### turbovault-graph
- Link graph construction
- Relationship analysis
- Health scoring

### turbovault-vault
- File operations
- Atomic edits
- Real-time watching
- Caching

### turbovault-batch
- Conflict validation for operation batches
- Sequential, fail-fast execution
- Per-file atomic writes (no batch-wide rollback)

### turbovault-export
- JSON/CSV export
- Report generation
- Data serialization

### turbovault-tools
- Reusable implementations behind 74 MCP tools
- Tool implementation
- Response formatting

### turbovault-plugin-api

- Curated `VaultApi` instead of raw manager/server access
- Object-safe compiled-in provider and factory contracts
- Strict plugin tool namespaces
- Bounded best-effort hooks with explicit lag and close states

### MCP provider facade

The `turbovault` crate splits the public catalog across 14 focused provider
modules for context, files, graph, discovery, templates, vault lifecycle,
batch, fanout, export, metadata, relationships, content, analysis, and audit.
TurboMCP's `CompositeHandler` prefixes mounted handlers internally;
TurboVault's facade maps core routes back to the established flat public
names. For example, clients still call `read_note`, not `files_read_note`.
Compiled-in plugins keep their namespace, such as `tasks_list_tasks`.

See [Plugin Architecture](plugins.md) for the host contract, lifecycle, and
contribution rules.

## Data Flow

1. **Claude** sends MCP tool request
2. **turbovault** routes the flat public name to its focused provider
3. **turbovault-tools** processes request
4. Dependencies (parser, graph, vault) execute operation
5. **Response** formatted and returned to Claude

## Performance

- Sub-100ms for most operations
- Parallel file scanning
- In-memory graph caching
- Full-text search indexing
