# turbovault-vault

[![Crates.io](https://img.shields.io/crates/v/turbovault-vault.svg)](https://crates.io/crates/turbovault-vault)
[![Docs.rs](https://docs.rs/turbovault-vault/badge.svg)](https://docs.rs/turbovault-vault)
[![License](https://img.shields.io/crates/l/turbovault-vault.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Filesystem API for Obsidian vaults – file operations, atomic transactions, real-time watching, and caching.

This crate provides the core abstraction for interacting with the Obsidian vault filesystem. It handles all file I/O with atomic operation guarantees, maintains consistency with the parser and graph layers, and provides real-time file watching for synchronization.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      VaultManager                           │
│  ┌─────────────┐  ┌──────────────┐  ┌──────────────────┐   │
│  │ File Cache  │  │ Link Graph   │  │ Parser           │   │
│  │ (RwLock)    │  │ (RwLock)     │  │                  │   │
│  └─────────────┘  └──────────────┘  └──────────────────┘   │
└─────────────────────────────────────────────────────────────┘
                          ▲
                          │
         ┌────────────────┼────────────────┐
         │                │                │
    ┌────────┐      ┌──────────┐    ┌──────────┐
    │ Atomic │      │ Watcher  │    │ Tools    │
    │ File   │      │ (notify) │    │ Layer    │
    │ Ops    │      └──────────┘    └──────────┘
    └────────┘
```

**Design Philosophy:**
- **Single Source of Truth**: All file operations flow through VaultManager
- **Consistency First**: Cache invalidation on write, atomic operations prevent partial states
- **Thread-Safe**: Arc<RwLock<>> for concurrent access from async tasks
- **Real-time Sync**: File watcher integration keeps graph and cache updated
- **Defensive**: Path traversal prevention, permission checks, comprehensive error handling

## Major Types

### VaultManager

The primary interface for vault operations. Coordinates file I/O, caching, parsing, and graph updates.

**Responsibilities:**
- File reading/writing with atomic guarantees
- Cache management (TTL-based invalidation)
- Integration with `turbovault-parser` for content parsing
- Integration with `turbovault-graph` for link tracking
- Path resolution and security (prevents traversal attacks)
- Vault scanning and initialization

**Thread Safety:** All operations are async and use `RwLock` for concurrent access. Safe to share across tasks via `Arc<VaultManager>`.

### AtomicFileOps

ACID-like transaction support for file operations.

**Responsibilities:**
- Atomic write-temp-rename pattern
- Automatic backup before operations
- Transaction-level rollback on error
- Per-file locking to prevent concurrent modification
- Support for batch operations (Write, Delete, Move)

**Guarantees:**
- All operations in a transaction succeed or all are rolled back
- Original file state is preserved on failure
- No partial file writes (temp file + atomic rename)
- Operations execute in order, rollback in reverse order

### VaultWatcher

Real-time filesystem monitoring built on `notify` crate.

**Responsibilities:**
- Event stream for file create/modify/delete/rename
- Configurable filtering (markdown-only, ignore hidden files)
- Debouncing to reduce event noise
- Platform-agnostic (uses `notify::RecommendedWatcher`)

**Event Types:**
- `FileCreated(PathBuf)` – New file appeared
- `FileModified(PathBuf)` – File content changed
- `FileDeleted(PathBuf)` – File removed
- `FileRenamed(PathBuf, PathBuf)` – File moved/renamed

### VaultEvent

Normalized file system events for application consumption. Provides helper methods:
- `path()` – Get primary affected path
- `is_markdown()` – Check if event is for .md file

## Practical Examples

### 1. Initialize Vault

```rust
use TurboVault_vault::VaultManager;
use TurboVault_core::{ServerConfig, VaultConfig};
use std::path::Path;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create configuration
    let vault_path = Path::new("/path/to/obsidian/vault");
    let mut config = ServerConfig::new();
    config.vaults.push(
        VaultConfig::builder("my_vault", vault_path).build()?
    );

    // Create vault manager
    let manager = VaultManager::new(config)?;

    // Scan vault and build initial graph
    // This parses all .md files and populates the link graph
    manager.initialize().await?;

    println!("Vault initialized successfully");
    Ok(())
}
```

### 2. Read and Write Notes

```rust
use TurboVault_vault::VaultManager;
use std::path::Path;

async fn read_write_example(manager: &VaultManager) -> Result<(), Box<dyn std::error::Error>> {
    // Read a note (from cache if available, disk otherwise)
    let content = manager.read_file(Path::new("Daily Notes/2025-01-15.md")).await?;
    println!("Note content: {}", content);

    // Write a new note atomically
    // Creates parent directories if needed
    let new_content = "# Meeting Notes\n\n- Discussed project roadmap\n- [[Project Alpha]]";
    manager.write_file(
        Path::new("Meetings/2025-01-15 Standup.md"),
        new_content
    ).await?;

    // Cache is automatically invalidated for the written file
    // Graph will be updated on next scan or watch event

    Ok(())
}
```

**Note:** `write_file` uses temp-file-then-rename for atomicity. No partial writes are ever visible to readers.

### 3. Watch for File Changes

```rust
use TurboVault_vault::{VaultWatcher, WatcherConfig, VaultEvent};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let vault_path = PathBuf::from("/path/to/vault");

    // Configure watcher (default: markdown only, ignore hidden, recursive)
    let config = WatcherConfig {
        recursive: true,
        markdown_only: true,
        ignore_hidden: true,
        debounce_ms: 100,
    };

    // Create watcher and event receiver
    let (mut watcher, mut event_rx) = VaultWatcher::new(vault_path, config)?;

    // Start watching
    watcher.start().await?;

    // Process events
    tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                VaultEvent::FileCreated(path) => {
                    println!("New file: {:?}", path);
                    // Trigger: parse file, update graph
                }
                VaultEvent::FileModified(path) => {
                    println!("Modified: {:?}", path);
                    // Trigger: re-parse, update links, invalidate cache
                }
                VaultEvent::FileDeleted(path) => {
                    println!("Deleted: {:?}", path);
                    // Trigger: remove from graph, clear cache
                }
                VaultEvent::FileRenamed(from, to) => {
                    println!("Renamed: {:?} -> {:?}", from, to);
                    // Trigger: update graph node, update cache keys
                }
            }
        }
    });

    // Watcher runs in background until stopped
    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    watcher.stop().await?;

    Ok(())
}
```

**Platform Notes:**
- Uses `notify::RecommendedWatcher` (inotify on Linux, FSEvents on macOS, ReadDirectoryChangesW on Windows)
- Some platforms may emit multiple events for single operation (handled by debouncing)
- Events are async and non-blocking

### 4. Query Link Graph

```rust
use TurboVault_vault::VaultManager;
use std::path::Path;

