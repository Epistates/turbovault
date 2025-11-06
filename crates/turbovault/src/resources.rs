//! Static resource documentation embedded directly in code.
//! These resources are served via MCP resources and tool endpoints.

/// Obsidian Flavored Markdown (OFM) complete syntax guide
pub const OFM_SYNTAX_GUIDE: &str = r#"# Obsidian Flavored Markdown (OFM) — Complete Syntax Guide

Obsidian Flavored Markdown extends CommonMark with Obsidian conventions that power vault-centric workflows, daily notes, and Zettelkasten knowledge bases. This guide provides comprehensive OFM syntax supported by `TurboVault` so tools, agents, and humans can produce high-quality, automation-friendly notes while maintaining full compatibility with standard Markdown.

## Core Philosophy

Obsidian strives for maximum capability without breaking any existing formats. As a result, it uses a combination of flavors of Markdown to provide enhanced functionality while maintaining compatibility. Within TurboVault:

- Prefer small, atomic notes that capture a single idea and link liberally
- Keep metadata explicit in YAML frontmatter or inline attribute blocks
- Treat backlinks, tags, and properties as first-class data that power graph analytics
- Maintain human readability: all OFM constructs remain valid Markdown

## Supported Standards

- **CommonMark** (full): The standardized version of Markdown with headings, emphasis, lists, blockquotes, code, tables
- **GitHub Flavored Markdown (GFM)**: GitHub's extension of CommonMark including task lists, strikethrough, and enhanced tables
- **LaTeX / MathJax**: Mathematical notation and advanced formatting support (inline `$math$` and block `$$math$$`)
- **YAML Frontmatter**: Parsed into strongly-typed note properties
- **Mermaid Diagrams**: Fenced blocks with language identifier `mermaid`

## Important Limitation

**Obsidian does NOT support using Markdown formatting or blank lines inside of HTML tags.** This is a critical constraint to remember when working with mixed HTML/Markdown content. Always keep HTML and Markdown formatting separate.

## Internal Linking System

Wikilinks create the foundation of your knowledge graph:

- **Standard wikilink**: `[[Note Title]]` — Creates a link to another note
- **Alias wikilink**: `[[Original Note|Readable Label]]` — Links with custom display text
- **Block reference link**: `[[Note Title#^block-id]]` — Links to specific blocks within notes using block references
- **Heading reference link**: `[[Note Title#Heading]]` — Links to specific headings within notes
- **File embed**: `![[Resource Note]]`, `![[image.png]]`, `![[document.pdf]]` — Embeds files directly into notes
- **Block embed**: `![[Note Title#^block-id]]` — Embeds specific blocks from other notes
- **External embed**: `![](https://example.com/media.png)` — Uses standard Markdown spec for external resources
- **Block identifier**: `^block-id` — Define a block identifier for linking (place at end of paragraph or list item)

## Text Formatting Extensions

OFM supports all standard Markdown formatting plus these Obsidian-specific extensions:

- **Bold**: `**bold text**`
- **Italic**: `_italic text_` or `*italic text*`
- **Strikethrough**: `~~strikethrough text~~` — Show deprecated or completed information
- **Highlight**: `==highlighted text==` — Emphasize key concepts with background highlighting
- **Comments**: `%%hidden comment text%%` — Comments hidden in preview mode, useful for developer notes and TODOs
- **Footnotes**: `[^id]` — Add footnote references and citations (define with `[^id]: Footnote content` at end of note)
- **Code span**: `` `inline code` ``

## Code and Content Blocks

### Code Blocks

Use fenced blocks with language identifiers for syntax highlighting and clarity:

```rust
fn example() -> Result<(), anyhow::Error> {
    Ok(())
}
```

Preferred languages include `rust`, `bash`, `json`, `yaml`, `toml`, `mermaid`, `graphviz`, `sql`, `python`, `javascript`.

### Task Lists

- Incomplete task: `- [ ] Draft system prompt`
- Completed task: `- [x] Publish v1.0 release notes`
- Nested tasks in lists are fully supported
- Dataview-style inline fields (`key:: value`) remain intact for downstream tooling

### Callouts (Admonitions)

Create highlighted information boxes with callouts:

```
> [!info] Optional Title
> Multi-line content with Markdown support.
> - Lists work fine
> - **Formatting works too**
```

Supported callout types: `note`, `abstract`, `info`, `tip`, `success`, `question`, `warning`, `failure`, `danger`, `bug`, `example`, `quote`. Unknown identifiers render as standard blockquotes.

### Tables

Advanced table formatting with alignment support:

```
| Feature       | Status | Notes                 |
| ------------- | ------ | --------------------- |
| Wikilinks     | ✅     | Full support          |
| Block refs    | ✅     | Unique IDs required   |
| Callouts      | ✅     | 12+ types supported   |
```

## Frontmatter and Metadata

### Mandatory Frontmatter Format

Every note should include YAML frontmatter:

```yaml
---
id: 20250101-unique-id              # Stable note identifier (REQUIRED, must be unique)
title: Optional human readable title
aliases:
  - Alternate name
tags: [project/alpha, reference/ofm]
created: 2025-01-01T10:00:00Z
updated: 2025-01-02T14:35:00Z
---
```

