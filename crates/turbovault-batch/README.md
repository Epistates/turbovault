# turbovault-batch

[![Crates.io](https://img.shields.io/crates/v/turbovault-batch.svg)](https://crates.io/crates/turbovault-batch)
[![Docs.rs](https://docs.rs/turbovault-batch/badge.svg)](https://docs.rs/turbovault-batch)
[![License](https://img.shields.io/crates/l/turbovault-batch.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Atomic, transactional batch file operations for Obsidian vaults.

This crate provides ACID-like transaction support for multi-file operations, ensuring vault integrity through atomic commits and rollback capabilities. It's designed for complex operations that need to modify multiple files while maintaining consistency.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    BatchTransaction                        │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │              Operations Queue                           │ │
│  │  - CreateFile                                          │ │
│  │  - WriteFile                                           │ │
│  │  - DeleteFile                                          │ │
│  │  - MoveFile                                            │ │
│  │  - UpdateLinks                                         │ │
│  └─────────────────────────────────────────────────────────┘ │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │              Conflict Detection                         │ │
│  │  - File existence checks                               │ │
│  │  - Path collision detection                            │ │
│  │  - Dependency validation                               │ │
│  └─────────────────────────────────────────────────────────┘ │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │              Atomic Execution                           │ │
│  │  - All operations succeed OR all fail                  │ │
│  │  - Rollback on any failure                              │ │
│  │  - Temporary file management                            │ │
│  └─────────────────────────────────────────────────────────┘
```

**Design Philosophy:**
- **ACID Compliance**: All operations succeed or all fail
- **Conflict Detection**: Pre-validate operations before execution
- **Rollback Safety**: Original state preserved on failure
- **Thread Safety**: Multiple transactions can run concurrently
- **Performance**: Batch operations are faster than individual operations

## Supported Operations

### CreateFile

Creates a new file with specified content:

```rust
use TurboVault_batch::{BatchTransaction, CreateFile};

let mut transaction = BatchTransaction::new();

// Create a new note
transaction.create_file(CreateFile {
    path: "Projects/RustProject.md".to_string(),
    content: "# Rust Project\n\nThis is a new project.".to_string(),
    frontmatter: Some(json!({
        "title": "Rust Project",
        "status": "active",
        "tags": ["rust", "project"]
    })),
})?;

// Execute the transaction
transaction.execute()?;
```

**Features:**
- **Path Validation**: Ensures path doesn't already exist
- **Content Validation**: Validates markdown syntax
- **Frontmatter Support**: Optional YAML frontmatter
- **Directory Creation**: Automatically creates parent directories

### WriteFile

Updates an existing file with new content:

```rust
use TurboVault_batch::{BatchTransaction, WriteFile};

let mut transaction = BatchTransaction::new();

// Update existing file
transaction.write_file(WriteFile {
    path: "Projects/RustProject.md".to_string(),
    content: "# Rust Project\n\nUpdated content here.".to_string(),
    frontmatter: Some(json!({
        "title": "Rust Project",
        "status": "completed",
        "tags": ["rust", "project", "completed"]
    })),
})?;

transaction.execute()?;
```

**Features:**
- **Existence Check**: Validates file exists before writing
- **Content Validation**: Ensures valid markdown
- **Frontmatter Merge**: Merges with existing frontmatter
- **Backup Creation**: Creates backup before modification

### DeleteFile

Removes a file and updates all references:

```rust
use TurboVault_batch::{BatchTransaction, DeleteFile};

let mut transaction = BatchTransaction::new();

// Delete file and update references
transaction.delete_file(DeleteFile {
    path: "Projects/OldProject.md".to_string(),
    update_references: true, // Remove links to this file
})?;

transaction.execute()?;
```

**Features:**
- **Reference Cleanup**: Removes links to deleted file
- **Backlink Updates**: Updates files that link to deleted file
- **Safety Check**: Prevents deletion of critical files
- **Backup Creation**: Creates backup before deletion

### MoveFile

Moves a file to a new location and updates all references:

```rust
use TurboVault_batch::{BatchTransaction, MoveFile};

let mut transaction = BatchTransaction::new();

// Move file and update references
transaction.move_file(MoveFile {
    from_path: "Projects/ActiveProject.md".to_string(),
    to_path: "Archive/CompletedProject.md".to_string(),
    update_references: true, // Update all links to this file
})?;

transaction.execute()?;
```

**Features:**
- **Path Validation**: Ensures destination doesn't exist
- **Reference Updates**: Updates all links to moved file
- **Directory Creation**: Creates destination directory if needed
- **Atomic Move**: Uses filesystem atomic operations where possible

### UpdateLinks

Updates links in a file to point to new locations:

```rust
use TurboVault_batch::{BatchTransaction, UpdateLinks};

let mut transaction = BatchTransaction::new();

// Update links in a file
transaction.update_links(UpdateLinks {
    file_path: "Index.md".to_string(),
    link_updates: vec![
        LinkUpdate {
            old_target: "Projects/OldProject.md".to_string(),
            new_target: "Archive/CompletedProject.md".to_string(),
        },
        LinkUpdate {
            old_target: "Notes/TempNote.md".to_string(),
            new_target: "Notes/PermanentNote.md".to_string(),
        },
    ],
})?;

transaction.execute()?;
```

**Features:**
- **Bulk Updates**: Updates multiple links in one operation
- **Validation**: Ensures new targets exist
- **Link Types**: Handles wikilinks, embeds, and references
- **Content Preservation**: Maintains link display text

## Transaction Semantics

### Atomic Execution

All operations in a transaction are executed atomically:

```rust
let mut transaction = BatchTransaction::new();

// Multiple operations
transaction.create_file(create_op)?;
transaction.write_file(write_op)?;
transaction.move_file(move_op)?;

// Either ALL succeed or ALL fail
match transaction.execute() {
    Ok(_) => println!("All operations completed successfully"),
    Err(e) => {
        println!("Transaction failed: {}", e);
        // All changes have been rolled back
        // Vault is in original state
    }
}
```

### Conflict Detection

Operations are validated before execution:

```rust
let mut transaction = BatchTransaction::new();

// This will fail during validation
transaction.create_file(CreateFile {
    path: "ExistingFile.md".to_string(),
    content: "New content".to_string(),
    frontmatter: None,
})?;

// Validation happens here
match transaction.execute() {
    Err(Error::Conflict(conflict)) => {
        println!("Conflict detected: {}", conflict);
        // No files were modified
    }
    _ => {}
}
```

**Conflict Types:**
- **File Exists**: Trying to create a file that already exists
- **File Missing**: Trying to modify a file that doesn't exist
- **Path Collision**: Multiple operations target the same path
- **Circular Dependency**: Operations that depend on each other

### Rollback Mechanism

On failure, all changes are rolled back:

```rust
let mut transaction = BatchTransaction::new();

// Add operations
transaction.create_file(create_op)?;
transaction.write_file(write_op)?; // This might fail

match transaction.execute() {
    Ok(_) => {
        // All operations succeeded
        println!("Transaction completed");
    }
    Err(e) => {
        // All operations were rolled back
        println!("Transaction failed: {}", e);
        // Vault is in original state
        // No partial changes remain
    }
}
```

**Rollback Process:**
1. **Stop Execution**: Halt on first failure
2. **Reverse Operations**: Undo completed operations in reverse order
3. **Restore Backups**: Restore original file content
4. **Cleanup**: Remove temporary files and directories
5. **Verify State**: Ensure vault is in original state

## Error Handling

### Error Types

```rust
use TurboVault_batch::Error;

match transaction.execute() {
    Err(Error::Conflict(conflict)) => {
        // Pre-execution validation failed
        println!("Conflict: {}", conflict);
    }
    Err(Error::IoError(io_error)) => {
        // Filesystem operation failed
        println!("IO Error: {}", io_error);
    }
    Err(Error::ValidationError(validation)) => {
        // Content validation failed
        println!("Validation Error: {}", validation);
    }
    Err(Error::RollbackError(rollback)) => {
        // Rollback failed (critical error)
        println!("Rollback Error: {}", rollback);
    }
    Ok(_) => {
        // Transaction completed successfully
    }
}
```

### Recovery Strategies

```rust
// Retry with conflict resolution
let mut transaction = BatchTransaction::new();

match transaction.execute() {
    Err(Error::Conflict(conflict)) => {
        // Resolve conflict and retry
        match resolve_conflict(conflict) {
            Ok(resolved_op) => {
                transaction.add_operation(resolved_op)?;
                transaction.execute()?; // Retry
            }
            Err(e) => {
                // Manual intervention required
                println!("Manual resolution needed: {}", e);
            }
        }
    }
    _ => {}
}
```

## Performance Characteristics

### Batch vs Individual Operations

```rust
// Individual operations (slower)
for file in files {
    vault_manager.write_file(&file.path, &file.content)?;
    vault_manager.update_links(&file.path, &file.link_updates)?;
}

// Batch operations (faster)
let mut transaction = BatchTransaction::new();
for file in files {
    transaction.write_file(WriteFile {
        path: file.path.clone(),
        content: file.content.clone(),
        frontmatter: file.frontmatter.clone(),
    })?;
    transaction.update_links(UpdateLinks {
        file_path: file.path.clone(),
        link_updates: file.link_updates.clone(),
    })?;
}
transaction.execute()?; // Single atomic operation
```

**Performance Benefits:**
- **Reduced I/O**: Fewer filesystem operations
- **Bulk Validation**: Single validation pass
- **Atomic Commits**: No intermediate states
- **Optimized Rollback**: Efficient cleanup on failure

### Memory Usage

- **Operation Queue**: ~100 bytes per operation
- **Conflict Detection**: ~50 bytes per operation
- **Rollback State**: ~200 bytes per operation
- **Total**: ~350 bytes per operation
- **Example**: 100 operations ≈ 35KB memory

### Thread Safety

- **Concurrent Transactions**: Multiple transactions can run simultaneously
- **No Shared State**: Each transaction is independent
- **File Locking**: Prevents concurrent modification of same files
- **Atomic Operations**: Uses filesystem atomic operations where possible

## Usage Examples

### Example 1: Project Migration

```rust
use TurboVault_batch::{BatchTransaction, MoveFile, UpdateLinks};

// Move project files to archive
let mut transaction = BatchTransaction::new();

// Move main project file
transaction.move_file(MoveFile {
    from_path: "Projects/ActiveProject.md".to_string(),
    to_path: "Archive/CompletedProject.md".to_string(),
    update_references: true,
})?;

// Move related files
transaction.move_file(MoveFile {
    from_path: "Projects/ActiveProject/Tasks.md".to_string(),
    to_path: "Archive/CompletedProject/Tasks.md".to_string(),
    update_references: true,
})?;

// Update project index
transaction.update_links(UpdateLinks {
    file_path: "Projects/Index.md".to_string(),
    link_updates: vec![LinkUpdate {
        old_target: "Projects/ActiveProject.md".to_string(),
        new_target: "Archive/CompletedProject.md".to_string(),
    }],
})?;

// Execute all operations atomically
transaction.execute()?;
println!("Project migration completed successfully");
```

### Example 2: Bulk Note Creation

```rust
use TurboVault_batch::{BatchTransaction, CreateFile};

// Create multiple related notes
let mut transaction = BatchTransaction::new();

let notes = vec![
    ("Projects/NewProject.md", "# New Project\n\nProject overview."),
    ("Projects/NewProject/Tasks.md", "# Tasks\n\n- [ ] Task 1\n- [ ] Task 2"),
    ("Projects/NewProject/Notes.md", "# Notes\n\nProject notes and ideas."),
];

for (path, content) in notes {
    transaction.create_file(CreateFile {
        path: path.to_string(),
        content: content.to_string(),
        frontmatter: Some(json!({
            "title": path.split('/').last().unwrap(),
            "project": "NewProject",
            "created": chrono::Utc::now().to_rfc3339()
        })),
    })?;
}

// Create project index
transaction.create_file(CreateFile {
    path: "Projects/NewProject/Index.md".to_string(),
    content: "# New Project Index\n\n- [[Tasks]]\n- [[Notes]]".to_string(),
    frontmatter: None,
})?;

transaction.execute()?;
println!("Bulk note creation completed");
```

### Example 3: Vault Cleanup

```rust
use TurboVault_batch::{BatchTransaction, DeleteFile, UpdateLinks};

// Clean up old files and update references
let mut transaction = BatchTransaction::new();

let files_to_delete = vec![
    "Temp/ScratchNote.md",
    "Temp/OldDraft.md",
    "Temp/TestNote.md",
];

// Delete files and update references
for file_path in files_to_delete {
    transaction.delete_file(DeleteFile {
        path: file_path.to_string(),
        update_references: true,
    })?;
}

// Update main index to remove references
transaction.update_links(UpdateLinks {
    file_path: "Index.md".to_string(),
    link_updates: vec![
        LinkUpdate {
            old_target: "Temp/ScratchNote.md".to_string(),
            new_target: "".to_string(), // Remove link
        },
        LinkUpdate {
            old_target: "Temp/OldDraft.md".to_string(),
            new_target: "".to_string(), // Remove link
        },
    ],
})?;

transaction.execute()?;
println!("Vault cleanup completed");
```

## Integration Points

### With turbovault-vault

```rust
// Vault manager provides transaction support
let transaction = vault_manager.create_transaction()?;
transaction.add_operation(operation)?;
vault_manager.execute_transaction(transaction)?;
```

### With turbovault-server

```rust
// MCP tools use batch operations for complex tasks
let result = batch_execute(operations)?;
// Returns: BatchResult with success/failure details
```

### With turbovault-tools

```rust
// Tools layer orchestrates batch operations
let batch_tools = BatchTools::new(vault_manager);
let result = batch_tools.execute_batch(operations)?;
```

## Development

### Running Tests

```bash
# All tests
cargo test

# Specific test categories
cargo test --test transaction_semantics
cargo test --test conflict_detection
cargo test --test rollback_mechanism

# With output
cargo test -- --nocapture
```

### Adding New Operations

1. **Define Operation Struct**: Add to `src/lib.rs`
2. **Implement Validation**: Add conflict detection logic
3. **Implement Execution**: Add operation execution logic
4. **Implement Rollback**: Add rollback logic
5. **Add Tests**: Comprehensive test coverage

### Performance Testing

```bash
# Benchmark batch operations
cargo bench

# Test with large batches
cargo test --test large_batch -- --nocapture

# Memory usage testing
cargo test --test memory_usage -- --nocapture
```

## Dependencies

- **turbovault-core**: Core data models and error types
- **turbovault-vault**: Vault management and file operations
- **serde**: Serialization for operation data
- **serde_json**: JSON handling for frontmatter
- **tokio**: Async runtime for file operations

## Design Decisions

### Why ACID Transactions?

- **Vault Integrity**: Prevents partial updates that break vault
- **Error Recovery**: Easy rollback on failure
- **Consistency**: Vault always in valid state
- **Performance**: Batch operations are more efficient
- **Reliability**: Production-grade error handling

### Why Pre-Validation?

- **Early Failure**: Detect conflicts before any changes
- **Performance**: Avoid expensive rollback operations
- **User Experience**: Clear error messages before execution
- **Debugging**: Easier to identify conflict sources

### Why Immutable Operations?

- **Thread Safety**: No shared mutable state
- **Predictability**: Operations don't change during execution
- **Testing**: Easier to test and reason about
- **Performance**: No locking or synchronization needed

## License

See workspace license.

## See Also

- `turbovault-core`: Core data models and error types
- `turbovault-vault`: Vault management and file operations
- `turbovault-server`: MCP server tools using batch operations
- `turbovault-tools`: Tools layer orchestrating batch operations