# turbovault-graph

[![Crates.io](https://img.shields.io/crates/v/turbovault-graph.svg)](https://crates.io/crates/turbovault-graph)
[![Docs.rs](https://docs.rs/turbovault-graph/badge.svg)](https://docs.rs/turbovault-graph)
[![License](https://img.shields.io/crates/l/turbovault-graph.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Link graph analysis and vault health diagnostics for Obsidian vaults.

This crate provides comprehensive analysis of the link graph within Obsidian vaults, enabling discovery of relationships, identification of important notes, detection of broken links, and overall vault health assessment.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    ObsidianVaultGraph                       │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │              petgraph::UnGraph                         │ │
│  │  NodeIndex ──→ VaultFile                               │ │
│  │  EdgeIndex ──→ Link                                     │ │
│  └─────────────────────────────────────────────────────────┘ │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │              Index Maps                                 │ │
│  │  path ──→ NodeIndex                                     │ │
│  │  title ──→ NodeIndex                                    │ │
│  │  NodeIndex ──→ VaultFile                               │ │
│  └─────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

**Design Philosophy:**
- **Immutable Graph**: Graph is built once and never modified (rebuild for updates)
- **Zero-Copy Lookups**: Direct access to `VaultFile` data via `NodeIndex`
- **Thread-Safe**: All operations are read-only after construction
- **Memory Efficient**: Uses `petgraph`'s compact representation
- **Fast Queries**: O(1) lookups via hash maps, O(V+E) graph traversals

## Core Operations

### Building the Graph

```rust
use TurboVault_graph::ObsidianVaultGraph;
use TurboVault_core::VaultFile;

// From a collection of parsed vault files
let files: Vec<VaultFile> = /* ... */;
let graph = ObsidianVaultGraph::from_files(files)?;

// Or from a single file (for incremental updates)
let graph = ObsidianVaultGraph::new();
graph.add_file(file)?;
```

**Process:**
1. **Node Creation**: One node per `VaultFile`
2. **Edge Creation**: One edge per `Link` between files
3. **Index Building**: Hash maps for fast path/title lookups
4. **Validation**: Ensures graph consistency

### Backlinks (Incoming Links)

```rust
// Get all files that link TO a specific file
let backlinks = graph.get_backlinks("MyNote.md")?;
// Returns: Vec<VaultFile> of files containing links to MyNote.md

// Count backlinks
let count = graph.count_backlinks("MyNote.md")?;
```

**Use Cases:**
- Find all references to a note
- Identify highly-referenced content
- Build "backlink" views in Obsidian

### Forward Links (Outgoing Links)

```rust
// Get all files that a specific file links TO
let forward_links = graph.get_forward_links("MyNote.md")?;
// Returns: Vec<VaultFile> of files linked from MyNote.md

// Count forward links
let count = graph.count_forward_links("MyNote.md")?;
```

**Use Cases:**
- Find all notes referenced by a file
- Identify hub notes (high out-degree)
- Build "forward link" views

### Related Notes (Co-citation)

```rust
// Find notes that share common backlinks (co-citation)
let related = graph.get_related_notes("MyNote.md", 5)?;
// Returns: Vec<VaultFile> sorted by similarity score

// Find notes that link to similar sets of files
let similar = graph.get_similar_notes("MyNote.md", 5)?;
```

**Algorithm:**
- **Co-citation**: Notes with overlapping backlink sets
- **Similarity Score**: Jaccard similarity of link sets
- **Ranking**: Sorted by similarity strength

### Cycle Detection

```rust
// Find cycles in the link graph
let cycles = graph.find_cycles()?;
// Returns: Vec<Vec<String>> where each inner Vec is a cycle path

// Check if a specific file is part of a cycle
let in_cycle = graph.is_part_of_cycle("MyNote.md")?;
```

**Algorithm:**
- Uses DFS-based cycle detection
- Returns all cycles found in the graph
- Useful for identifying circular references

### Orphan Detection

```rust
// Find files with no incoming links
let orphans = graph.find_orphans()?;
// Returns: Vec<VaultFile> of isolated files

// Find files with no outgoing links (dead ends)
let dead_ends = graph.find_dead_ends()?;
```

**Use Cases:**
- Identify unlinked content
- Find terminal notes in knowledge graphs
- Vault cleanup and organization

### Connected Components

```rust
// Find all connected components (isolated clusters)
let components = graph.find_connected_components()?;
// Returns: Vec<Vec<VaultFile>> where each Vec is a component

// Get the largest connected component
let largest = graph.get_largest_component()?;
```

**Algorithm:**
- Uses Union-Find (Disjoint Set Union)
- Identifies isolated clusters of notes
- Useful for vault fragmentation analysis

### Graph Statistics

```rust
// Overall graph metrics
let stats = graph.get_statistics()?;
// Returns: GraphStatistics {
//   total_nodes: 1250,
//   total_edges: 3400,
//   average_degree: 2.72,
//   density: 0.0022,
//   largest_component_size: 1200,
//   orphan_count: 50,
//   cycle_count: 3
// }
```

**Metrics:**
- **Node Count**: Total number of files
- **Edge Count**: Total number of links
- **Average Degree**: Links per file (in + out)
- **Density**: Actual edges / possible edges
- **Component Analysis**: Largest component size, orphan count
- **Cycle Analysis**: Number of cycles detected

## Vault Health Analysis

### Health Scoring

```rust
// Calculate overall vault health (0-100)
let health_score = graph.calculate_health_score()?;
// Returns: u8 representing health percentage

// Get detailed health breakdown
let health = graph.get_health_analysis()?;
// Returns: VaultHealth {
//   overall_score: 85,
//   connectivity_score: 90,
//   organization_score: 80,
//   issues: vec![
//     HealthIssue::OrphanedFiles(25),
//     HealthIssue::BrokenLinks(5),
//     HealthIssue::Cycles(2)
//   ]
// }
```

**Scoring Factors:**
- **Connectivity**: Percentage of files in largest component
- **Organization**: Ratio of backlinks to forward links
- **Issues**: Penalties for orphans, broken links, cycles
- **Balance**: Even distribution of link density

### Hub Note Detection

```rust
// Find highly-connected notes (hubs)
let hubs = graph.find_hub_notes(10)?;
// Returns: Vec<VaultFile> sorted by connection count

// Get hub statistics
let hub_stats = graph.get_hub_statistics()?;
// Returns: HubStatistics {
//   total_hubs: 15,
//   average_hub_degree: 25.3,
//   max_hub_degree: 67
// }
```

**Criteria:**
- **High In-Degree**: Many files link to this note
- **High Out-Degree**: This note links to many files
- **Centrality**: High betweenness centrality
- **Threshold**: Configurable minimum connection count

### Broken Link Detection

```rust
// Find links that don't resolve to existing files
let broken_links = graph.find_broken_links()?;
// Returns: Vec<BrokenLink> with details

// Get broken link statistics
let broken_stats = graph.get_broken_link_statistics()?;
// Returns: BrokenLinkStatistics {
//   total_broken: 12,
//   broken_wikilinks: 8,
//   broken_embeds: 4,
//   most_broken_file: "Index.md"
// }
```

**Detection:**
- **Wikilinks**: `[[NonExistentNote]]` → broken
- **Embeds**: `![[MissingImage.png]]` → broken
- **Path Resolution**: Handles folder structures
- **Case Sensitivity**: Respects filesystem case sensitivity

## Performance Characteristics

### Memory Usage

- **Graph Storage**: ~8 bytes per node + ~16 bytes per edge
- **Index Maps**: ~32 bytes per file (path + title lookups)
- **Total**: ~50 bytes per file + ~16 bytes per link
- **Example**: 10k files, 25k links ≈ 1.2MB memory

### Query Performance

- **Node Lookup**: O(1) via hash map
- **Backlinks**: O(1) via pre-computed index
- **Forward Links**: O(1) via pre-computed index
- **Graph Traversal**: O(V+E) for DFS/BFS operations
- **Statistics**: O(V+E) for one-time calculation

### Thread Safety

- **Read-Only**: All operations are immutable after construction
- **Concurrent Access**: Multiple threads can query simultaneously
- **No Locks**: No synchronization primitives needed
- **Clone-Friendly**: Cheap cloning for parallel processing

## Integration Points

### With turbovault-vault

```rust
// Vault manager builds graph from file changes
let graph = vault_manager.build_graph().await?;

// Graph is used for health monitoring
let health = graph.calculate_health_score()?;
if health < 70 {
    // Trigger vault health alerts
}
```

### With turbovault-server

```rust
// MCP tools use graph for analysis
let hubs = graph.find_hub_notes(5)?;
let response = json!({
    "hubs": hubs,
    "health_score": graph.calculate_health_score()?
});
```

### With turbovault-tools

```rust
// Search tools use graph for related note discovery
let related = graph.get_related_notes(query_file, 10)?;
// Used in search results and recommendations
```

## Usage Examples

### Example 1: Vault Health Dashboard

```rust
use TurboVault_graph::ObsidianVaultGraph;

let graph = ObsidianVaultGraph::from_files(vault_files)?;

// Overall health
let health = graph.get_health_analysis()?;
println!("Vault Health: {}/100", health.overall_score);

// Top issues
for issue in health.issues {
    match issue {
        HealthIssue::OrphanedFiles(count) => {
            println!("⚠️  {} orphaned files", count);
        }
        HealthIssue::BrokenLinks(count) => {
            println!("🔗 {} broken links", count);
        }
        HealthIssue::Cycles(count) => {
            println!("🔄 {} cycles detected", count);
        }
    }
}

// Hub notes
let hubs = graph.find_hub_notes(5)?;
println!("📊 Top hub notes:");
for hub in hubs {
    let backlinks = graph.count_backlinks(&hub.path)?;
    println!("  - {} ({} backlinks)", hub.path, backlinks);
}
```

### Example 2: Content Discovery

```rust
// Find related content
let related = graph.get_related_notes("RustAsync.md", 5)?;
println!("Notes related to RustAsync.md:");
for note in related {
    println!("  - {}", note.path);
}

// Find similar content
let similar = graph.get_similar_notes("RustAsync.md", 5)?;
println!("Notes with similar link patterns:");
for note in similar {
    println!("  - {}", note.path);
}
```

### Example 3: Vault Organization

```rust
// Find orphaned files
let orphans = graph.find_orphans()?;
println!("Orphaned files (no incoming links):");
for orphan in orphans {
    println!("  - {}", orphan.path);
}

// Find dead ends
let dead_ends = graph.find_dead_ends()?;
println!("Dead end files (no outgoing links):");
for dead_end in dead_ends {
    println!("  - {}", dead_end.path);
    }

    // Find cycles
let cycles = graph.find_cycles()?;
println!("Circular references:");
        for cycle in cycles {
    println!("  - {}", cycle.join(" → "));
}
```

## Development

### Running Tests

```bash
# All tests
cargo test

# Specific test categories
cargo test --test graph_operations
cargo test --test health_analysis
cargo test --test performance

# With output
cargo test -- --nocapture
```

### Adding New Analysis

1. **Define Analysis Function**: Add method to `ObsidianVaultGraph`
2. **Implement Algorithm**: Use `petgraph` algorithms or custom logic
3. **Add Return Type**: Create result struct in `turbovault-core`
4. **Add Tests**: Comprehensive test coverage
5. **Update Documentation**: Add usage examples

### Performance Benchmarking

```bash
# Benchmark graph operations
cargo bench

# Profile memory usage
cargo test --test memory_usage -- --nocapture

# Test with large vaults
cargo test --test large_vault -- --nocapture
```

## Dependencies

- **petgraph**: Graph data structures and algorithms
- **turbovault-core**: Data models and error types
- **serde**: Serialization for health reports
- **serde_json**: JSON output for MCP tools

## Design Decisions

### Why petgraph?

- **Production Ready**: Battle-tested in Rust ecosystem
- **Memory Efficient**: Compact representation
- **Algorithm Rich**: Built-in DFS, BFS, cycle detection
- **Type Safe**: Generic over node and edge types
- **Performance**: Optimized for graph operations

### Why Immutable Graph?

- **Thread Safety**: No synchronization needed
- **Consistency**: Graph state never changes
- **Caching**: Results can be cached indefinitely
- **Simplicity**: No complex state management
- **Performance**: No locks or atomic operations

### Why Hash Map Indexing?

- **Fast Lookups**: O(1) path/title resolution
- **Memory Efficient**: Only stores indices, not data
- **Flexible**: Easy to add new index types
- **Consistent**: Same lookup performance for all files

## License

See workspace license.

## See Also

- `turbovault-core`: Core data models and types
- `turbovault-parser`: Link extraction from markdown
- `turbovault-vault`: Vault management and file operations
- `turbovault-server`: MCP server tools using graph analysis