async fn graph_queries(manager: &VaultManager) -> Result<(), Box<dyn std::error::Error>> {
    let note = Path::new("Project Alpha.md");

    // Get files linking TO this note (backlinks)
    let backlinks = manager.get_backlinks(note).await?;
    println!("Backlinks to 'Project Alpha': {:?}", backlinks);

    // Get files linked FROM this note (forward links)
    let forward_links = manager.get_forward_links(note).await?;
    println!("Links from 'Project Alpha': {:?}", forward_links);

    // Find related notes within 2 hops (BFS traversal)
    let related = manager.get_related_notes(note, 2).await?;
    println!("Related notes (2 hops): {:?}", related);

    // Find orphaned notes (no incoming or outgoing links)
    let orphans = manager.get_orphaned_notes().await?;
    println!("Orphaned notes: {:?}", orphans);

    // Get graph statistics
    let stats = manager.get_stats().await?;
    println!("Total files: {}", stats.total_files);
    println!("Total links: {}", stats.total_links);
    println!("Orphaned files: {}", stats.orphaned_files);

    Ok(())
}
```

**Graph Integration:** VaultManager maintains an `Arc<RwLock<LinkGraph>>` that's updated during `initialize()` and should be updated on file changes (via watcher integration in server layer).

### 5. Atomic Transactions with Error Handling

```rust
use TurboVault_vault::{AtomicFileOps, FileOp};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let backup_dir = PathBuf::from("/tmp/vault_backups");
    let atomic = AtomicFileOps::new(backup_dir).await?;

    // Execute multiple operations atomically
    let operations = vec![
        FileOp::Write(
            PathBuf::from("/vault/note1.md"),
            "# Note 1\nContent here".to_string(),
        ),
        FileOp::Write(
            PathBuf::from("/vault/note2.md"),
            "# Note 2\n[[note1]]".to_string(),
        ),
        FileOp::Move(
            PathBuf::from("/vault/old_note.md"),
            PathBuf::from("/vault/archive/old_note.md"),
        ),
    ];

    // All succeed or all rollback
    match atomic.execute_transaction(operations).await? {
        result if result.rolled_back => {
            eprintln!("Transaction failed and was rolled back!");
            eprintln!("Executed {} operations before error", result.operations);
        }
        result => {
            println!("Transaction succeeded!");
            println!("Affected paths: {:?}", result.affected_paths);
        }
    }

    Ok(())
}
```

**Error Recovery:**
- If any operation fails, all previous operations are rolled back
- Original file state is restored from backups
- Backups are cleaned up on success
- Per-file locking prevents concurrent modifications during transaction

## Atomic Operation Guarantees

### Write Operation
1. Create parent directories (idempotent)
2. Write content to `{file}.tmp`
3. Atomic rename from `{file}.tmp` to `{file}`
4. Invalidate cache entry

**Guarantee:** No partial writes visible. Either old content or new content, never corrupted state.

### Transaction Semantics
```
BEGIN TRANSACTION
  ├─ Create backups for all affected files
  ├─ Acquire locks for all paths (prevent concurrent access)
  ├─ Execute operations in order
  │   ├─ Operation 1: SUCCESS
  │   ├─ Operation 2: SUCCESS
  │   └─ Operation 3: FAILURE
  ├─ ROLLBACK (restore from backups in reverse order)
  └─ Release locks
