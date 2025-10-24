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
│  │   turbovault-tools (38 MCP Tools) │ │
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
- Multi-file transactions
- Rollback support
- Consistency guarantees

### turbovault-export
- JSON/CSV export
- Report generation
- Data serialization

### turbovault-tools
- 38 MCP tools
- Tool implementation
- Response formatting

## Data Flow

1. **Claude** sends MCP tool request
2. **turbovault-server** routes to appropriate tool
3. **turbovault-tools** processes request
4. Dependencies (parser, graph, vault) execute operation
5. **Response** formatted and returned to Claude

## Performance

- Sub-100ms for most operations
- Parallel file scanning
- In-memory graph caching
- Full-text search indexing
