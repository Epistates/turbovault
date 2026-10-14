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
source: "[Paper](papers/paper.md)"
---
```

`tags`, `aliases` and `cssclasses` are read the way Obsidian 1.9 and later read them: as YAML lists only. A comma-separated string or the singular `tag`, `alias` and `cssclass` don't count; `validate_vault` reports notes still written that way. Wikilinks and Markdown links inside properties are links, as in Obsidian 1.11.

### Markdown links

```markdown
[text](Note.md)
[text](folder/My%20Note.md#Heading)
[text](<My Note.md> "Title")
[text][ref]   [ref][]   [ref]

[ref]: Note.md
```

Destinations are percent-decoded, so `My%20Note.md` is the note `My Note.md`. Reference links take their destination from the definition. Inside a table, Obsidian writes a wikilink alias as `[[Note\|Shown]]`; the escaped pipe is read as the alias separator. Moving a note rewrites every form in the way it was written, including relative paths, reference definitions and canvas file cards.

### Canvases and Bases

`.canvas` files are read and written as JSON Canvas 1.0 (`read_canvas`, `write_canvas`); a canvas's file cards and the links in its text cards count as backlinks of the notes they show. `.base` files are evaluated by `query_base`: filters, formulas, sort, `groupBy` and limit over the vault's notes. Functions TurboVault doesn't evaluate are refused by name.

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

- The `get_ofm_guide` tool and the `obsidian://syntax/quick-ref` and `obsidian://syntax/complete-guide` resources, which serve TurboVault's own guide
- [Main Documentation](../README.md)
