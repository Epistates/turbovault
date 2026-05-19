# Obsidian Flavored Markdown Reference

Reference for Obsidian Flavored Markdown (OFM) syntax currently parsed or classified by TurboVault.

## Overview

TurboVault parses a focused OFM subset for vault analysis: links, embeds, frontmatter, headings, tags, tasks, callouts, tables, code blocks, and link type classification. Some Obsidian syntax is accepted as plain Markdown text but is not yet extracted as first-class structured data.

## Core Syntax

### Text Formatting

```markdown
**bold**
*italic*
~~strikethrough~~
`code`
```

Not yet extracted as first-class parser nodes: highlights `==text==`, comments `%%hidden%%`, and math/LaTeX.

### Wikilinks

```markdown
[[Note Name]]
[[Note Name|Alias]]
[[Note Name#Heading]]
[[Note Name#^block-id]]
![[image.png]]
![[Note Name#section]]
```

### Link Type Classification

TurboVault classifies links into specific types for graph analysis:

| Syntax | LinkType | Description |
|--------|----------|-------------|
| `[[Note]]` | `WikiLink` | Basic wikilink |
| `[[Note#Heading]]` | `HeadingRef` | Cross-file heading reference |
| `[[Note#^blockid]]` | `BlockRef` | Block reference |
| `[[#Heading]]` | `Anchor` | Same-document heading anchor |
| `[[#^blockid]]` | `BlockRef` | Same-document block reference |
| `![[Note]]` | `Embed` | Embedded content |
| `[text](./file.md)` | `MarkdownLink` | Standard markdown link to file |
| `[text](file.md#section)` | `HeadingRef` | Markdown link with heading |
| `[text](#section)` | `Anchor` | Same-document anchor (markdown) |
| `[text](https://...)` | `ExternalLink` | External URL |

### Callouts

```markdown
> [!note] Title
> Content here

> [!warning]
> Multiple lines
> Of content
```

Recognized structured types: note, tip, info, todo, important, success, question, warning, failure, danger, bug, example, quote. Aliases include fail, missing, error, and cite. Other Obsidian callout identifiers are accepted syntactically but currently map to note.

### Task Lists

```markdown
- [ ] Incomplete
- [x] Complete
- [/] In progress
- [-] Cancelled
```

### Frontmatter

```yaml
---
id: unique-id
title: Note Title
tags: [tag1, tag2]
---
```

## Advanced Features

### Block References

```markdown
^block-id  # Define at end of content block
[[Note#^block-id]]  # Reference in wikilink
```

TurboVault classifies block reference links. Block reference definitions are not yet represented as first-class content block nodes.

### Attributes

```markdown
```attr
status: evergreen
owner: name
```
```

Attribute code blocks are currently treated as code blocks, not parsed as typed attributes.

### Callout Variations

- `note` - Default note
- `warning` - Important warning
- `tip` - Helpful tip
- `example` - Example content
- `error` - Error message

## Tables

```markdown
| Header 1 | Header 2 |
|----------|----------|
| Cell 1   | Cell 2   |
```

## Code Blocks

````markdown
```rust
fn main() {
    println!("Hello");
}
```
````

## See Also

- [OFM System Prompt](../../resources/obsidian_flavored_markdown_system_prompt.md)
- [Main Documentation](../README.md)