END TRANSACTION (rolled_back = true)
```

**Properties:**
- **Atomicity:** All or nothing
- **Consistency:** Valid state before and after
- **Isolation:** Per-file locking (not full MVCC, but sufficient for vault operations)
- **Durability:** Writes go through OS page cache (fsync not called, trade-off for performance)

### Consistency Model

**Cache Consistency:**
- Write invalidates cache immediately
- Read checks cache expiration (default TTL from `ServerConfig.cache_ttl`)
- Expired entries refetch from disk

**Graph Consistency:**
- Graph is updated during `initialize()` (full scan)
- File watcher events should trigger incremental updates (implemented in server layer)
- Graph queries use `RwLock` (concurrent reads, exclusive writes)

**Concurrency Pattern:**
```rust
// Multiple concurrent reads (no blocking)
let content1 = manager.read_file("note1.md").await?; // ✓ concurrent
let content2 = manager.read_file("note2.md").await?; // ✓ concurrent

// Write blocks other writes to same file, but not reads
manager.write_file("note1.md", "new content").await?; // Acquires write lock briefly
```

## File Watching Integration

The watcher is designed to integrate with the server layer for real-time graph updates:

```rust
// Typical integration pattern (in server layer)
async fn watch_vault(manager: Arc<VaultManager>) {
    let (mut watcher, mut events) = VaultWatcher::new(
        manager.vault_path().clone(),
        WatcherConfig::default()
    ).unwrap();

    watcher.start().await.unwrap();

    while let Some(event) = events.recv().await {
        match event {
            VaultEvent::FileCreated(path) | VaultEvent::FileModified(path) => {
                // Re-parse file
                if let Ok(vault_file) = manager.parse_file(&path).await {
                    // Update graph
                    let mut graph = manager.link_graph().write().await;
                    let _ = graph.add_file(&vault_file);
                    let _ = graph.update_links(&vault_file);
                }
            }
            VaultEvent::FileDeleted(path) => {
                // Remove from graph
                let mut graph = manager.link_graph().write().await;
                let _ = graph.remove_file(&path);
            }
            VaultEvent::FileRenamed(from, to) => {
                // Update graph node
                // (Implementation depends on graph API)
            }
        }
    }
}
```

**Debouncing:** Configure `WatcherConfig.debounce_ms` to reduce event spam (e.g., text editors save multiple times per second).

## Error Handling and Recovery

### Error Types (from `TurboVault_core::Error`)

- `Error::io(...)` – Filesystem errors (permission denied, file not found)
- `Error::path_traversal(...)` – Security violation (attempted `../../../etc/passwd`)
- `Error::parse_error(...)` – Parser failures (malformed frontmatter)
- `Error::invalid_path(...)` – Invalid path format

### Recovery Strategies

**Read Errors:**
```rust
match manager.read_file(path).await {
    Ok(content) => process(content),
    Err(e) if e.to_string().contains("not found") => {
        // File doesn't exist, handle gracefully
        create_default_note(path).await?;
    }
    Err(e) => {
        // Permission error or I/O failure
        log::error!("Failed to read {}: {}", path.display(), e);
        return Err(e);
    }
}
```

**Write Errors:**
```rust
match manager.write_file(path, content).await {
    Ok(_) => println!("Write succeeded"),
    Err(e) => {
        // Atomic operation guarantees no partial write
        // Original file (if existed) is unchanged
        log::error!("Write failed: {}", e);
        // Retry logic or user notification
    }
}
```

**Transaction Rollback:**
- Automatic rollback on any operation failure
- Check `TransactionResult.rolled_back` to detect failures
- Original files restored from backups
- Safe to retry transaction (idempotent if operations are idempotent)

### Path Security

All paths are validated through `resolve_path()`:

```rust
// Prevents traversal attacks
manager.read_file("../../../etc/passwd").await; // Error: PathTraversal

