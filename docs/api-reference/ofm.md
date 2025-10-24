# Obsidian Flavored Markdown Reference

Complete reference for Obsidian Flavored Markdown (OFM) syntax supported by TurboVault.

## Overview

TurboVault fully supports Obsidian Flavored Markdown, which extends CommonMark with Obsidian-specific features.

## Core Syntax

### Text Formatting

```markdown
**bold**
*italic*
~~strikethrough~~
==highlight==
`code`
```

### Wikilinks

```markdown
[[Note Name]]
[[Note Name|Alias]]
[[Note Name#Heading]]
[[Note Name#^block-id]]
![[image.png]]
![[Note Name#section]]
```

### Callouts

```markdown
> [!note] Title
> Content here

> [!warning]
> Multiple lines
> Of content
```

Supported types: note, abstract, info, tip, success, question, warning, failure, danger, bug, example, quote

### Task Lists

```markdown
- [ ] Incomplete
- [x] Complete
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

### Attributes

```markdown
```attr
status: evergreen
owner: name
```
```

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
