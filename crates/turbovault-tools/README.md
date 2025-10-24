# turbovault-tools

[![Crates.io](https://img.shields.io/crates/v/turbovault-tools.svg)](https://crates.io/crates/turbovault-tools)
[![Docs.rs](https://docs.rs/turbovault-tools/badge.svg)](https://docs.rs/turbovault-tools)
[![License](https://img.shields.io/crates/l/turbovault-tools.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

MCP Tools Layer - The bridge between AI agents and Obsidian vault operations.

This crate implements the Model Context Protocol (MCP) tools that enable AI agents to discover, query, analyze, and manage Obsidian vaults through a structured, type-safe API. It orchestrates all vault operations by integrating the parser, graph, vault, batch, and export crates into a cohesive agent-friendly interface.

## Architecture Overview

```
AI Agent (Claude, GPT, etc.)
         ↓
    MCP Protocol
         ↓
┌────────────────────────────────────────┐
│     turbovault-tools (THIS CRATE)    │
│                                        │
│  ┌──────────────────────────────────┐ │
│  │   Search Engine (Tantivy)        │ │
│  │   - Full-text search             │ │
│  │   - TF-IDF ranking               │ │
│  │   - Tag & metadata filtering     │ │
│  └──────────────────────────────────┘ │
│                                        │
│  ┌──────────────────────────────────┐ │
│  │   Template Engine                │ │
│  │   - Pre-built templates          │ │
│  │   - Field validation             │ │
│  │   - Note generation              │ │
│  └──────────────────────────────────┘ │
│                                        │
│  ┌──────────────────────────────────┐ │
│  │   11 Tool Categories             │ │
│  │   - File ops                     │ │
│  │   - Search & discovery           │ │
│  │   - Graph analysis               │ │
│  │   - Batch operations             │ │
│  │   - Export & reporting           │ │
│  │   - Templates                    │ │
│  │   - Validation                   │ │
│  │   - Metadata queries             │ │
│  │   - Relationships                │ │
│  │   - Vault lifecycle              │ │
│  │   - Analysis tools               │ │
│  └──────────────────────────────────┘ │
└────────────────────────────────────────┘
         ↓
┌────────────────────────────────────────┐
│   Integration with Other Crates       │
│                                        │
│   turbovault-vault ─ File I/O        │
│   turbovault-parser ─ OFM parsing    │
│   turbovault-graph ─ Link analysis   │
│   turbovault-batch ─ Transactions    │
│   turbovault-export ─ Data export    │
│   turbovault-core ─ Types & errors   │
└────────────────────────────────────────┘
```

## What This Crate Provides

### 1. Agent-Optimized API Surface

Every tool is designed with AI agents in mind:
- **Discoverable**: Tools describe themselves with clear names and parameters
- **Structured Output**: JSON-serializable results for easy parsing
- **Error Friendly**: Errors include suggestions and context for recovery
- **Batching Support**: Coordinate multiple operations atomically
- **Search First**: Rich search and discovery to find relevant notes

### 2. Production-Grade Search Engine

Built on **Tantivy** (Apache Lucene-inspired):
- Full-text indexing of all markdown content
- TF-IDF relevance scoring
- Field-specific search (content, title, tags, frontmatter)
- Advanced filtering (tags, metadata, exclusions)
- Related note recommendations
- Fuzzy matching support

### 3. Template System for Consistent Note Creation

Pre-built templates for common patterns:
- **Documentation**: Standard docs with sections and cross-references
- **Tasks**: Action items with priority, status, due dates
- **Research**: Findings with sources and related notes

Field types with validation:
- Text, LongText, Date (ISO 8601)
- Select (dropdown), MultiSelect (tags)
- Number, Boolean
- Custom examples and defaults

### 4. Comprehensive Vault Analysis

Health monitoring and metrics:
- Vault health scoring (0-100)
- Hub note detection (highly-connected nodes)
- Orphan and dead-end identification
- Broken link detection with suggestions
- Cycle detection in link graph
- Connectivity metrics and density

## Tool Categories

### FileTools (6 operations)

Basic file lifecycle management:

```rust
use TurboVault_tools::FileTools;

let tools = FileTools::new(vault_manager);

// Read note content
let content = tools.read_file("notes/readme.md").await?;

// Write/create note (atomic, creates directories)
tools.write_file("notes/new-idea.md", "# My Idea\n\nContent...").await?;

// Move/rename (updates all backlinks)
tools.move_file("old/path.md", "new/path.md").await?;

// Copy with metadata preservation
tools.copy_file("template.md", "new-note.md").await?;

// Safe deletion (validates path traversal)
tools.delete_file("archive/old.md").await?;
```

**Key Features:**
- Path traversal protection (all paths validated against vault root)
- Atomic writes via temp files
- Automatic directory creation
- Safe error handling with rollback

### SearchTools (4 operations)

Relationship discovery and navigation:

```rust
use TurboVault_tools::SearchTools;

let tools = SearchTools::new(vault_manager);

// Find all notes linking to this note
let backlinks = tools.find_backlinks("concepts/rust.md").await?;

// Find all notes this note links to
let forward_links = tools.find_forward_links("concepts/rust.md").await?;

// Find notes within N hops in link graph
let related = tools.find_related_notes("concepts/rust.md", 2).await?;

// Simple filename search
let matching = tools.search_files("todo").await?;
```

**Key Features:**
- Graph-based relationship tracking
- Hop distance limiting for performance
- Wikilink and embed support
- Bidirectional link traversal

### SearchEngine (5 operations)

Full-text search powered by Tantivy:

```rust
use TurboVault_tools::{SearchEngine, SearchQuery};

let engine = SearchEngine::new(vault_manager).await?;

// Simple keyword search
let results = engine.search("async rust patterns").await?;
// Returns: Vec<SearchResultInfo> with scores, snippets, metadata

// Advanced search with filters
let query = SearchQuery::new("database design")
    .with_tags(vec!["architecture".to_string()])
    .with_frontmatter("status".to_string(), "complete".to_string())
    .exclude(vec!["archive/".to_string()])
    .limit(20);
let results = engine.advanced_search(query).await?;

// Search by tags only
let tagged = engine.search_by_tags(vec!["urgent", "bug"]).await?;

// Find similar notes (content-based)
let similar = engine.find_related("notes/current.md", 5).await?;

// Get recommendations for an agent
let recommendations = engine.recommend_related("notes/current.md").await?;
```

**SearchResultInfo Structure:**
```rust
pub struct SearchResultInfo {
    pub path: String,              // Relative to vault root
    pub title: String,             // From frontmatter or first heading
    pub preview: String,           // First 200 chars
    pub score: f64,                // Relevance (0.0-1.0)
    pub snippet: String,           // Match context with highlighting
    pub tags: Vec<String>,         // Frontmatter tags
    pub outgoing_links: Vec<String>, // Files this note links to
    pub backlink_count: usize,     // How many notes link here
}
```

**Performance**: Indexes on creation (~1000 files/sec), searches in <100ms for 10k+ note vaults.

### TemplateEngine (4 operations)

Structured note creation for agents:

```rust
use TurboVault_tools::{TemplateEngine, TemplateFieldType};
use std::collections::HashMap;

let engine = TemplateEngine::new(vault_manager);

// List all available templates
let templates = engine.list_templates();
// Returns: ["doc", "task", "research"]

// Get template details
let task_template = engine.get_template("task").unwrap();
println!("Fields: {:?}", task_template.fields);

// Create note from template
let mut fields = HashMap::new();
fields.insert("title".to_string(), "Fix database connection".to_string());
fields.insert("priority".to_string(), "high".to_string());
fields.insert("due_date".to_string(), "2025-12-31".to_string());

let created = engine.create_from_template(
    "task",
    "tasks/fix-db-connection.md",
    fields
).await?;
// Result: CreatedNoteInfo with path, preview, template_id

// Find all notes created from a template
let task_notes = engine.find_notes_from_template("task").await?;
```

**Built-in Templates:**

1. **doc** (Documentation):
   - Fields: title, summary, tags (architecture/security/guide)
   - Structure: Overview, Details, Links sections
   - Use case: Technical documentation, guides, explanations

2. **task** (Action Item):
   - Fields: title, priority (low/medium/high/critical), due_date
   - Structure: Priority, Description, Checklist
   - Use case: Task tracking, TODOs, action items

3. **research** (Research Note):
   - Fields: topic, date_researched
   - Structure: Key Findings, Sources, Related notes
   - Use case: Research findings, literature notes, investigations

**Field Validation Examples:**
```rust
// Date validation
template.validate_field("due_date", "2025-12-31")?; // OK
template.validate_field("due_date", "invalid")?;    // Error: Invalid date format

// Select validation
template.validate_field("priority", "high")?;       // OK
template.validate_field("priority", "urgent")?;     // Error: Invalid option

// Required field check
template.validate_field("title", "")?;              // Error: Field is required
```

### AnalysisTools (4 operations)

Vault-wide statistics and metrics:

```rust
use TurboVault_tools::AnalysisTools;

let tools = AnalysisTools::new(vault_manager);

// Get overall vault statistics
let stats = tools.get_vault_stats().await?;
// Returns: VaultStats {
//   total_files, total_links, orphaned_files, average_links_per_file
// }

// Find orphaned notes (no incoming or outgoing links)
let orphans = tools.list_orphaned_notes().await?;

// Detect cycles (mutual reference chains)
let cycles = tools.detect_cycles().await?;
// Returns: Vec<Vec<String>> - each cycle is a loop of file paths

// Calculate link density (actual links / possible links)
let density = tools.get_link_density().await?;
// Returns: f64 (0.0 = no links, 1.0 = fully connected)

// Get comprehensive connectivity metrics
let metrics = tools.get_connectivity_metrics().await?;
// Returns: JSON with all metrics combined
```

### GraphTools (7 operations)

Link graph analysis and health monitoring:

```rust
use TurboVault_tools::GraphTools;

let tools = GraphTools::new(vault_manager);

// Quick health check (fast, essential metrics only)
let health = tools.quick_health_check().await?;
// Returns: HealthInfo {
//   health_score: 85,
//   total_notes: 1250,
//   broken_links_count: 3,
//   is_healthy: true
// }

// Full health analysis (comprehensive, slower)
let full = tools.full_health_analysis().await?;
// Includes: hub_notes, dead_end_notes, isolated_clusters

// Get broken links with suggestions
let broken = tools.get_broken_links().await?;
// Returns: Vec<BrokenLinkInfo> with suggestions for fixes

// Find hub notes (highly connected)
let hubs = tools.get_hub_notes(10).await?;
// Returns: Top 10 notes by link count

// Find dead-end notes (no outgoing links)
let dead_ends = tools.get_dead_end_notes().await?;

// Detect cycles in link graph
let cycles = tools.detect_cycles().await?;

// Get connected components (isolated groups)
let components = tools.get_connected_components().await?;

// Find isolated clusters (small disconnected groups)
let clusters = tools.get_isolated_clusters().await?;
```

**Health Score Calculation:**
- 100: Perfect (no broken links, no orphans)
- 80-99: Good (minor issues)
- 60-79: Fair (needs attention)
- 0-59: Poor (critical issues)

### BatchTools (1 operation)

Atomic multi-file operations:

```rust
use TurboVault_batch::BatchOperation;
use TurboVault_tools::BatchTools;

let tools = BatchTools::new(vault_manager);

let operations = vec![
    BatchOperation::CreateFile {
        path: "new/note.md".to_string(),
        content: "# New Note".to_string(),
    },
    BatchOperation::MoveFile {
        from: "old/path.md".to_string(),
        to: "new/path.md".to_string(),
    },
    BatchOperation::UpdateLinks {
        file: "index.md".to_string(),
        old_target: "old/path".to_string(),
        new_target: "new/path".to_string(),
    },
];

// Execute atomically: all succeed or all fail
let result = tools.batch_execute(operations).await?;
// Returns: BatchResult {
//   success: true,
//   executed: 3,
//   total: 3,
//   transaction_id: "uuid",
//   duration_ms: 45,
//   changes: ["Created: new/note.md", "Moved: old/path.md → new/path.md", ...]
// }
```

**Batch Operation Types:**
- `CreateFile`: Create new file with content
- `WriteFile`: Write/overwrite existing file
- `DeleteFile`: Remove file
- `MoveFile`: Rename/move file
- `UpdateLinks`: Find and replace link targets

**Transaction Guarantees:**
- Conflict detection (operations on same file)
- Validation before execution
- Stop-on-first-error with detailed error reporting
- Transaction ID for tracking

### ExportTools (4 operations)

Data export for downstream processing:

```rust
use TurboVault_tools::ExportTools;

let tools = ExportTools::new(vault_manager);

// Export health report
let json = tools.export_health_report("json").await?;
let csv = tools.export_health_report("csv").await?;

// Export broken links
let broken_json = tools.export_broken_links("json").await?;

// Export vault statistics
let stats_csv = tools.export_vault_stats("csv").await?;

// Export comprehensive analysis report
let analysis = tools.export_analysis_report("json").await?;
```

**Export Formats:**
- **JSON**: Pretty-printed, nested structure, easy to parse
- **CSV**: Flattened, compatible with spreadsheets and databases

**Use Cases:**
- Time-series tracking of vault health
- External reporting and dashboards
- Integration with BI tools
- Historical trend analysis

### ValidationTools (3 operations)

Content validation and quality checks:

```rust
use TurboVault_tools::ValidationTools;

let tools = ValidationTools::new(vault_manager);

// Validate single note with default rules
let report = tools.validate_note("notes/readme.md").await?;
// Returns: ValidationReportInfo {
//   passed: true,
//   total_issues: 2,
//   warning_count: 2,
//   issues: [...]
// }

// Validate with custom rules
let report = tools.validate_note_with_rules(
    "notes/readme.md",
    true,                                    // require_frontmatter
    vec!["title".to_string(), "tags".to_string()], // required_fields
    true,                                    // check_links
    Some(100)                                // min_length
).await?;

// Validate entire vault
let vault_report = tools.validate_vault().await?;

// Quick validation with issue limit (for large vaults)
let quick = tools.validate_vault_quick(50).await?;
```

**Validation Rules:**
- Frontmatter presence and required fields
- Link validity (broken link detection)
- Content length minimums
- Custom validators can be added

**Severity Levels:**
- Info: Informational, not blocking
- Warning: Should fix but not critical
- Error: Important issue to address
- Critical: Blocks deployment or causes failures

### MetadataTools (2 operations)

Frontmatter querying and extraction:

```rust
use TurboVault_tools::MetadataTools;

let tools = MetadataTools::new(vault_manager);

// Query files by metadata pattern
let results = tools.query_metadata(r#"status: "draft""#).await?;
let results = tools.query_metadata("priority > 3").await?;
let results = tools.query_metadata("priority < 5").await?;
let results = tools.query_metadata(r#"tags: contains("important")"#).await?;

// Get specific metadata value (supports dot notation)
let value = tools.get_metadata_value("notes/task.md", "priority").await?;
let nested = tools.get_metadata_value("notes/task.md", "config.timeout").await?;
```

**Query Syntax:**
- `key: "value"` - Exact match
- `key > number` - Greater than
- `key < number` - Less than
- `key: contains("substring")` - String contains

**Returns**: JSON with matched files and their metadata

### RelationshipTools (3 operations)

Link strength analysis and suggestions:

```rust
use TurboVault_tools::RelationshipTools;

let tools = RelationshipTools::new(vault_manager);

// Calculate link strength between two files
let strength = tools.get_link_strength("notes/a.md", "notes/b.md").await?;
// Returns: {
//   strength: 0.75,
//   components: {
//     direct_links: 2,
//     backlinks: 1,
//     shared_references: 3
//   },
//   interpretation: "Strong - frequently connected"
// }

// Get link suggestions for a file
let suggestions = tools.suggest_links("notes/current.md", 5).await?;
// Returns: Top 5 suggested links with reasons

// Get centrality ranking (importance scores)
let rankings = tools.get_centrality_ranking().await?;
// Returns: All files ranked by betweenness, closeness, eigenvector centrality
```

**Link Strength Calculation:**
```
strength = (direct_links * 1.0) + (backlinks * 0.7) + (shared_references * 0.3)
normalized to 0.0-1.0
```

**Centrality Metrics:**
- **Betweenness**: How often this note bridges other notes
- **Closeness**: How quickly this note can reach others
- **Eigenvector**: Importance based on connections to important notes

### VaultLifecycleTools (7 operations)

Multi-vault management and lifecycle:

```rust
use TurboVault_tools::VaultLifecycleTools;
use std::path::Path;

let tools = VaultLifecycleTools::new(multi_vault_manager);

// Create new vault with template
let vault_info = tools.create_vault(
    "research",
    Path::new("/vaults/research"),
    Some("research")  // template: default, research, or team
).await?;

// Add existing vault
let vault_info = tools.add_vault_from_path(
    "personal",
    Path::new("/vaults/personal")
).await?;

// List all registered vaults
let vaults = tools.list_vaults().await?;

// Get active vault
let active = tools.get_active_vault().await?;

// Switch to different vault
tools.set_active_vault("research").await?;

// Remove vault from registry (doesn't delete files)
tools.remove_vault("old-vault").await?;

// Validate vault structure
let validation = tools.validate_vault("research").await?;
```

**Vault Templates:**
- **default**: Areas, Projects, Resources, Archive (PARA method)
- **research**: Literature, Theory, Findings, Hypotheses
- **team**: Team, Projects, Decisions, Documentation

## Practical Agent Workflows

### Workflow 1: Finding and Analyzing Notes

**Agent Goal**: "Find all high-priority tasks that are overdue"

```rust
// Step 1: Search by metadata
let results = metadata_tools.query_metadata("priority > 3").await?;

// Step 2: Filter by date
let mut overdue = Vec::new();
for file in results["files"].as_array().unwrap() {
    let path = file["path"].as_str().unwrap();
    let due_date = metadata_tools.get_metadata_value(path, "due_date").await?;

    // Compare with current date
    if is_overdue(&due_date) {
        overdue.push(path);
    }
}

// Step 3: Get task details
for path in overdue {
    let content = file_tools.read_file(path).await?;
    // Process task...
}
```

### Workflow 2: Vault Health Analysis

**Agent Goal**: "Analyze vault health and generate improvement report"

```rust
// Step 1: Quick health check
let health = graph_tools.quick_health_check().await?;

if health.health_score < 70 {
    // Step 2: Detailed analysis
    let full = graph_tools.full_health_analysis().await?;

    // Step 3: Get broken links
    let broken = graph_tools.get_broken_links().await?;

    // Step 4: Find orphans
    let orphans = analysis_tools.list_orphaned_notes().await?;

    // Step 5: Get hub notes (potential index pages)
    let hubs = graph_tools.get_hub_notes(10).await?;

    // Step 6: Export comprehensive report
    let report = export_tools.export_analysis_report("json").await?;

    // Agent generates: "Your vault needs attention:
    // - 15 broken links found (see suggestions)
    // - 23 orphaned notes that should be linked
    // - Consider creating index pages for hub topics: [hubs]"
}
```

### Workflow 3: Bulk Organization

**Agent Goal**: "Reorganize project notes into archive"

```rust
// Step 1: Find completed project notes
let completed = metadata_tools.query_metadata(r#"status: "completed""#).await?;

// Step 2: Build batch operations
let mut operations = Vec::new();

for file in completed {
    let old_path = file["path"].as_str().unwrap();
    let new_path = format!("archive/{}", old_path);

    operations.push(BatchOperation::MoveFile {
        from: old_path.to_string(),
        to: new_path.clone(),
    });

    // Update any references
    let backlinks = search_tools.find_backlinks(old_path).await?;
    for backlink in backlinks {
        operations.push(BatchOperation::UpdateLinks {
            file: backlink.clone(),
            old_target: old_path.to_string(),
            new_target: new_path.clone(),
        });
    }
}

// Step 3: Execute atomically
let result = batch_tools.batch_execute(operations).await?;

if result.success {
    // Agent reports: "Archived 12 completed projects and updated 45 references"
}
```

### Workflow 4: Knowledge Discovery

**Agent Goal**: "Find notes related to current topic for context"

```rust
let current_note = "concepts/async-programming.md";

// Step 1: Get direct relationships
let backlinks = search_tools.find_backlinks(current_note).await?;
let forward_links = search_tools.find_forward_links(current_note).await?;

// Step 2: Get semantically similar notes
let similar = search_engine.find_related(current_note, 10).await?;

// Step 3: Get notes within 2 hops in graph
let nearby = search_tools.find_related_notes(current_note, 2).await?;

// Step 4: Search for related tags
let vault_file = vault_manager.parse_file(Path::new(current_note)).await?;
if let Some(fm) = vault_file.frontmatter {
    let tags = fm.tags();
    let tagged = search_engine.search_by_tags(tags).await?;
}

// Agent synthesizes: "I found 25 related notes:
// - 5 direct references
// - 10 semantically similar (by content)
// - 8 nearby in your knowledge graph
// - 12 with shared tags"
```

### Workflow 5: Template-Based Note Creation

**Agent Goal**: "Create a new research note about Rust concurrency"

```rust
// Step 1: List available templates
let templates = template_engine.list_templates();

// Step 2: Select appropriate template
let research = template_engine.get_template("research").unwrap();

// Step 3: Fill in fields
let mut fields = std::collections::HashMap::new();
fields.insert("topic".to_string(), "Rust Concurrency Patterns".to_string());
fields.insert("date_researched".to_string(), "2025-10-16".to_string());

// Step 4: Create note from template
let created = template_engine.create_from_template(
    "research",
    "research/rust-concurrency.md",
    fields
).await?;

// Step 5: Find related notes to link
let related = search_engine.search("rust async concurrency").await?;

// Step 6: Update note with links
let mut content = file_tools.read_file(&created.path).await?;
content.push_str("\n## Related Notes\n");
for result in related.iter().take(5) {
    content.push_str(&format!("- [[{}]]\n", result.path));
}
file_tools.write_file(&created.path, &content).await?;

// Agent reports: "Created research note and linked to 5 related topics"
```

## Integration with turbovault-server

The tools are registered with the MCP server via turbomcp macros:

```rust
// From crates/turbovault-server/src/tools.rs
use TurboVault_tools::*;
use turbomcp::prelude::*;

#[turbomcp::server(name = "obsidian-mcp", version = "1.0.0")]
impl ObsidianServer {
    /// File operations
    #[tool("read_note")]
    async fn read_note(&self, path: String) -> McpResult<String> {
        self.file_tools().read_file(&path).await.into_mcp()
    }

    /// Search operations
    #[tool("search")]
    async fn search(&self, query: String) -> McpResult<Vec<SearchResultInfo>> {
        let engine = SearchEngine::new(self.vault_manager.clone()).await?;
        engine.search(&query).await.into_mcp()
    }

    // ... 36 more tools ...
}
```

## Performance and Scaling

### Memory Usage

- **Base**: ~50MB for server infrastructure
- **Search Index**: ~1MB per 1000 notes (in-memory Tantivy index)
- **Link Graph**: ~500KB per 1000 notes (in-memory graph structure)
- **File Cache**: Configurable, TTL-based eviction

**Total for 10,000 note vault**: ~80MB

### Latency Characteristics

| Operation | Typical Latency | Notes |
|-----------|----------------|-------|
| File Read | <10ms | Direct filesystem access |
| File Write | <20ms | Atomic write via temp file |
| Simple Search | <50ms | In-memory index lookup |
| Advanced Search | <100ms | With filters and ranking |
| Graph Analysis | <200ms | Full vault traversal |
| Health Check | <300ms | Comprehensive metrics |
| Batch Operation | 50ms * ops | Sequential with validation |

### Throughput

- **File Scanning**: 1000+ files/second
- **Search Indexing**: 800+ files/second
- **Concurrent Reads**: Limited by filesystem
- **Graph Building**: 500+ files/second

### Scaling Strategies

**For Large Vaults (10k+ notes):**
1. Use `validate_vault_quick()` instead of `validate_vault()`
2. Limit search results with `.limit()`
3. Cache frequently accessed notes
4. Use batch operations for bulk updates
5. Enable link graph persistence (future)

**For Multiple Vaults:**
1. Use `VaultLifecycleTools` to manage multiple vaults
2. Only one vault active at a time (switch with `set_active_vault`)
3. Each vault has independent index and graph
4. Resource usage scales linearly with number of vaults

## Error Handling from Agent Perspective

All tools return `Result<T, Error>` which maps to MCP errors:

### Common Error Patterns

```rust
// File not found
Err(Error::NotFound("File not found: notes/missing.md"))
// → Agent receives: "NotFound" error, can suggest creating file

// Path traversal attempt
Err(Error::InvalidPath("Path outside vault: ../../etc/passwd"))
// → Agent receives: "InvalidPath" error, understands security boundary

// Validation failure
Err(Error::ValidationError("Missing required field: title"))
// → Agent receives: error with suggestion to add field

// Batch conflict
Err(Error::ConfigError("Conflicting operations on same file"))
// → Agent receives: error explaining operations need to be sequential
```

### Error Recovery Strategies

**For Agents:**
1. **Parse error messages**: Contain actionable information
2. **Use suggestions**: Broken links include similar filenames
3. **Validate before batch**: Call `validate()` before `batch_execute()`
4. **Check file existence**: Use metadata query before operations
5. **Handle partial failures**: BatchResult indicates which operation failed

**Example Agent Error Handling:**
```rust
match file_tools.read_file("notes/task.md").await {
    Ok(content) => { /* Process content */ },
    Err(e) if e.is_not_found() => {
        // Suggest creating file
        "File doesn't exist. Should I create it from a template?"
    },
    Err(e) => {
        // Other errors
        format!("Error: {}", e)
    }
}
```

## Tool Input/Output Schema Patterns

### Input Patterns

**Simple String Arguments:**
```rust
read_file(path: String)
search(query: String)
validate_note(path: String)
```

**Structured Options:**
```rust
validate_note_with_rules(
    path: String,
    require_frontmatter: bool,
    required_fields: Vec<String>,
    check_links: bool,
    min_length: Option<usize>
)
```

**Builder Pattern for Complex Queries:**
```rust
let query = SearchQuery::new("database")
    .with_tags(vec!["architecture"])
    .with_frontmatter("status", "published")
    .limit(10);
```

**Batch Operations (Enum-based):**
```rust
vec![
    BatchOperation::CreateFile { path, content },
    BatchOperation::MoveFile { from, to },
]
```

### Output Patterns

**Simple Values:**
```rust
String                  // File content
Vec<String>             // List of paths
bool                    // Success/failure
f64                     // Metrics
```

**Structured Results:**
```rust
VaultStats {            // Metrics
    total_files: usize,
    total_links: usize,
    orphaned_files: usize,
    average_links_per_file: f64,
}

SearchResultInfo {      // Search results
    path: String,
    title: String,
    score: f64,
    snippet: String,
    tags: Vec<String>,
    // ...
}

BatchResult {           // Operation results
    success: bool,
    executed: usize,
    total: usize,
    changes: Vec<String>,
    errors: Vec<String>,
    transaction_id: String,
    duration_ms: u64,
}
```

**JSON Values (for flexibility):**
```rust
serde_json::Value       // Metadata queries, metrics
```

## Development and Testing

### Running Tests

```bash
# Run all tests in this crate
cargo test -p turbovault-tools

# Run with output
cargo test -p turbovault-tools -- --nocapture

# Run specific test
cargo test -p turbovault-tools test_search_engine

# Run with test coverage
cargo tarpaulin --packages turbovault-tools
```

### Adding New Tools

1. **Create tool module** (e.g., `src/my_tools.rs`):
```rust
use TurboVault_vault::VaultManager;
use std::sync::Arc;

pub struct MyTools {
    pub manager: Arc<VaultManager>,
}

impl MyTools {
    pub fn new(manager: Arc<VaultManager>) -> Self {
        Self { manager }
    }

    pub async fn my_operation(&self, param: String) -> Result<String> {
        // Implementation
        Ok("result".to_string())
    }
}
```

2. **Export from lib.rs:**
```rust
pub mod my_tools;
pub use my_tools::MyTools;
```

3. **Register in server** (in turbovault-server):
```rust
#[tool("my_operation")]
async fn my_operation(&self, param: String) -> McpResult<String> {
    let tools = MyTools::new(self.vault_manager.clone());
    tools.my_operation(param).await.into_mcp()
}
```

### Testing Patterns

**Unit Tests** (in tool modules):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_helper_function() {
        assert_eq!(extract_keywords("hello world"), vec!["hello", "world"]);
    }
}
```

**Integration Tests** (require vault setup):
```rust
#[tokio::test]
async fn test_search_integration() {
    let temp_dir = tempfile::tempdir().unwrap();
    let vault_path = temp_dir.path();

    // Create test vault
    let config = ServerConfig::new()
        .with_vault("test", vault_path);
    let manager = VaultManager::new(config).unwrap();

    // Create test files
    manager.write_file(
        Path::new("test.md"),
        "# Test\nContent here"
    ).await.unwrap();

    // Test search
    let engine = SearchEngine::new(Arc::new(manager)).await.unwrap();
    let results = engine.search("content").await.unwrap();
    assert!(!results.is_empty());
}
```

## Dependencies

This crate integrates all other TurboVault crates:

```toml
[dependencies]
# Internal crates (ordered by dependency)
turbovault-core = { workspace = true }
turbovault-parser = { workspace = true }
turbovault-graph = { workspace = true }
turbovault-vault = { workspace = true }
turbovault-batch = { workspace = true }
turbovault-export = { workspace = true }

# MCP integration (turbomcp)
turbomcp = { version = "2.0.2", features = ["full"] }
turbomcp-protocol = "2.0.2"
turbomcp-server = "2.0.2"

# Search engine (Apache Lucene-inspired)
tantivy = "0.22"

# Core async/serde/etc
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
chrono = { workspace = true, features = ["serde"] }
```

## Architecture Design Choices

### Why Tantivy for Search?

- **Production-grade**: Used in production systems, battle-tested
- **Lucene-inspired**: Proven search architecture and algorithms
- **Fast**: Sub-100ms searches on 10k+ note vaults
- **Memory-efficient**: In-memory index with reasonable memory footprint
- **Rust-native**: No C dependencies, excellent safety and performance

**Alternatives Considered:**
- MeiliSearch: Too heavy, requires separate service
- Sonic: Limited query features
- Simple grep: No ranking, too slow for large vaults
- SQLite FTS5: Considered, but Tantivy has better Rust integration

### Why Separate Tool Modules?

**Single Responsibility:**
- Each module focuses on one domain (search, files, graphs)
- Easy to understand and maintain
- Clear boundaries for testing

**Composability:**
- Tools can be used independently
- Agent can choose appropriate tools for task
- Easy to extend with new tool categories

**Performance:**
- Only create what you need (lazy initialization)
- Can optimize each domain separately
- Clear performance boundaries

### Why Arc<VaultManager> Everywhere?

**Shared State:**
- All tools need access to vault manager
- Vault manager maintains cache, graph, file locks
- Single source of truth for vault state

**Thread Safety:**
- Arc enables sharing across async tasks
- VaultManager uses internal locking (RwLock, DashMap)
- Safe concurrent access from multiple tools

**Lifetime Simplicity:**
- No lifetime annotations needed
- Tools can be moved freely
- Simplifies async code significantly

## References to Other Documentation

For deeper dives into specific areas:

- **Core Types and Errors**: See `crates/turbovault-core/README.md`
- **OFM Parsing**: See `crates/turbovault-parser/README.md`
- **Link Graph Analysis**: See `crates/turbovault-graph/README.md`
- **File Operations**: See `crates/turbovault-vault/README.md`
- **Batch Transactions**: See `crates/turbovault-batch/README.md`
- **Export Formats**: See `crates/turbovault-export/README.md`
- **MCP Server**: See `crates/turbovault-server/README.md`
- **Deployment**: See `/docs/deployment/index.md` (project root)
- **Code Quality**: See `/DILIGENCE_PASS_COMPLETE.md` (project root)

## Future Enhancements

Potential additions (not yet implemented):

1. **Persistent Search Index**: Save/load index to avoid rebuilding
2. **Incremental Indexing**: Update index on file changes, not full rebuild
3. **Advanced Tantivy Features**: Fuzzy search, phrase queries, boosting
4. **Custom Validators**: Plugin system for validation rules
5. **More Templates**: Code snippets, meeting notes, project templates
6. **Link Refactoring**: Rename note and update all backlinks atomically
7. **Snapshot/Restore**: Point-in-time vault backups
8. **Diff/Merge Tools**: For concurrent edits and conflict resolution
9. **Real-time Collaboration**: Multi-user editing support
10. **Graph Visualization**: Export graph data for visualization tools

## License

Part of the TurboVault project. See project root for license information.