- `id` is required and must be unique per note
- `aliases` allows multiple wikilinkable names for a single note
- `tags` accept hierarchical syntax (`area/topic/subtopic`)
- Additional fields are stored and surfaced as properties
- Use ISO-8601 timestamps for `created` and `updated` fields

### Attribute Blocks

Store additional properties using attribute blocks (parsed into note properties):

```attr
status: evergreen
owner: nick
review_cycle: quarterly
next_review: 2025-04-01
```

Prefer lower_snake_case keys. These are equivalent to frontmatter and useful for inline property management.

## Advanced Linking Patterns

### Directional Relationships

Indicate relationships between notes:

- `[[note-a]] -> [[note-b]]` — Indicates directional edge from A to B
- `[[note-a]] <-> [[note-b]]` — Bidirectional relationship
- `[[Research Plan#Milestones]]` — Reference specific sections within notes

### Hub and Spoke Patterns

- Define hub notes with `tags: [moc]` for "Map of Contents"
- Alternative: `status: hub` property
- Hub notes aggregate and organize related atomic notes

## Graph-Friendly Conventions

Build knowledge graphs that scale with your vault:

- Use ISO-8601 date format for date-based notes (`2025-01-01`) for chronological ordering
- Keep notes atomic (one main idea per note)
- Link liberally to create discovery paths
- Define hub notes (`tags: [moc]`) to organize related topics
- Use block references for precise citations within notes
- Leverage backlinks and local graph visualization

## Best Practices

### Content Organization

- Maintain one H1 (`# Title`) per note that matches the canonical title
- Prefer sentence case headings (`## Daily Log`) for clarity
- Keep paragraph line length under 120 characters for diff readability and collaboration
- Store canonical links in frontmatter `links` array when needed

### Formatting Guidelines

- Use `<!--` HTML comments `-->` sparingly (they are ignored by parsers)
- Never place Markdown formatting inside HTML tags
- Use comments (`%%text%%`) for drafting notes that shouldn't appear in final output
- Use highlights (`==text==`) to emphasize key concepts without disrupting flow
- Use strikethrough (`~~text~~`) only for showing deprecated or completed information

### Automation-Friendly Practices

- Ensure every note has unique `id` in frontmatter
- Use consistent property naming (lower_snake_case)
- Use hierarchical tags (`project/alpha`, `area/topic/subtopic`)
- Leverage block IDs (`^block-id`) for precise linking
- Keep templates and examples in dedicated notes

## Technical Implementation

- OFM is built on top of CommonMark compliance
- Extensions are implemented without breaking existing Markdown parsers
- Block references use unique identifiers for precise targeting within notes
- Internal linking system creates a graph database of note relationships
- Wikilinks are resolved through note IDs and aliases at parse time
- The graph structure enables backlink tracking and relationship discovery

## Integration with Obsidian Features

TurboVault supports Obsidian's powerful built-in features:

- **Graph View**: Internal wikilinks automatically create visual connections between notes
- **Backlinks**: Automatic tracking of all references to a note (what links to this note)
- **Local Graph**: Focused view of connections around a specific note in context
- **Unlinked Mentions**: Find potential connections between notes based on text matching
- **Search and Replace**: Full-text search across the entire vault with regex support
- **Daily Notes**: Automatic creation of date-based notes following ISO-8601 convention
- **Zettelkasten**: Support for bidirectional linking and atomic note structure

## Common Patterns

### Daily Note Template

```
# {{title}}

## Wins
- [ ]

## Tasks
- [ ]

## Notes
-

## Related
- [[Previous Day]]
- [[Next Day]]
```

### Index / Map of Contents (MOC) Pattern

```yaml
---
id: 20250101-project-moc
title: Project Alpha - Map of Contents
tags: [moc, project/alpha]
---
```

Create hub notes that organize atomic notes by topic or project.

### Zettelkasten Pattern

- Each note has a unique ID (e.g., `20250101-101`)
- Short, atomic content (one idea per note)
- Rich interlinking with wikilinks and block references
- Enables knowledge discovery and serendipitous connections

## Related Tools and Resources

- `get_ofm_syntax_guide` — This complete guide
- `get_ofm_quick_ref` — Quick reference cheat sheet
- `get_ofm_examples` — Comprehensive example note with all features
- `obsidian://syntax/complete-guide` — Resource identifier for MCP clients

## Compatibility Considerations

- All OFM features are backward compatible with standard Markdown
- Content can be exported to various formats while preserving structure
- Internal wikilinks and block references are Obsidian-specific but degrade gracefully in other systems
- Comments (`%%text%%`) and highlights (`==text==`) may not render in all Markdown viewers
- HTML tags require careful handling (no nested Markdown formatting)

Ensure your automation respects this guide before emitting OFM content in production. When in doubt, refer to the original features or use the quick reference for common patterns.
"#;

/// Quick reference for Obsidian Flavored Markdown
pub const OFM_QUICK_REFERENCE: &str = r#"# Obsidian Flavored Markdown (OFM) Quick Reference

