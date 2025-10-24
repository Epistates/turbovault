# turbovault-export

[![Crates.io](https://img.shields.io/crates/v/turbovault-export.svg)](https://crates.io/crates/turbovault-export)
[![Docs.rs](https://docs.rs/turbovault-export/badge.svg)](https://docs.rs/turbovault-export)
[![License](https://img.shields.io/crates/l/turbovault-export.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Data export functionality for vault analysis in multiple formats.

This crate provides comprehensive export capabilities for Obsidian vault data, enabling downstream analysis, reporting, and integration with external tools. It supports multiple output formats and export types for different use cases.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                    ExportEngine                             │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │              Format Handlers                           │ │
│  │  - JSON Exporter                                        │ │
│  │  - CSV Exporter                                         │ │
│  │  - Markdown Exporter                                    │ │
│  │  - XML Exporter                                         │ │
│  └─────────────────────────────────────────────────────────┘ │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │              Export Types                              │ │
│  │  - Health Report                                       │ │
│  │  - Broken Links Report                                 │ │
│  │  - Vault Statistics                                   │ │
│  │  - Analysis Report                                     │ │
│  └─────────────────────────────────────────────────────────┘ │
│                                                             │
│  ┌─────────────────────────────────────────────────────────┐ │
│  │              Data Sources                              │ │
│  │  - VaultManager                                        │ │
│  │  - ObsidianVaultGraph                                  │ │
│  │  - SearchEngine                                        │ │
│  └─────────────────────────────────────────────────────────┘ │
└─────────────────────────────────────────────────────────────┘
```

**Design Philosophy:**
- **Format Agnostic**: Supports multiple output formats
- **Data Rich**: Exports comprehensive vault information
- **Performance**: Efficient streaming for large vaults
- **Extensible**: Easy to add new formats and export types
- **Thread Safe**: Concurrent export operations

## Supported Formats

### JSON Export

Structured data export for programmatic consumption:

```rust
use TurboVault_export::{ExportEngine, ExportFormat, ExportType};

let engine = ExportEngine::new(vault_manager, graph, search_engine)?;

// Export health report as JSON
let json_data = engine.export(ExportType::HealthReport, ExportFormat::Json)?;
// Returns: serde_json::Value with structured health data

// Export vault statistics as JSON
let stats_data = engine.export(ExportType::VaultStatistics, ExportFormat::Json)?;
// Returns: VaultStatistics struct serialized to JSON
```

**JSON Structure Example:**
```json
{
  "export_type": "health_report",
  "timestamp": "2024-01-15T10:30:00Z",
  "vault_path": "/path/to/vault",
  "health_score": 85,
  "issues": [
    {
      "type": "orphaned_files",
      "count": 25,
      "files": ["Temp/ScratchNote.md", "Temp/OldDraft.md"]
    },
    {
      "type": "broken_links",
      "count": 5,
      "links": [
        {
          "file": "Index.md",
          "line": 15,
          "target": "NonExistentNote.md",
          "link_type": "wikilink"
        }
      ]
    }
  ],
  "recommendations": [
    "Consider linking orphaned files to main content",
    "Fix broken links to improve vault connectivity"
  ]
}
```

### CSV Export

Tabular data export for spreadsheet analysis:

```rust
// Export broken links as CSV
let csv_data = engine.export(ExportType::BrokenLinksReport, ExportFormat::Csv)?;
// Returns: String with CSV data

// Export vault statistics as CSV
let stats_csv = engine.export(ExportType::VaultStatistics, ExportFormat::Csv)?;
// Returns: String with CSV data
```

**CSV Structure Example:**
```csv
file_path,line_number,target,link_type,severity
Index.md,15,NonExistentNote.md,wikilink,high
Projects/ProjectA.md,8,MissingImage.png,embed,medium
Notes/NoteB.md,22,OldNote.md,wikilink,low
```

### Markdown Export

Human-readable reports in markdown format:

```rust
// Export analysis report as markdown
let markdown_data = engine.export(ExportType::AnalysisReport, ExportFormat::Markdown)?;
// Returns: String with markdown content
```

**Markdown Structure Example:**
```markdown
# Vault Analysis Report

**Generated:** 2024-01-15T10:30:00Z  
**Vault:** /path/to/vault  
**Health Score:** 85/100

## Overview

This vault contains 1,250 files with 3,400 links. The overall health score is 85/100.

## Issues Found

### Orphaned Files (25 files)
- `Temp/ScratchNote.md`
- `Temp/OldDraft.md`
- `Notes/UnlinkedNote.md`

### Broken Links (5 links)
- `Index.md:15` → `NonExistentNote.md` (wikilink)
- `Projects/ProjectA.md:8` → `MissingImage.png` (embed)

## Recommendations

1. Consider linking orphaned files to main content
2. Fix broken links to improve vault connectivity
3. Review hub notes for better organization
```

## Export Types

### Health Report

Comprehensive vault health analysis:

```rust
use TurboVault_export::{ExportEngine, ExportType, ExportFormat};

let engine = ExportEngine::new(vault_manager, graph, search_engine)?;

// Export health report
let health_report = engine.export(ExportType::HealthReport, ExportFormat::Json)?;

// Health report includes:
// - Overall health score (0-100)
// - Detailed issue breakdown
// - Orphaned files list
// - Broken links with locations
// - Cycle detection results
// - Hub note analysis
// - Recommendations for improvement
```

**Health Report Data:**
- **Overall Score**: 0-100 health rating
- **Issues**: Categorized problems (orphans, broken links, cycles)
- **Statistics**: File counts, link counts, connectivity metrics
- **Recommendations**: Actionable improvement suggestions

### Broken Links Report

Detailed analysis of broken links:

```rust
// Export broken links report
let broken_links = engine.export(ExportType::BrokenLinksReport, ExportFormat::Csv)?;

// Broken links report includes:
// - File path and line number
// - Target that doesn't exist
// - Link type (wikilink, embed, reference)
// - Severity level (high, medium, low)
// - Suggested fixes
```

**Broken Links Data:**
- **Location**: File path and line number
- **Target**: Missing file or resource
- **Type**: Wikilink, embed, or reference
- **Severity**: Impact level for prioritization
- **Context**: Surrounding content for context

### Vault Statistics

Comprehensive vault metrics and analytics:

```rust
// Export vault statistics
let stats = engine.export(ExportType::VaultStatistics, ExportFormat::Json)?;

// Vault statistics include:
// - File counts by type
// - Link counts by type
// - Graph metrics (density, connectivity)
// - Content analysis (word counts, tag usage)
// - Growth trends over time
// - Performance metrics
```

**Statistics Data:**
- **File Metrics**: Total files, by type, by folder
- **Link Metrics**: Total links, by type, connectivity
- **Graph Metrics**: Density, components, cycles
- **Content Metrics**: Word counts, tag usage, frontmatter
- **Performance**: Search times, graph build times

### Analysis Report

Detailed vault analysis with insights:

```rust
// Export analysis report
let analysis = engine.export(ExportType::AnalysisReport, ExportFormat::Markdown)?;

// Analysis report includes:
// - Executive summary
// - Detailed findings
// - Trend analysis
// - Comparative metrics
// - Actionable recommendations
// - Future outlook
```

**Analysis Data:**
- **Summary**: High-level findings and trends
- **Findings**: Detailed analysis results
- **Trends**: Changes over time
- **Comparisons**: Benchmarks and standards
- **Recommendations**: Specific action items
- **Outlook**: Future considerations

## Usage Examples

### Example 1: Health Dashboard Export

```rust
use TurboVault_export::{ExportEngine, ExportType, ExportFormat};

let engine = ExportEngine::new(vault_manager, graph, search_engine)?;

// Export health report for dashboard
let health_data = engine.export(ExportType::HealthReport, ExportFormat::Json)?;

// Save to file
std::fs::write("health_report.json", health_data.to_string())?;

// Also export as CSV for spreadsheet analysis
let health_csv = engine.export(ExportType::HealthReport, ExportFormat::Csv)?;
std::fs::write("health_report.csv", health_csv)?;

println!("Health reports exported successfully");
```

### Example 2: Broken Links Analysis

```rust
// Export broken links for manual review
let broken_links = engine.export(ExportType::BrokenLinksReport, ExportFormat::Csv)?;

// Parse CSV for processing
let mut reader = csv::Reader::from_reader(broken_links.as_bytes());
for result in reader.records() {
    let record = result?;
    let file_path = &record[0];
    let line_number = record[1].parse::<u32>()?;
    let target = &record[2];
    let link_type = &record[3];
    let severity = &record[4];
    
    println!("{}:{} → {} ({}) - {}", file_path, line_number, target, link_type, severity);
}
```

### Example 3: Vault Statistics Export

```rust
// Export comprehensive statistics
let stats = engine.export(ExportType::VaultStatistics, ExportFormat::Json)?;

// Parse and analyze statistics
let stats_data: serde_json::Value = serde_json::from_str(&stats.to_string())?;

let total_files = stats_data["file_counts"]["total"].as_u64().unwrap();
let total_links = stats_data["link_counts"]["total"].as_u64().unwrap();
let health_score = stats_data["health_score"].as_u64().unwrap();

println!("Vault Statistics:");
println!("  Files: {}", total_files);
println!("  Links: {}", total_links);
println!("  Health: {}/100", health_score);

// Export as markdown for documentation
let stats_md = engine.export(ExportType::VaultStatistics, ExportFormat::Markdown)?;
std::fs::write("vault_stats.md", stats_md)?;
```

### Example 4: Automated Reporting

```rust
use chrono::Utc;

// Generate daily health report
let engine = ExportEngine::new(vault_manager, graph, search_engine)?;

let timestamp = Utc::now().format("%Y-%m-%d").to_string();
let report_name = format!("health_report_{}.json", timestamp);

// Export health report
let health_data = engine.export(ExportType::HealthReport, ExportFormat::Json)?;
std::fs::write(&report_name, health_data.to_string())?;

// Export analysis report
let analysis_data = engine.export(ExportType::AnalysisReport, ExportFormat::Markdown)?;
let analysis_name = format!("analysis_report_{}.md", timestamp);
std::fs::write(&analysis_name, analysis_data)?;

println!("Daily reports generated: {} and {}", report_name, analysis_name);
```

## Integration Points

### With turbovault-vault

```rust
// Vault manager provides file data for export
let vault_manager = VaultManager::new(config)?;
let engine = ExportEngine::new(vault_manager, graph, search_engine)?;
```

### With turbovault-graph

```rust
// Graph provides link analysis data
let graph = ObsidianVaultGraph::from_files(files)?;
let engine = ExportEngine::new(vault_manager, graph, search_engine)?;
```

### With turbovault-server

```rust
// MCP server provides export tools
let export_tools = ExportTools::new(vault_manager, graph, search_engine)?;
let result = export_tools.export_health_report(ExportFormat::Json)?;
```

### With turbovault-tools

```rust
// Tools layer orchestrates export operations
let tools = MCPTools::new(vault_manager, graph, search_engine)?;
let export_result = tools.export_data(ExportType::HealthReport, ExportFormat::Json)?;
```

## Performance Characteristics

### Memory Usage

- **Streaming Export**: Processes data in chunks to minimize memory usage
- **Format Buffers**: Small buffers for output formatting
- **Data Structures**: Efficient serialization with minimal overhead
- **Total**: ~1MB for typical vault exports

### Export Performance

- **JSON Export**: ~50ms for 1k files
- **CSV Export**: ~30ms for 1k files
- **Markdown Export**: ~100ms for 1k files
- **Large Vaults**: Scales linearly with file count

### Thread Safety

- **Concurrent Exports**: Multiple export operations can run simultaneously
- **Read-Only Operations**: No shared mutable state
- **Format Handlers**: Stateless and thread-safe
- **Data Sources**: Read-only access to vault data

## Development

### Running Tests

```bash
# All tests
cargo test

# Specific test categories
cargo test --test json_export
cargo test --test csv_export
cargo test --test markdown_export

# With output
cargo test -- --nocapture
```

### Adding New Formats

1. **Define Format Handler**: Implement `FormatHandler` trait
2. **Add Format Enum**: Add to `ExportFormat` enum
3. **Implement Serialization**: Add format-specific serialization
4. **Add Tests**: Comprehensive test coverage
5. **Update Documentation**: Add usage examples

### Adding New Export Types

1. **Define Export Type**: Add to `ExportType` enum
2. **Implement Data Collection**: Gather required data from sources
3. **Add Format Support**: Implement for all supported formats
4. **Add Tests**: Test with sample data
5. **Update Documentation**: Add usage examples

## Dependencies

- **turbovault-core**: Core data models and error types
- **turbovault-vault**: Vault management and file operations
- **turbovault-graph**: Link graph analysis
- **serde**: Serialization framework
- **serde_json**: JSON serialization
- **csv**: CSV serialization
- **chrono**: Timestamp handling

## Design Decisions

### Why Multiple Formats?

- **Different Use Cases**: JSON for programs, CSV for spreadsheets, Markdown for humans
- **Integration**: Easy integration with external tools
- **Flexibility**: Users can choose appropriate format
- **Standards**: Common formats for data exchange

### Why Streaming Export?

- **Memory Efficiency**: Handle large vaults without memory issues
- **Performance**: Faster processing for large datasets
- **Scalability**: Linear scaling with vault size
- **Reliability**: Less likely to fail on large exports

### Why Read-Only Operations?

- **Thread Safety**: No synchronization needed
- **Performance**: No locking overhead
- **Reliability**: No risk of data corruption
- **Simplicity**: Easier to reason about and test

## License

See workspace license.

## See Also

- `turbovault-core`: Core data models and error types
- `turbovault-vault`: Vault management and file operations
- `turbovault-graph`: Link graph analysis for export data
- `turbovault-server`: MCP server tools using export functionality
- `turbovault-tools`: Tools layer orchestrating export operations