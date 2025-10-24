# Crate-Specific Documentation

This section provides detailed documentation for each individual crate in the `TurboVault` workspace. Each crate has its own specific purpose, API, and implementation details.

## Crate Overview

The `TurboVault` project is organized as a Rust workspace with 8 crates:

| Crate | Purpose | Lines of Code | Key Features |
|-------|---------|---------------|--------------|
| **turbovault-core** | Core types, errors, config | ~2,000 LOC | Data models, error handling, configuration |
| **turbovault-parser** | OFM parsing (wikilinks, frontmatter) | ~1,500 LOC | Markdown parsing, link extraction |
| **turbovault-graph** | Link graph analysis | ~1,200 LOC | Graph algorithms, health analysis |
| **turbovault-vault** | File I/O, caching, validation | ~1,800 LOC | Vault management, atomic operations |
| **turbovault-batch** | Atomic multi-file operations | ~800 LOC | Transaction support, rollback |
| **turbovault-export** | JSON/CSV export formats | ~600 LOC | Data export, reporting |
| **turbovault-tools** | MCP tools implementation | ~2,500 LOC | 38 MCP tools, search engine |
| **turbovault-server** | CLI + MCP server | ~600 LOC | Main binary, server orchestration |

**Total**: ~11,000 lines of production Rust code

## Individual Crate Documentation

### [turbovault-core](../crates/turbovault-core/README.md)

**Core data models, error types, and configuration for the Obsidian vault management system.**

This crate provides the foundational types and utilities that all other `TurboVault` crates depend on. It defines the canonical data structures, error handling, configuration management, and cross-cutting concerns like metrics and validation.

**Key Components:**
- **Data Models**: `VaultFile`, `Link`, `Tag`, `TaskItem`, `Callout`, `Heading`, `Frontmatter`
- **Error Types**: Unified error system with composable error handling
- **Configuration**: `ServerConfig`, `VaultConfig` with validation
- **Multi-Vault Management**: Coordination between multiple vaults
- **Metrics**: Lightweight metrics collection with zero locking overhead
- **Validation**: Content validation framework with extensible validators
- **Profiles**: Pre-tuned configurations for different deployment scenarios
- **Resilience**: Retry logic, circuit breakers, and error recovery patterns

### [turbovault-parser](../crates/turbovault-parser/README.md)

**Obsidian Flavored Markdown (OFM) parser for extracting structured data from Obsidian vault files.**

This crate provides fast, production-ready parsing of Obsidian markdown files, extracting structured data from various markdown elements.

**Key Components:**
- **Frontmatter Parser**: YAML metadata extraction from `---` delimited blocks
- **Wikilink Parser**: `[[Note]]`, `[[folder/Note#Heading]]`, `[[Note#^block]]` parsing
- **Embed Parser**: `![[Image.png]]`, `![[OtherNote]]` extraction
- **Tag Parser**: `#tag`, `#parent/child` parsing
- **Task Parser**: `- [ ] Todo`, `- [x] Done` extraction
- **Callout Parser**: `> [!NOTE]`, `> [!WARNING]+` parsing
- **Heading Parser**: `# H1` through `###### H6` with anchor generation

**Architecture:**
- Multi-pass approach with zero-allocation where possible
- Built on battle-tested libraries (`pulldown-cmark`, `nom`, `regex`)
- Comprehensive test coverage
- Thread-safe and parallel-safe operations

### [turbovault-graph](../crates/turbovault-graph/README.md)

**Link graph analysis and vault health diagnostics for Obsidian vaults.**

This crate provides comprehensive analysis of the link graph within Obsidian vaults, enabling discovery of relationships, identification of important notes, detection of broken links, and overall vault health assessment.

**Key Components:**
- **Graph Building**: Creates link graph from parsed vault files
- **Backlinks**: Find all files that link TO a specific file
- **Forward Links**: Find all files that a specific file links TO
- **Related Notes**: Co-citation and similarity analysis
- **Cycle Detection**: Find circular references in the link graph
- **Orphan Detection**: Find files with no incoming or outgoing links
- **Connected Components**: Find isolated clusters of notes
- **Health Analysis**: Overall vault health scoring and recommendations
- **Hub Detection**: Identify highly-connected notes
- **Broken Link Detection**: Find links that don't resolve to existing files

**Architecture:**
- Immutable graph built once and never modified
- Uses `petgraph` for efficient graph operations
- Thread-safe read-only operations
- Memory efficient with O(1) lookups via hash maps

### [turbovault-vault](../crates/turbovault-vault/README.md)

**Filesystem API for Obsidian vaults** – file operations, atomic transactions, real-time watching, and caching.

This crate provides the core abstraction for interacting with the Obsidian vault filesystem. It handles all file I/O with atomic operation guarantees, maintains consistency with the parser and graph layers, and provides real-time file watching for synchronization.

**Key Components:**
- **VaultManager**: Primary interface for vault operations
- **AtomicFileOps**: ACID-like transaction support for file operations
- **VaultWatcher**: Real-time filesystem monitoring using `notify` crate
- **File Cache**: TTL-based caching with invalidation on writes
- **Path Security**: Prevents traversal attacks and validates paths
- **Integration**: Coordinates with parser and graph layers

**Architecture:**
- Single source of truth for all file operations
- Thread-safe with `Arc<RwLock<>>` for concurrent access
- Real-time sync through file watcher integration
- Defensive programming with comprehensive error handling

### [turbovault-batch](../crates/turbovault-batch/README.md)

**Atomic, transactional batch file operations for Obsidian vaults.**