## Wikilinks
- `[[Note Title]]` - Link to note
- `[[Note Title|Display Text]]` - Link with custom label
- `[[Note Title#Heading]]` - Link to specific heading
- `[[Note Title#^block-id]]` - Link to specific block

## Text Formatting
- `**bold**` - Bold text
- `_italic_` or `*italic*` - Italic text
- `~~strikethrough~~` - Strikethrough text
- `==highlight==` - Highlighted text
- `` `code` `` - Inline code
- `%%comment%%` - Hidden comment

## Lists & Tasks
- `- item` - Unordered list
- `1. item` - Ordered list
- `- [ ] task` - Incomplete task
- `- [x] task` - Completed task

## Code Blocks
\`\`\`language
code here
\`\`\`

## Callouts (Admonitions)
\`\`\`
> [!type] Title
> Content here
\`\`\`

Supported types: `note`, `abstract`, `info`, `tip`, `success`, `question`, `warning`, `failure`, `danger`, `bug`, `example`, `quote`

## Tables
```
| Column 1 | Column 2 |
|----------|----------|
| Cell 1   | Cell 2   |
```

## Embeds & Media
- `![[Note Title]]` - Embed note
- `![[image.png]]` - Embed image
- `![[File.pdf]]` - Embed file

## Frontmatter (YAML)
```yaml
---
id: unique-identifier
title: Note Title
tags: [tag1, tag2]
aliases: [alt-name]
---
```

## Block References
- `^block-id` - Create block reference
- `[[Note#^block-id]]` - Link to block

## HTML & Comments
- `<!-- HTML comment -->` - Standard HTML comment
- Mix Markdown and HTML, but never nest Markdown inside HTML tags
"#;

/// Example note demonstrating OFM features
pub const OFM_EXAMPLE_NOTE: &str = r#"---
id: 20250106-ofm-example
title: Obsidian Flavored Markdown - Example Note
aliases: [OFM Example, Example Note]
tags: [example, reference/ofm, tutorial]
created: 2025-01-06T10:00:00Z
updated: 2025-01-06T14:30:00Z
---

# Obsidian Flavored Markdown - Example Note

This note demonstrates all major OFM features supported by TurboVault.

## Text Formatting

Here's a sentence with **bold text**, _italic text_, and ~~strikethrough~~. You can also use ==highlighted text== to emphasize important concepts, and `` `inline code` `` for technical terms.

%%This is a hidden comment that won't appear in preview mode. Great for drafting and internal notes!%%

## Wikilinks and References

Link to other notes using wikilinks:
- [[Example Note]] - Standard link
- [[Daily Note|Link with custom label]] - Link with custom display text
- [[Archive/Old Note#Findings]] - Link to specific heading
- [[Research#^important-finding]] - Link to specific block

## Embeds

You can embed entire notes:
![[Another Note]]

Or specific blocks:
![[Archive/Old Note#^block-id]]

Or images and media:
![[sample-image.png]]

## Lists and Tasks

### Unordered List
- First item
- Second item
  - Nested item
  - Another nested item
- Third item

### Ordered List
1. First step
2. Second step
   1. Sub-step A
   2. Sub-step B
3. Third step

### Task List
- [x] Completed task
- [ ] Incomplete task
- [ ] Another task
  - [x] Sub-task complete
  - [ ] Sub-task incomplete

## Code Blocks

Here's a Rust example:

```rust
fn example() -> Result<(), Box<dyn std::error::Error>> {
    println!("Hello, world!");
    Ok(())
}
```

JavaScript example:
```javascript
function greet(name) {
  return `Hello, ${name}!`;
}
```

SQL example:
```sql
SELECT id, title, created FROM notes
WHERE tags LIKE '%example%'
ORDER BY created DESC;
```

## Callouts (Admonitions)

> [!note] This is a note
> A note callout for general information.

> [!tip] Pro Tip
> A tip callout with helpful advice.

> [!warning] Be Careful
> A warning callout for important cautions.

> [!example] Example Callout
> This demonstrates an example callout type.

## Tables

| Feature | Status | Notes |
|---------|--------|-------|
| Wikilinks | ✅ | Full support |
| Embeds | ✅ | Files and blocks |
| Callouts | ✅ | 12+ types |
| Frontmatter | ✅ | YAML format |
| Math | ✅ | LaTeX notation |
| Tasks | ✅ | Nested support |

## Block References

This paragraph has a block reference identifier. ^example-block

You can link to this block using `[[OFM Example Note#^example-block]]`

## Nested Content Structure

### Section 1
Content for section 1.

### Section 2
Content for section 2.

#### Subsection 2.1
Nested content under section 2.

## Graphs and Diagrams

```mermaid
graph LR
    A[Start] --> B{Decision}
    B -->|Yes| C[Action 1]
    B -->|No| D[Action 2]
    C --> E[End]
    D --> E
```

## Related Notes

Links to other example content:
- [[Daily Note Template]]
- [[Map of Contents Example]]
- [[Zettelkasten Pattern]]

---

**Created:** 2025-01-06 | **Updated:** 2025-01-06 14:30 | **Version:** 1.0
"#;
