# turbovault-parser

[![Crates.io](https://img.shields.io/crates/v/turbovault-parser.svg)](https://crates.io/crates/turbovault-parser)
[![Docs.rs](https://docs.rs/turbovault-parser/badge.svg)](https://docs.rs/turbovault-parser)
[![License](https://img.shields.io/crates/l/turbovault-parser.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Obsidian Flavored Markdown (OFM) parser for extracting structured data from Obsidian vault files.

This crate provides fast, production-ready parsing of Obsidian markdown files, extracting:

- **YAML Frontmatter**: Metadata in `---` delimited blocks
- **Wikilinks**: `[[Note]]`, `[[folder/Note#Heading]]`, `[[Note#^block]]`
- **Embeds**: `![[Image.png]]`, `![[OtherNote]]`
- **Tags**: `#tag`, `#parent/child`
- **Tasks**: `- [ ] Todo`, `- [x] Done`
- **Callouts**: `> [!NOTE]`, `> [!WARNING]+`, etc.
- **Headings**: `# H1` through `###### H6` with anchor generation

Built on battle-tested libraries (`pulldown-cmark`, `nom`, `regex`) with comprehensive test coverage.

## Architecture

The parser uses a multi-pass approach:

1. **Frontmatter Extraction**: Regex-based YAML extraction from document start
2. **Link Parsing**: Wikilinks and embeds via regex with position tracking
3. **Content Elements**: Tags, tasks, callouts, headings extracted in parallel

All parsing is zero-allocation where possible, returning structured types from `turbovault-core`.

## Parsers

### Frontmatter Parser (`parsers/frontmatter_parser.rs`)

Extracts YAML metadata from the beginning of files:

```rust
use TurboVault_parser::parsers::extract_frontmatter;

let content = r#"---
title: My Note
tags: [rust, parser]
---
Content here"#;

let (frontmatter, rest) = extract_frontmatter(content)?;
assert_eq!(frontmatter, Some("title: My Note\ntags: [rust, parser]".to_string()));
assert_eq!(rest, "Content here");
```

**Features:**
- Handles multi-line YAML
- Strips frontmatter from content
- Returns `None` if no frontmatter present
- Resilient to malformed frontmatter (missing closing `---`)

### Wikilink Parser (`parsers/wikilinks.rs`)

Parses Obsidian's internal linking syntax:

```rust
use TurboVault_parser::Parser;
use std::path::PathBuf;

let parser = Parser::new(PathBuf::from("/vault"));
let content = "See [[Note#Heading]] and [[folder/SubNote]]";

// Links are extracted during parse_file()
// Supports:
// - [[Note]]                  - Basic link
// - [[folder/Note]]          - Folder paths
// - [[Note#Heading]]         - Section links
// - [[Note#^block-id]]       - Block references
// - [[Note|Display Text]]    - Custom display text (not yet implemented)
```

**Features:**
- Position tracking for each link
- Differentiates from embeds (excludes `![[...]]`)
- Captures full target path including heading/block anchors

### Embed Parser (`parsers/embeds.rs`)

Extracts embedded files and notes:

```rust
// Parses: ![[Image.png]], ![[attachments/diagram.svg]], ![[OtherNote]]
let content = "See diagram: ![[assets/architecture.png]]";

// Extracted as Link with type_ = LinkType::Embed
```

**Features:**
- Handles image embeds (`.png`, `.jpg`, `.svg`, etc.)
- Handles note embeds (transcluded content)
- Folder-aware paths

### Tag Parser (`parsers/tags.rs`)

Extracts hashtags and nested tags:

```rust
let content = "This is #rust and #project/obsidian code";

// Extracts:
// - Tag { name: "rust", is_nested: false }
// - Tag { name: "project/obsidian", is_nested: true }
```

**Features:**
- Supports flat tags: `#rust`
- Supports nested tags: `#parent/child/grandchild`
- Alphanumeric, hyphens, underscores, and slashes

### Task Parser (`parsers/tasks.rs`)

Parses task list items with completion status:

```rust
let content = r#"
- [ ] Write documentation
- [x] Implement parser
  - [ ] Nested task
"#;

// Extracts TaskItem with:
// - content: "Write documentation"
// - is_completed: false
// - position: SourcePosition
// - due_date: None (TODO)
```

**Features:**
- Uncompleted tasks: `- [ ]`
- Completed tasks: `- [x]`
- Supports indentation (nested tasks)
- Line number tracking

### Callout Parser (`parsers/callouts.rs`)

Parses Obsidian's callout/admonition syntax:

```rust
let content = r#"
> [!NOTE] Important info
> This is the callout content

> [!WARNING]- Expandable warning
> Hidden by default
"#;

// Supported types:
// NOTE, TIP, INFO, TODO, IMPORTANT, SUCCESS, QUESTION,
// WARNING, FAILURE, DANGER, BUG, EXAMPLE, QUOTE
```

**Features:**
- Type detection (maps to `CalloutType` enum)
- Optional title extraction
- Foldable callouts: `[!TYPE]+` (expanded) or `[!TYPE]-` (collapsed)
- Note: Multi-line content parsing is TODO

### Heading Parser (`parsers/headings.rs`)

Extracts markdown headings with automatic anchor generation:

```rust
let content = "# Main Title\n## Sub Section";

// Extracts:
// - Heading { text: "Main Title", level: 1, anchor: Some("main-title") }
// - Heading { text: "Sub Section", level: 2, anchor: Some("sub-section") }
```

**Features:**
- Levels 1-6 (`#` through `######`)
- Automatic anchor generation (lowercase, spaces to hyphens)
- Position tracking for TOC generation

## Usage Examples

### Example 1: Basic File Parsing

```rust
use TurboVault_parser::Parser;
use std::path::PathBuf;

let parser = Parser::new(PathBuf::from("/vault"));
let content = r#"---
title: Project Plan
tags: [project, planning]
---

# Overview

This project uses #rust and #obsidian.

## Tasks

- [ ] Set up repository
- [x] Create parser

See [[Architecture]] for details.

![[diagram.png]]
"#;

let vault_file = parser.parse_file(&PathBuf::from("Plan.md"), content)?;

// Access parsed elements:
assert_eq!(vault_file.frontmatter.unwrap().data.get("title"), Some(&json!("Project Plan")));
assert_eq!(vault_file.headings.len(), 2);
assert_eq!(vault_file.tags.len(), 2);
assert_eq!(vault_file.tasks.len(), 2);
assert_eq!(vault_file.links.len(), 2); // 1 wikilink + 1 embed
```

### Example 2: Link Extraction for Graph Building

```rust
use TurboVault_parser::Parser;
use TurboVault_core::LinkType;

let parser = Parser::new(PathBuf::from("/vault"));
let content = "See [[Note A]] and [[Note B#Section]]";
let file = parser.parse_file(&PathBuf::from("source.md"), content)?;

for link in file.links {
    match link.type_ {
        LinkType::WikiLink => println!("Link to: {}", link.target),
        LinkType::Embed => println!("Embeds: {}", link.target),
        _ => {}
    }
}
```

### Example 3: Task Tracking

```rust
let parser = Parser::new(PathBuf::from("/vault"));
let content = r#"
# Daily Tasks

- [x] Morning standup
- [ ] Code review
- [ ] Write tests
"#;

let file = parser.parse_file(&PathBuf::from("daily.md"), content)?;

let completed = file.tasks.iter().filter(|t| t.is_completed).count();
let total = file.tasks.len();
println!("Progress: {}/{} tasks completed", completed, total);
```

## Obsidian-Specific Features

This parser handles Obsidian's unique markdown extensions:

1. **Wikilinks**: Double-bracket syntax for internal linking
2. **Embeds**: `!` prefix for transclusion
3. **Block References**: `#^block-id` syntax for linking to specific blocks
4. **Nested Tags**: `/` separated hierarchical tags
5. **Callouts**: Extended blockquote syntax with type and foldability
6. **Heading Anchors**: Automatic URL-safe anchor generation

These features go beyond standard CommonMark and are essential for Obsidian vault operations.

## Integration with turbovault-core

This crate depends on `turbovault-core` for:

- **Data Models**: `VaultFile`, `Link`, `Tag`, `TaskItem`, `Callout`, `Heading`, `Frontmatter`
- **Error Types**: `Result<T, Error>` for consistent error handling
- **Configuration**: `VaultConfig` for vault-specific parsing settings
- **Type Safety**: Strong types prevent string-based API errors

The parser produces structured data that can be consumed by:
- `turbovault-vault`: Vault management and indexing
- `turbovault-server`: MCP server tool implementations
- Link graph builders and analyzers

## Development

### Running Tests

```bash
# All tests
cargo test

# Specific parser tests
cargo test --test frontmatter_parser
cargo test --test wikilinks

# With output
cargo test -- --nocapture
```

### Adding a New Parser

1. Create `src/parsers/my_parser.rs`
2. Implement parsing function returning `Vec<MyType>`
3. Add corresponding model to `turbovault-core/src/models.rs`
4. Integrate into `Parser::parse_content()` in `src/parsers.rs`
5. Add comprehensive tests

### Performance Characteristics

- **Zero-copy where possible**: Uses string slices, not copies
- **Lazy evaluation**: Only parses markdown files (`.md` extension)
- **Parallel-safe**: All parsers are stateless and thread-safe
- **Regex compilation**: Uses `lazy_static` for one-time regex compilation
- **Position tracking**: Maintains byte offsets for fast lookups

### Dependencies

- `pulldown-cmark`: CommonMark parsing foundation (currently unused, reserved for future)
- `frontmatter-gen`: Frontmatter extraction (currently using custom regex)
- `regex`: Pattern matching for Obsidian syntax
- `nom`: Parser combinators (available for complex parsing)
- `serde`, `serde_yaml`, `serde_json`: Frontmatter deserialization

## Limitations and Future Work

Current limitations:

1. **Callout Content**: Only parses first line, not continuation lines
2. **Display Text in Links**: `[[Target|Display]]` not yet parsed
3. **Markdown Metadata**: No extraction from CommonMark elements yet
4. **Dataview Queries**: Not parsed (Obsidian plugin-specific)
5. **Mermaid/Code Blocks**: Not extracted as structured data

Planned improvements:

- Multi-line callout content parsing
- Display text extraction for wikilinks
- Code block metadata (language, title)
- Better error recovery and partial parsing
- Performance benchmarking and optimization

## License

See workspace license.

## See Also

- `turbovault-core`: Core data models and types
- `turbovault-vault`: Vault management using parsed data
- `turbovault-server`: MCP server tools built on parser output