// Canonicalization handles symlinks
manager.read_file("symlink/note.md").await; // Resolved to real path, checked against vault root

// Absolute paths must be under vault
manager.read_file("/vault/note.md").await; // OK if /vault is the vault root
manager.read_file("/tmp/note.md").await;   // Error: PathTraversal (outside vault)
```

## Thread-Safety and Concurrency

### Read Operations
- Use `RwLock::read()` – multiple concurrent readers
- Cache lookups are fast (HashMap with RwLock)
- Disk reads are async I/O (non-blocking)

### Write Operations
- Use `RwLock::write()` – exclusive lock for cache/graph
- Lock held briefly (invalidate cache, update graph)
- File I/O is async (non-blocking)

### Atomic Operations
- Per-file locking via `Arc<RwLock<()>>` registry
- Prevents concurrent transactions on same file
- Different files can be modified concurrently

### Typical Concurrency Patterns

```rust
// Safe: Concurrent reads from multiple tasks
let m = Arc::new(manager);
let (r1, r2) = tokio::join!(
    m.read_file("note1.md"),
    m.read_file("note2.md"),
);

// Safe: Read + Write (write briefly blocks, then readers continue)
let m = Arc::new(manager);
tokio::spawn({
    let m = m.clone();
    async move {
        m.write_file("note.md", "content").await.unwrap();
    }
});
let content = m.read_file("note.md").await.unwrap(); // May see old or new content

// Safe: Atomic ops use internal locking
let ops1 = vec![FileOp::Write(path1, content1)];
let ops2 = vec![FileOp::Write(path2, content2)];
tokio::join!(
    atomic.execute_transaction(ops1),
    atomic.execute_transaction(ops2),
); // Both execute safely (different files)
```

## Integration Points

### Upstream: Parser

```rust
use TurboVault_parser::Parser;

// VaultManager owns a Parser instance
let parser = Parser::new(vault_path.clone());

// During initialize() or file watch events
let vault_file = parser.parse_file(&path, &content)?;
// vault_file contains: metadata, frontmatter, links, headings, tags, tasks, etc.
```

**Flow:** Raw file content → Parser → `VaultFile` (structured data) → Graph

### Downstream: Graph

```rust
use TurboVault_graph::LinkGraph;

// VaultManager owns Arc<RwLock<LinkGraph>>
let link_graph = Arc::new(RwLock::new(LinkGraph::new()));

// During initialize()
for vault_file in files {
    graph.add_file(&vault_file)?;       // Add node
    graph.update_links(&vault_file)?;   // Add edges
}

// Query graph
let backlinks = graph.backlinks(&path)?;
let stats = graph.stats();
```

**Flow:** `VaultFile` → Graph (nodes + edges) → Query results (backlinks, related notes, orphans)

### Downstream: Tools

```rust
// Tools layer calls VaultManager methods
// Example: list_files tool
let files = manager.scan_vault().await?;

