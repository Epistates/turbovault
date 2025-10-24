# TurboVault Documentation Guide

## Overview

TurboVault includes comprehensive inline documentation for all public APIs, organized into 8 separate Rust crates. Documentation is generated for **docs.rs** and can be built and viewed locally.

## Building Documentation Locally

### View Generated Docs in Browser

```bash
# Build and open documentation in your default browser
cargo doc --no-deps --open
```

This command:
- ✅ Builds documentation for all crates
- ✅ Excludes dependency documentation (faster)
- ✅ Opens `target/doc/turbovault/index.html` in your browser

### Build Without Opening

```bash
cargo doc --no-deps --document-private-items
```

Output will be in `target/doc/` directory.

## Documentation Organization

### Crate Structure

TurboVault is organized into 8 specialized crates:

| Crate | Purpose | Docs |
|-------|---------|------|
| **turbovault** | Main MCP server entry point | Server overview, transport config, tool integration |
| **turbovault-core** | Core types and error handling | Data models, error types, configuration |
| **turbovault-parser** | Markdown parsing | OFM syntax, frontmatter, link extraction |
| **turbovault-graph** | Link graph analysis | Graph operations, health analysis, relationships |
| **turbovault-vault** | Vault file operations | File I/O, watching, atomic operations, edits |
| **turbovault-batch** | Batch transactions | Multi-file operations, conflict detection |
| **turbovault-export** | Data export | Report generation, JSON/CSV export |
| **turbovault-tools** | MCP tool implementations | File tools, search, analysis, validation |

### What You'll Find

Each crate documentation includes:

- **Overview** - Purpose and capabilities
- **Quick Start** - Working code examples
- **Core Concepts** - Key ideas explained
- **Advanced Usage** - Complex scenarios
- **Performance Notes** - Big-O complexity
- **Error Handling** - Common errors
- **Thread Safety** - Concurrency guarantees
- **Module Links** - Submodule navigation

## Using Documentation

### Search for Types/Functions

In the generated documentation:

1. Use the search box (top left)
2. Type a type, function, or module name
3. Results show crate, module, and item

Example searches:
- `VaultManager` - Find vault operations
- `LinkGraph` - Find graph analysis
- `BatchOperation` - Find batch operations
- `LinkType` - Find link type definitions

### Navigate Modules

Click module names to explore:

```
turbovault
├── tools (MCP server tools)
├── prelude (common types)
└── Re-exports from:
    ├── turbovault_core
    ├── turbovault_tools
    ├── turbovault_vault
    ├── turbovault_parser
    ├── turbovault_graph
    ├── turbovault_batch
    └── turbovault_export
```

### Read Examples

Every public module includes examples:

- Look for `//! # Example` sections
- Code blocks show typical usage
- `no_run` blocks indicate they require external setup

## Common Tasks

### Find How to Read a File

```bash
# In docs, search "read_file"
# Navigate to turbovault_vault::manager::VaultManager
# See method documentation and example
```

### Find Graph Analysis

```bash
# Search "LinkGraph"
# Navigate to turbovault_graph
# See quick start example and core concepts
```

### Find Batch Operations

```bash
# Search "BatchOperation"
# Navigate to turbovault_batch
# See operation types and conflict detection
```

### Find Export Formats

```bash
# Search "to_json" or "to_csv"
# Navigate to turbovault_export
# See exporter implementations
```

### Find MCP Tools

```bash
# Search "FileTools" or "GraphTools"
# Navigate to turbovault_tools
# See all available tool implementations
```

## Online Documentation

When published to crates.io, documentation is automatically available at:

### Main Crate
- https://docs.rs/turbovault

### Component Crates
- https://docs.rs/turbovault-core
- https://docs.rs/turbovault-parser
- https://docs.rs/turbovault-graph
- https://docs.rs/turbovault-vault
- https://docs.rs/turbovault-batch
- https://docs.rs/turbovault-export
- https://docs.rs/turbovault-tools

Each crate's page includes:
- ✅ Full API documentation
- ✅ Inline code examples
- ✅ Module structure
- ✅ Type definitions
- ✅ Cross-crate links

## Documentation Quality

### Coverage

- ✅ 8/8 crates fully documented
- ✅ All public APIs have docs
- ✅ Major types have examples
- ✅ Architecture is explained
- ✅ Performance notes included

### Build Status

```bash
# Check build status
cargo doc --no-deps 2>&1 | grep -E "(warning|error)"

# Should output nothing (clean build)
```

## Tips & Tricks

### Keyboard Navigation

In the generated HTML docs:

- `S` - Open search
- `?` - Show keyboard shortcuts
- Arrow keys - Navigate results

### Feature Documentation

Docs are built with all features enabled (`all-features = true`), so you'll see:

- All transport options (HTTP, WebSocket, TCP, Unix)
- Optional dependencies
- Feature-gated code

### Source Code Links

Click `[source]` link next to any item to view the actual source code.

## Troubleshooting

### Docs Won't Build

```bash
# Clean and rebuild
cargo clean
cargo doc --no-deps

# Check Rust version (1.90.0+ required)
rustc --version
```

### Search Not Working

- Make sure JavaScript is enabled in your browser
- Try searching from the docs homepage
- Clear browser cache

### Can't Find Something

- Try searching alternative names
- Check the module structure
- Look at submodules under main module

## Contributing Documentation

To improve documentation:

1. Edit `.rs` files with `//!` doc comments
2. Add examples to doc comments
3. Build locally: `cargo doc --no-deps --open`
4. Submit pull request

## Documentation Standards

When adding new code:

1. **Module-level docs**: Explain what the module does
2. **Type-level docs**: Explain the type's purpose
3. **Function-level docs**: Explain inputs, outputs, errors
4. **Examples**: Show typical usage
5. **Links**: Reference related types/functions

Example format:

```rust
/// Brief description of what this does.
///
/// More detailed explanation if needed.
///
/// # Arguments
///
/// * `param1` - What this parameter does
///
/// # Returns
///
/// What is returned
///
/// # Errors
///
/// What errors can occur
///
/// # Example
///
/// ```
/// // Example code here
/// ```
pub fn my_function(param1: String) -> Result<()> {
    // ...
}
```

## Getting Help

### Documentation Issues

- Check if the docs build locally: `cargo doc --no-deps`
- Search docs.rs for your question
- Check inline examples

### API Questions

- Look for `//!` comments in source code
- Check examples in doc comments
- Review the module documentation

## Next Steps

1. **Read the overview**: Start with `turbovault` crate docs
2. **Pick a domain**: Choose what you want to do
3. **Find the crate**: Locate the relevant crate
4. **Study examples**: Read provided examples
5. **Explore the API**: Click through related types
6. **Try it out**: Build a small test program

## Version Information

- **TurboVault Version**: 0.1.3
- **Rust Edition**: 2021
- **Minimum Rust Version**: 1.90.0
- **Last Updated**: October 24, 2025

---

For the most up-to-date documentation and examples, visit:
- Local: `cargo doc --no-deps --open`
- Online: https://docs.rs/turbovault
