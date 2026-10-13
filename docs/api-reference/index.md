# API Reference

Reference for TurboVault's MCP tools. At runtime, `tools/list` is the
authoritative catalog, including schemas and behavior annotations.

## Tools

TurboVault serves 52 MCP tools; [tools.md](tools.md) lists them by category, and `tools/list` at runtime carries each one's schemas and annotations. Every vault-scoped tool takes an optional `vault` argument naming the vault to run against. Coming from 3.x, see [the 4.0 migration guide](../migration/4.0.md).

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
health = vault_health()

if health.health_score < 70:
    # Detailed analysis
    broken = graph_report(kind="broken_links")
    orphans = graph_report(kind="dead_ends")
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