// Example: get_backlinks tool
let backlinks = manager.get_backlinks(path).await?;

// Example: write_file tool
manager.write_file(path, content).await?;
```

**Flow:** MCP client → Server → Tools → VaultManager → Filesystem

## Performance Characteristics

### File Cache
- **Hit Ratio:** Depends on TTL and access patterns (default TTL: configurable)
- **Invalidation:** Write-through (invalidate on write)
- **Memory:** Unbounded HashMap (consider LRU for large vaults in future)

### Vault Scanning
- **Time Complexity:** O(n) where n = number of files
- **I/O:** Sequential reads (not parallelized currently)
- **Optimization:** Respects `excluded_paths` and `max_file_size` from config

### Graph Queries
- **Backlinks/Forward Links:** O(1) lookup in adjacency list (petgraph internals)
- **Related Notes:** BFS traversal, O(E + V) where E = edges, V = vertices
- **Orphan Detection:** O(V) to check in-degree and out-degree

### File Watching
- **Overhead:** Minimal (OS-level notifications via inotify/FSEvents)
- **Debouncing:** Configured delay before emitting event (reduces spam)

### Atomic Operations
- **Lock Contention:** Per-file locks minimize contention
- **Backup Overhead:** One file copy per operation (trade-off for safety)
- **Temp File:** One extra write per operation (atomicity guarantee)

## Vault Size Limits

**Tested Scale:**
- Files: Tested with hundreds of files (test suite)
- File Size: Limited by `ServerConfig.max_file_size` (default: usually 5MB)
- Vault Size: No hard limit (bounded by memory for cache and graph)

**Scalability Considerations:**
- **Large Vaults (10k+ files):** Consider incremental graph updates instead of full `initialize()`
- **Large Files:** Parser may be slow on very large markdown files (consider chunking)
- **High Write Frequency:** Atomic ops create temp files/backups (disk space consideration)

**Future Optimizations:**
- LRU cache eviction (currently unbounded HashMap)
- Parallel file scanning during `initialize()`
- Incremental graph updates (delta sync)
- Memory-mapped file I/O for large files

## Development and Testing

### Running Tests

```bash
# From crate directory
cd crates/turbovault-vault
cargo test

# With output
cargo test -- --nocapture

# Specific test
cargo test test_atomic_write

# Integration tests (from workspace root)
cargo test --test vault_lifecycle_test
```

### Test Coverage

The crate includes comprehensive tests:

**Unit Tests:**
- `manager.rs`: VaultManager operations (read, write, cache, path security)
- `atomic.rs`: Atomic operations (write, delete, move, transactions, rollback)
- `watcher.rs`: File watching (create, modify, delete events, filtering)

**Integration Tests:**
- `vault_lifecycle_test.rs`: Multi-vault management
- Test fixtures use `tempfile` for isolated temporary directories

### Writing Tests

```rust
use TurboVault_vault::VaultManager;
use TurboVault_core::{ServerConfig, VaultConfig};
use tempfile::TempDir;

#[tokio::test]
async fn test_custom_scenario() {
    // Create isolated temporary vault
    let temp_dir = TempDir::new().unwrap();
    let mut config = ServerConfig::new();
    config.vaults.push(
        VaultConfig::builder("test", temp_dir.path()).build().unwrap()
    );

    let manager = VaultManager::new(config).unwrap();

    // Create test file
    std::fs::write(temp_dir.path().join("test.md"), "# Test").unwrap();

    // Test your scenario
    manager.initialize().await.unwrap();
    let content = manager.read_file(Path::new("test.md")).await.unwrap();
    assert_eq!(content, "# Test");
}
```

### Common Test Patterns

**Path Canonicalization (macOS):**
```rust
// macOS uses /private/var vs /var symlinks
let event_path = event.path().canonicalize().ok();
let expected_path = file_path.canonicalize().ok();
assert_eq!(event_path, expected_path); // Safe comparison
```

**Watcher Timing:**
```rust
// Give filesystem time to propagate events
tokio::time::sleep(Duration::from_millis(500)).await;