This crate provides ACID-like transaction support for multi-file operations, ensuring vault integrity through atomic commits and rollback capabilities. It's designed for complex operations that need to modify multiple files while maintaining consistency.

**Key Components:**
- **BatchTransaction**: Main transaction interface
- **CreateFile**: Create new files with content and frontmatter
- **WriteFile**: Update existing files with new content
- **DeleteFile**: Remove files and update references
- **MoveFile**: Move files to new locations and update references
- **UpdateLinks**: Update links in files to point to new locations
- **Conflict Detection**: Pre-validate operations before execution
- **Rollback Mechanism**: Restore original state on failure

**Architecture:**
- ACID compliance with all operations succeeding or all failing
- Pre-validation to detect conflicts before execution
- Atomic execution with rollback safety
- Thread-safe concurrent transactions

### [turbovault-export](../crates/turbovault-export/README.md)

**Data export functionality for vault analysis in multiple formats.**

This crate provides comprehensive export capabilities for Obsidian vault data, enabling downstream analysis, reporting, and integration with external tools. It supports multiple output formats and export types for different use cases.

**Key Components:**
- **ExportEngine**: Main export interface
- **Format Handlers**: JSON, CSV, Markdown, XML exporters
- **Health Report**: Comprehensive vault health analysis
- **Broken Links Report**: Detailed analysis of broken links
- **Vault Statistics**: Comprehensive vault metrics and analytics
- **Analysis Report**: Detailed vault analysis with insights
- **Streaming Export**: Memory-efficient processing for large vaults

**Architecture:**
- Format-agnostic design supporting multiple output formats
- Data-rich exports with comprehensive vault information
- Performance optimized with efficient streaming
- Extensible design for adding new formats and export types

### [turbovault-tools](../crates/turbovault-tools/README.md)

**MCP Tools Layer** - The bridge between AI agents and Obsidian vault operations.

This crate implements the Model Context Protocol (MCP) tools that enable AI agents to discover, query, analyze, and manage Obsidian vaults through a structured, type-safe API. It orchestrates all vault operations by integrating the parser, graph, vault, batch, and export crates into a cohesive agent-friendly interface.

**Key Components:**
- **38 MCP Tools**: Complete vault management API
- **Search Engine**: Tantivy-powered full-text search with TF-IDF ranking
- **Template Engine**: Pre-built templates with field validation
- **11 Tool Categories**: FileTools, SearchTools, SearchEngine, TemplateEngine, AnalysisTools, GraphTools, BatchTools, ExportTools, ValidationTools, MetadataTools, RelationshipTools, VaultLifecycleTools
- **Agent-Optimized API**: Discoverable tools with clear names and parameters
- **Production-Grade**: OpenTelemetry observability, error handling, performance monitoring

**Architecture:**
- Agent-optimized API surface with discoverable tools
- Production-grade search engine built on Tantivy
- Template system for consistent note creation
- Comprehensive vault analysis and health monitoring

### [turbovault-server](../crates/turbovault-server/README.md)

**Production-Grade MCP Server for Obsidian Vault Management**

The main executable binary that exposes 38 MCP tools for AI agents to autonomously manage Obsidian vaults. This is the entry point for end users - it orchestrates all vault operations by integrating the core, parser, graph, vault, batch, export, and tools crates into a unified Model Context Protocol server.

**Key Components:**
- **CLI Interface**: Command-line argument parsing and configuration
- **MCP Server**: STDIO transport for MCP protocol communication
- **38 MCP Tools**: Complete vault management API
- **Observability**: OpenTelemetry integration for production monitoring
- **Configuration Profiles**: Pre-tuned configurations for different use cases
- **Vault Management**: Multi-vault support with runtime configuration

**Architecture:**
- Main binary that orchestrates all other crates
- STDIO transport for MCP-compliant communication
- Production-ready with comprehensive observability
- Flexible configuration for different deployment scenarios

## Crate Dependencies

```
turbovault-server
├── turbovault-tools
│   ├── turbovault-vault
│   │   ├── turbovault-core
│   │   └── turbovault-parser
│   │       └── turbovault-core
│   ├── turbovault-graph
│   │   └── turbovault-core
│   ├── turbovault-batch
│   │   ├── turbovault-core
│   │   └── turbovault-vault
│   └── turbovault-export
│       ├── turbovault-core
│       ├── turbovault-vault
│       └── turbovault-graph
└── turbovault-core
```

## Development Guidelines

### Adding New Crates

1. **Define Purpose**: Clear, single responsibility
2. **Add to Workspace**: Update root `Cargo.toml`
3. **Create README**: Follow established patterns
4. **Add Dependencies**: Only on `turbovault-core` and stdlib
5. **Implement Tests**: Comprehensive test coverage
6. **Update Documentation**: Add to this index

### Crate Communication

- **Core Types**: All crates use types from `turbovault-core`
- **Error Handling**: Consistent error types across all crates
- **Configuration**: Shared configuration from `turbovault-core`
- **Observability**: OpenTelemetry integration where appropriate

### Testing Strategy

- **Unit Tests**: Each crate has comprehensive unit tests
- **Integration Tests**: Cross-crate integration testing
- **Performance Tests**: Benchmarking for critical paths
- **Documentation Tests**: Examples in documentation are tested

## License

All crates are licensed under the same terms as the `TurboVault` project. See the project root for license information.

## Support

- **Issues**: [GitHub Issues](https://github.com/epistates/TurboVault/issues)
- **Documentation**: This index + individual crate READMEs
- **Examples**: See `tests/` directories in each crate
- **Development**: See main [Development documentation](../development/index.md)
