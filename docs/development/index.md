# Development Guide

Contributing to TurboVault development.

## Building from Source

```bash
# Clone repository
git clone https://github.com/epistates/TurboVault.git
cd TurboVault

# Build debug binary (fast compile, slower runtime)
make build

# Build release binary (slow compile, optimized runtime)
make release

# Run tests
make test

# Run with cargo
cargo run -p turbovault-server -- --vault /path/to/vault --init
```

## Development Workflow

```bash
# Run checks and tests (CI-like)
make dev

# Format code
make fmt

# Run linter
make lint

# Generate documentation
make doc

# Full CI pipeline
make all
```

## Project Requirements

- **Rust**: 1.90.0 or newer
- **Edition**: 2024
- **MSRV Policy**: Latest stable Rust

## Architecture Overview

```
┌─────────────────────────────────────────────────────────┐
│              AI Agent (Claude Desktop)                   │
└───────────────────────────┬─────────────────────────────┘
                            │ MCP Protocol (STDIO)
┌───────────────────────────▼─────────────────────────────┐
│                  mcp-obsidian Binary                     │
│                                                          │
│  ┌────────────────────────────────────────────────┐    │
│  │  CLI (main.rs)                                 │    │
│  │  - Parse args (vault path, profile)            │    │
│  │  - Initialize observability (OTLP)             │    │
│  │  - Create VaultManager                          │    │
│  │  - Start MCP server (STDIO transport)          │    │
│  └────────────────────────────────────────────────┘    │
│                                                          │
│  ┌────────────────────────────────────────────────┐    │
│  │  38 MCP Tools (tools.rs)                       │    │
│  │  - File ops, search, graph, batch, export      │    │
│  │  - Error conversion (Error → McpError)         │    │
│  │  - Observability (spans, metrics)              │    │
│  └────────────────────────────────────────────────┘    │
└───────────────────────────┬─────────────────────────────┘
                            │
        ┌───────────────────┼───────────────────┐
        │                   │                   │
┌───────▼──────┐   ┌───────▼──────┐   ┌───────▼──────┐
│ TurboVault- │   │ TurboVault- │   │  turbomcp    │
│   tools      │   │   vault      │   │  (MCP SDK)   │
│              │   │              │   │              │
│ - FileTools  │   │ VaultManager │   │ - Protocol   │
│ - SearchEng  │   │ - File I/O   │   │ - Transport  │
│ - Templates  │   │ - Caching    │   │ - Macros     │
│ - GraphTools │   │ - Validation │   │ - Observ.    │
└──────────────┘   └──────────────┘   └──────────────┘
        │                   │
        └───────────────────┼───────────────────┐
                            │                   │
                  ┌─────────▼─────────┐ ┌───────▼──────┐
                  │ turbovault-parser │ │ TurboVault- │
                  │                    │ │   graph      │
                  │ - OFM parsing      │ │              │
                  │ - Wikilink extract │ │ - Link graph │
                  │ - Frontmatter      │ │ - Analysis   │
                  └────────────────────┘ └──────────────┘
```

## Crate Structure

The project is organized as a Rust workspace with 8 crates:

| Crate | Purpose | Lines of Code |
|-------|---------|---------------|
| **turbovault-core** | Core types, errors, config | ~2,000 LOC |
| **turbovault-parser** | OFM parsing (wikilinks, frontmatter) | ~1,500 LOC |
| **turbovault-graph** | Link graph analysis | ~1,200 LOC |
| **turbovault-vault** | File I/O, caching, validation | ~1,800 LOC |
| **turbovault-batch** | Atomic multi-file operations | ~800 LOC |
| **turbovault-export** | JSON/CSV export formats | ~600 LOC |
| **turbovault-tools** | MCP tools implementation | ~2,500 LOC |
| **turbovault-server** | CLI + MCP server | ~600 LOC |

**Total**: ~11,000 lines of production Rust code

## Adding a New Tool

1. **Implement in `tools.rs`**:

   ```rust
   #[tool("my_new_tool")]
   async fn my_new_tool(&self, param: String) -> McpResult<String> {
       let manager = self.get_manager().await?;
       let tools = MyTools::new(manager);
       let result = tools.my_operation(&param).await.map_err(to_mcp_error)?;
       Ok(result)
   }
   ```

2. **Add to turbovault-tools** (if reusable logic):

   ```rust
   // crates/turbovault-tools/src/my_tools.rs
   pub struct MyTools {
       pub manager: Arc<VaultManager>,
   }

   impl MyTools {
       pub async fn my_operation(&self, param: &str) -> Result<String> {
           // Implementation
       }
   }
   ```

3. **Write tests**:

   ```rust
   #[tokio::test]
   async fn test_my_new_tool() {
       let (_temp, manager) = create_test_vault().await;
       let server = ObsidianMcpServer::new();
       server.initialize(manager).await;

       // Test tool invocation
       let result = server.my_new_tool("test".to_string()).await;
       assert!(result.is_ok());
   }
   ```

4. **Update documentation** in this README

## Running Tests

```bash
# All server tests
cargo test -p turbovault-server

# Specific test
cargo test -p turbovault-server test_server_initialization

# With output
cargo test -p turbovault-server -- --nocapture

# Integration tests only
cargo test -p turbovault-server --test integration_test
```

## Code Quality

```bash
# Format code
cargo fmt --all

# Run linter
cargo clippy --all -- -D warnings

# Check compilation without building
cargo check -p turbovault-server
```

## Contributing

See individual crate READMEs for detailed development guides:
- [turbovault-server](crates/turbovault-server/README.md) - Adding new tools
- [turbovault-tools](crates/turbovault-tools/README.md) - Tool implementation patterns
- [turbovault-parser](crates/turbovault-parser/README.md) - OFM parsing
- [turbovault-graph](crates/turbovault-graph/README.md) - Graph algorithms

## Design Principles

- **No mocks, no placeholders** – All file operations are real
- **Production-ready** – Comprehensive error handling, security by design
- **Test-driven** – Tests use real temporary filesystems (tempfile crate)
- **Container-first** – Development via Docker (see workspace Makefile)
- **Type-safe** – Strong types prevent string-based API errors
- **Zero-panic libraries** – All errors are `Result<T, Error>`
