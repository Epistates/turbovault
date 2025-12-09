# API Reference

Complete reference for all 44 MCP tools available to AI agents.

## Tool Categories

### File Operations (6 tools)
- `read_note` - Read note content
- `write_note` - Write/create note
- `delete_note` - Delete note
- `move_note` - Move/rename note
- `copy_note` - Copy note
- `list_files` - List vault files

### Search & Discovery (5 tools)
- `search` - Full-text search
- `advanced_search` - Search with filters
- `search_by_tags` - Search by tags
- `find_related` - Find similar notes
- `recommend_related` - Get recommendations

### Link Analysis (4 tools)
- `get_backlinks` - Find notes linking to this note
- `get_forward_links` - Find notes this note links to
- `find_related_notes` - Find notes within N hops
- `search_files` - Filename search

### Graph Analysis (7 tools)
- `quick_health_check` - Fast health metrics
- `full_health_analysis` - Comprehensive health report
- `get_broken_links` - Find broken links with suggestions
- `get_hub_notes` - Find highly connected notes
- `get_dead_end_notes` - Find notes with no outgoing links
- `detect_cycles` - Find circular references
- `get_connected_components` - Find isolated clusters

### Template System (4 tools)
- `list_templates` - List available templates
- `get_template` - Get template details
- `create_from_template` - Create note from template
- `find_notes_from_template` - Find notes created from template

### Batch Operations (1 tool)
- `batch_execute` - Execute multiple operations atomically

### Export & Reporting (4 tools)
- `export_health_report` - Export health metrics
- `export_broken_links` - Export broken links
- `export_vault_stats` - Export vault statistics
- `export_analysis_report` - Export comprehensive analysis

### Validation (3 tools)
- `validate_note` - Validate single note
- `validate_note_with_rules` - Validate with custom rules
- `validate_vault` - Validate entire vault

### Metadata Queries (2 tools)
- `query_metadata` - Query frontmatter
- `get_metadata_value` - Get specific metadata value

### Relationship Analysis (3 tools)
- `get_link_strength` - Calculate link strength between notes
- `suggest_links` - Get link suggestions
- `get_centrality_ranking` - Get importance rankings

### Vault Lifecycle (7 tools)
- `create_vault` - Create new vault
- `add_vault` - Add existing vault
- `list_vaults` - List all vaults
- `get_active_vault` - Get current vault
- `set_active_vault` - Switch vault
- `remove_vault` - Remove vault
- `validate_vault` - Validate vault structure

## Example Workflows

### Search and Summarize
```python
# Search for notes about a topic
results = search("rust async programming")

# Read the top results
for result in results[:3]:
    content = read_note(result.path)
    # Process content...
```

### Vault Health Analysis
```python
# Quick health check
health = quick_health_check()

if health.health_score < 70:
    # Detailed analysis
    broken = get_broken_links()
    orphans = get_dead_end_notes()
    # Generate recommendations...
```

### Create Structured Notes
```python
# List available templates
templates = list_templates()

# Create note from template
created = create_from_template(
    template_id="task",
    path="tasks/user-auth.md",
    fields={
        "title": "User Authentication",
        "priority": "high"
    }
)
```

### Bulk Organization
```python
# Find completed projects
completed = query_metadata('status: "completed"')

# Build batch operations
operations = []
for project in completed:
    operations.append({
        "type": "move_file",
        "from": project.path,
        "to": f"archive/{project.path}"
    })

# Execute atomically
result = batch_execute(operations)
```

## Data Types

### SearchResultInfo
```rust
{
    path: String,              // Relative to vault root
    title: String,             // From frontmatter or first heading
    preview: String,           // First 200 chars
    score: f64,                // Relevance (0.0-1.0)
    snippet: String,           // Match context with highlighting
    tags: Vec<String>,         // Frontmatter tags
    outgoing_links: Vec<String>, // Files this note links to
    backlink_count: usize,     // How many notes link here
}
```

### HealthInfo
```rust
{
    health_score: u8,          // 0-100 health score
    total_notes: usize,        // Total notes in vault
    broken_links_count: usize, // Number of broken links
    is_healthy: bool,          // Overall health status
}
```

### BatchResult
```rust
{
    success: bool,             // All operations succeeded
    executed: usize,           // Number of operations completed
    total: usize,              // Total operations in batch
    transaction_id: String,    // Unique transaction ID
    duration_ms: u64,          // Execution time
    changes: Vec<String>,     // Successful changes
    errors: Vec<String>,       // Error messages
}
```

## Error Handling

All tools return structured errors with context:

- `NotFound` - File or resource not found
- `InvalidPath` - Path validation failure
- `PathTraversalAttempt` - Security violation
- `ValidationError` - Content validation failure
- `ConfigError` - Configuration issue

Error messages include suggestions for recovery when possible.
