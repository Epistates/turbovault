# turbovault-core

[![Crates.io](https://img.shields.io/crates/v/turbovault-core.svg)](https://crates.io/crates/turbovault-core)
[![Docs.rs](https://docs.rs/turbovault-core/badge.svg)](https://docs.rs/turbovault-core)
[![License](https://img.shields.io/crates/l/turbovault-core.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Core data models, error types, and configuration for the Obsidian vault management system.

This crate provides the foundational types and utilities that all other TurboVault crates depend on. It defines the canonical data structures, error handling, configuration management, and cross-cutting concerns like metrics and validation.

## Purpose

turbovault-core is the foundation layer that:
- Defines all core data models (vault files, links, headings, tags, etc.)
- Provides a unified error type system for composable error handling
- Manages server and vault configuration with validation
- Implements multi-vault coordination and lifecycle management
- Offers lightweight metrics collection with zero locking overhead
- Provides content validation framework with extensible validators
- Defines configuration profiles for different deployment scenarios
- Implements resilience patterns (retry logic, circuit breakers)

## Key Components

### Data Models (`models.rs`)

Rich, serializable types representing Obsidian vault elements:

```rust
use TurboVault_core::prelude::*;

// Core vault file with parsed content
let file = VaultFile::new(path, content, metadata);

// Structured link types
let link = Link::new(LinkType::WikiLink, source_file, target, position);

// Parse results include headings, tags, callouts, tasks, etc.
assert_eq!(file.headings.len(), 3);
assert!(file.has_tag("rust"));
```

**Types:**
- `VaultFile` - Complete parsed markdown file with all elements
- `Link` - Links with type classification (see LinkType below)
- `Heading` - Hierarchical headings with anchors
- `Tag` - Inline and frontmatter tags
- `TaskItem` - Checkboxes with completion status
- `Callout` - Obsidian callout blocks (note, warning, tip, etc.)
- `Block` - Generic content blocks with IDs
- `Frontmatter` - YAML metadata
- `FileMetadata` - File system metadata (size, timestamps, checksum)
- `ContentBlock` - Block-level AST (Heading, Paragraph, Code, List, Table, etc.)
- `InlineElement` - Inline formatting (Strong, Emphasis, Code, Link, Image)

**LinkType variants:**
- `WikiLink` - Basic wikilink: `[[Note]]`
- `Embed` - Embedded content: `![[Note]]`
- `BlockRef` - Block reference: `[[Note#^blockid]]` or `#^blockid`
- `HeadingRef` - Cross-file heading: `[[Note#Heading]]` or `file.md#section`
- `Anchor` - Same-document anchor: `[[#Heading]]` or `#section`
- `MarkdownLink` - Markdown link to file: `[text](./file.md)`
- `ExternalLink` - External URL: `[text](https://...)`

### Error Handling (`error.rs`)

Unified error type for the entire system:

```rust
use TurboVault_core::{Error, Result};

fn read_file(path: &Path) -> Result<String> {
    if !path.exists() {
        return Err(Error::file_not_found(path));
    }

    let content = std::fs::read_to_string(path)?;

    if content.len() > max_size {
        return Err(Error::file_too_large(path, content.len() as u64, max_size));
    }

    Ok(content)
}
```

**Error Categories:**
- `Io` - File system errors (auto-converted from `std::io::Error`)
- `FileNotFound` - Missing files with path context
- `InvalidPath` - Path validation failures
- `PathTraversalAttempt` - Security violations
- `FileTooLarge` - Size limit violations
- `ParseError` - Content parsing failures
- `ConfigError` - Configuration validation issues
- `ValidationError` - Content validation failures
- `ConcurrencyError` - Race condition detection
- `NotFound` - Missing graph entries

### Configuration (`config.rs`)

Builder-based configuration with validation:

```rust
use TurboVault_core::config::*;

// Single vault configuration
let vault = VaultConfig::builder("main", "/path/to/vault")
    .as_default()
    .watch_for_changes(true)
    .build()?;

// Server-wide configuration
let mut server_config = ServerConfig::new();
server_config.vaults.push(vault);
server_config.max_file_size = 20 * 1024 * 1024; // 20MB
server_config.validate()?;
```

**Features:**
- `VaultConfig` - Per-vault settings with optional overrides
- `ServerConfig` - Global server settings with defaults
- Builder pattern for ergonomic construction
- Automatic validation on build
- Persistence to/from YAML

### Multi-Vault Management (`multi_vault.rs`)

Coordinate multiple Obsidian vaults simultaneously:

```rust
use TurboVault_core::MultiVaultManager;

let manager = MultiVaultManager::new(server_config)?;

// Add new vault
manager.add_vault(new_vault_config).await?;

// Switch active vault
manager.set_active_vault("work").await?;

// List all vaults
let vaults = manager.list_vaults().await?;
for vault_info in vaults {
    println!("{} - {}", vault_info.name, vault_info.path.display());
}
```

**Features:**
- Vault isolation and independent lifecycle
- Default vault concept with automatic fallback
- Setting inheritance with per-vault overrides
- Async-safe with RwLock coordination
- Clone-safe for sharing across threads

### Metrics (`metrics.rs`)

Lock-free metrics infrastructure for high-performance observability:

```rust
use TurboVault_core::metrics::*;

// Lock-free counter (atomic operations)
let counter = Counter::new("requests");
counter.increment();
counter.add(5);
assert_eq!(counter.value(), 6);

// Histogram for distributions
let histogram = Histogram::new("latency_ms");
histogram.record(42.5);

// RAII timer for automatic duration recording
{
    let _timer = histogram.timer();
    // ... operation ...
} // Duration automatically recorded on drop
```

**Features:**
- `Counter` - Monotonically increasing atomic values
- `Histogram` - Distribution tracking with statistics
- `HistogramTimer` - RAII timer for automatic recording
- `MetricsContext` - Global registry (rarely used)
- Zero locking overhead
- Saturating arithmetic (no overflow panics)

### Validation (`validation.rs`)

Extensible content validation framework:

```rust
use TurboVault_core::validation::*;

// Create validators
let validator = CompositeValidator::new()
    .add_validator(Box::new(
        FrontmatterValidator::new().require_field("title")
    ))
    .add_validator(Box::new(LinkValidator::new()))
    .add_validator(Box::new(
        ContentValidator::new()
            .min_length(100)
            .require_heading()
    ));

// Validate a file
let report = validator.validate(&file);

if !report.passed {
    for issue in report.issues {
        println!("[{}] {}: {}",
            issue.severity,
            issue.category,
            issue.message
        );
        if let Some(suggestion) = issue.suggestion {
            println!("  Suggestion: {}", suggestion);
        }
    }
}
```

**Validators:**
- `FrontmatterValidator` - Required fields, tag format validation
- `LinkValidator` - Empty targets, suspicious URLs, fragment-only links
- `ContentValidator` - Length checks, required headings
- `CompositeValidator` - Run multiple validators

**Severity Levels:**
- `Info` - Informational messages
- `Warning` - Should be addressed but not critical
- `Error` - Should be fixed (fails validation)
- `Critical` - Must be fixed (fails validation)

### Configuration Profiles (`profiles.rs`)

Pre-configured deployments for common use cases:

```rust
use TurboVault_core::profiles::ConfigProfile;

// Create profile-based configuration
let config = ConfigProfile::Production.create_config();

// Or get recommendation based on vault size
let profile = ConfigProfile::recommend(vault_size);
let config = profile.create_config();
```

**Available Profiles:**
- `Development` - Verbose logging, all features enabled, metrics on
- `Production` - Optimized for reliability and security
- `ReadOnly` - Search/analysis only, no write operations
- `HighPerformance` - Tuned for large vaults (5000+ files)
- `Minimal` - Bare essentials only
- `MultiVault` - Multiple vault support with isolation
- `Collaboration` - Team features, webhooks, exports

### Resilience (`resilience.rs`)

Patterns for graceful error handling:

```rust
use TurboVault_core::resilience::*;

// Retry with exponential backoff
let config = RetryConfig::conservative();
let result = retry_with_backoff(&config, || {
    Box::pin(async {
        vault_operation().await
    })
}).await?;

// Circuit breaker for preventing cascading failures
let circuit_breaker = CircuitBreaker::new(
    3,  // failure threshold
    2,  // success threshold
    Duration::from_secs(30)  // timeout
);

if circuit_breaker.is_request_allowed() {
    match operation().await {
        Ok(result) => {
            circuit_breaker.record_success();
            Ok(result)
        }
        Err(e) => {
            circuit_breaker.record_failure();
            Err(e)
        }
    }
} else {
    Err(Error::other("Circuit breaker open"))
}
```

**Features:**
- Exponential backoff retry with configurable limits
- Circuit breaker with Closed/Open/HalfOpen states
- Fallback strategies for graceful degradation

## Usage Examples

### Basic Setup

```rust
use TurboVault_core::prelude::*;

// Configure a vault
let vault_config = VaultConfig::builder("my-vault", "/path/to/vault")
    .as_default()
    .build()?;

// Create server configuration
let mut server_config = ServerConfig::new();
server_config.vaults.push(vault_config);
server_config.validate()?;

// Create multi-vault manager
let manager = MultiVaultManager::new(server_config)?;
```

### Working with Files

```rust
use TurboVault_core::models::*;

// Create a vault file
let metadata = FileMetadata {
    path: PathBuf::from("notes/readme.md"),
    size: 1024,
    created_at: 1234567890.0,
    modified_at: 1234567890.0,
    checksum: "abc123".to_string(),
    is_attachment: false,
};

let mut file = VaultFile::new(
    PathBuf::from("notes/readme.md"),
    content,
    metadata
);

// Add parsed elements
file.headings.push(Heading {
    text: "Introduction".to_string(),
    level: 1,
    position: SourcePosition::new(0, 0, 0, 14),
    anchor: Some("introduction".to_string()),
});

file.links.push(Link::new(
    LinkType::WikiLink,
    PathBuf::from("notes/readme.md"),
    "Other Note".to_string(),
    SourcePosition::new(5, 10, 100, 20)
));

// Query parsed content
let outgoing = file.outgoing_links();
let has_rust_tag = file.has_tag("rust");
let blocks = file.blocks_with_ids();
```

## Dependent Crates

All other TurboVault crates depend on turbovault-core:

- **turbovault-parser** - Uses models for parse results
- **turbovault-vault** - Uses config and models for vault operations
- **turbovault-graph** - Uses models for link graph construction
- **turbovault-batch** - Uses models and config for batch operations
- **turbovault-export** - Uses models for export operations
- **turbovault-tools** - Uses all types for MCP tool implementations
- **turbovault-server** - Uses config and multi-vault management

## Development

### Building

```bash
cargo build -p turbovault-core
```

### Testing

```bash
# Run all tests
cargo test -p turbovault-core

# Run with output
cargo test -p turbovault-core -- --nocapture

# Run specific test
cargo test -p turbovault-core test_vault_config_builder
```

### Documentation

```bash
# Generate and open docs
cargo doc -p turbovault-core --open
```

## Design Decisions

### Why Separate Core Crate?

1. **Dependency Inversion**: Higher-level crates depend on abstractions, not implementations
2. **Compilation Performance**: Core changes don't rebuild entire project
3. **API Stability**: Core types are stable; implementations can evolve
4. **Testing**: Easy to test models and config without heavyweight dependencies

### Why Builder Pattern?

Configuration objects have many optional fields. Builders provide:
- Clear, fluent API
- Compile-time enforcement of required fields
- Validation on build
- Future-proof (add fields without breaking API)

### Why Lock-Free Metrics?

Traditional metrics use Mutex/RwLock which:
- Adds contention in high-throughput scenarios
- Can cause unpredictable latency spikes
- Doesn't scale well across many cores

Lock-free atomics provide:
- Predictable O(1) performance
- No contention or waiting
- Perfect for hot paths

### Why Custom Error Type?

Using `anyhow::Error` loses type information. Custom `Error` enum provides:
- Pattern matching on error categories
- Rich context (path, size, line numbers)
- Composable error handling across crates
- Clear API contracts

## License

Licensed under the same terms as the TurboVault project.