// Drain event queue
while rx.try_recv().is_ok() {}
```

### Debugging

Enable logs during tests:

```bash
RUST_LOG=TurboVault_vault=debug cargo test -- --nocapture
```

Instrumentation spans:
- `vault_initialize` – Full vault scan
- `vault_read_file` – File read operation
- `vault_write_file` – File write operation
- `vault_parse_file` – Parse single file
- `vault_scan` – Scan vault for files

## Dependencies

From `Cargo.toml`:

```toml
[dependencies]
turbovault-core = { path = "../turbovault-core" }      # Core types, config, errors
turbovault-parser = { path = "../turbovault-parser" }  # Markdown parsing
turbovault-graph = { path = "../turbovault-graph" }    # Link graph

tokio = { workspace = true }           # Async runtime
tokio-util = { workspace = true }      # Async utilities
notify = { workspace = true }          # File system watching (v6)
walkdir = { workspace = true }         # Directory traversal
dashmap = { workspace = true }         # Concurrent HashMap (used in atomic ops)
uuid = { workspace = true }            # Unique IDs
serde = { workspace = true }           # Serialization
serde_json = { workspace = true }      # JSON support
thiserror = { workspace = true }       # Error derive macros
anyhow = { workspace = true }          # Error handling
log = { workspace = true }             # Logging facade
tracing = { workspace = true }         # Instrumentation
```

**Key Dependencies:**
- **notify 6.x:** Cross-platform file watching (actively maintained as of 2025)
- **tokio:** Async runtime (all I/O is async)
- **dashmap:** Lock-free concurrent HashMap (for atomic ops locking registry)

## Design Decisions

### Why temp-file-then-rename?
**Atomicity.** Unix rename is atomic; writing directly to a file is not. Prevents readers from seeing partial writes.

### Why not use a database?
**Simplicity.** The filesystem IS the database for Obsidian. VaultManager is a thin layer that adds safety and graph integration.

### Why TTL-based cache?
**Balance.** Full consistency requires invalidating on every external change (via watcher). TTL provides "good enough" consistency with simpler implementation. Production systems should integrate watcher for real-time invalidation.

### Why per-file locking instead of global lock?
**Concurrency.** Different files can be modified concurrently. Global lock would serialize all writes unnecessarily.

### Why Arc<RwLock<>> everywhere?
**Async + Shared State.** Tokio tasks need shared ownership (`Arc`) and interior mutability (`RwLock` for concurrent reads). Alternative would be message-passing (actors), but RwLock is simpler for this use case.

### Why not fsync after writes?
**Performance Trade-off.** Calling `fsync` after every write is slow. Vault operations are not financial transactions; OS page cache durability is acceptable. Could be configurable in future.

## Future Enhancements

Potential improvements (not currently implemented):

1. **LRU Cache Eviction** – Prevent unbounded memory growth for large vaults
2. **Incremental Graph Updates** – Delta sync instead of full re-parse on watch events
3. **Parallel Scanning** – Use rayon or tokio::spawn for concurrent file parsing
4. **Memory-Mapped I/O** – For very large files (>10MB)
5. **Configurable fsync** – Durability vs performance trade-off
6. **Watcher Debouncing Logic** – More sophisticated event coalescing
7. **Multi-Vault Manager Integration** – Currently VaultManager is single-vault; integrate with MultiVaultManager for seamless switching

## Summary

**turbovault-vault** is the filesystem abstraction layer for the TurboVault server. It provides:

✅ **Atomic file operations** (write-temp-rename, transactions with rollback)
✅ **Thread-safe concurrent access** (Arc<RwLock<>>, per-file locking)
✅ **Real-time file watching** (notify 6.x, cross-platform)
✅ **Integrated caching** (TTL-based, write-through invalidation)
✅ **Security** (path traversal prevention, canonicalization)
✅ **Parser integration** (automatic VaultFile generation)
✅ **Graph integration** (link tracking, backlinks, orphan detection)

**Key Insight:** VaultManager is the coordination layer between raw filesystem, parsed content, and graph structure. All file operations flow through it to maintain consistency.

## License

MIT (see workspace root)

## Contributing

See workspace root `CLAUDE.md` for development guidelines. This crate follows the project's core principles:

- **No mocks, no placeholders** – All file operations are real
- **Production-ready** – Comprehensive error handling, security by design
- **Test-driven** – Tests use real temporary filesystems (tempfile crate)
- **Container-first** – Development via Docker (see workspace Makefile)