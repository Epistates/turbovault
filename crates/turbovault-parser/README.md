# turbovault-parser

[![Crates.io](https://img.shields.io/crates/v/turbovault-parser.svg)](https://crates.io/crates/turbovault-parser)
[![Docs.rs](https://docs.rs/turbovault-parser/badge.svg)](https://docs.rs/turbovault-parser)
[![License](https://img.shields.io/crates/l/turbovault-parser.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

An Obsidian Flavored Markdown parser, and a JSON Canvas 1.0 reader and writer, for [TurboVault](https://github.com/epistates/turbovault).

One pulldown-cmark pass finds code, links and structure; Obsidian syntax is then read outside the code ranges it found, so a `[[link]]` in a code block is never a link. Positions are byte offsets plus line and column.

## What it reads

- **Wikilinks and embeds**: `[[Note]]`, `[[Note|alias]]`, `[[Note#Heading]]`, `[[Note#^block]]`, `![[image.png]]`, and the table-escaped `[[Note\|alias]]`.
- **Markdown links**: inline (`[x](My%20Note.md)`, `[x](<My Note.md>)`), reference-style (`[x][ref]`, `[ref][]`, `[ref]` with `[ref]: target`), with destinations percent-decoded.
- **Frontmatter** as YAML, bounded in size and depth; wikilinks and Markdown links in property values count as links.
- **Tags** (`#tag`, `#parent/child`), **tasks** (`[ ]`, `[x]`, `[/]`, `[-]`, Tasks-plugin emoji, Dataview fields), **callouts**, **headings** with anchors, **citations**.
- **Blocks**: `parse_blocks` turns a note into a tree of paragraphs, headings, lists, quotes, tables, code and callouts, for renderers and outline tools.
- **JSON Canvas 1.0**: `parse_canvas` reads `.canvas` files into nodes and edges; canvases validate and serialize back in Obsidian's own layout.

## Example

```rust
use turbovault_parser::{ParsedContent, parse_markdown_links, parse_wikilinks};

let note = "---\ntags: [project]\n---\n# Plan\n\nSee [[Roadmap|the roadmap]] and [notes][n].\n\n- [ ] Ship it\n\n[n]: Meeting%20Notes.md\n";

let parsed = ParsedContent::parse(note);
assert_eq!(parsed.headings[0].text, "Plan");
assert_eq!(parsed.tasks.len(), 1);

assert_eq!(parse_wikilinks(note)[0].target, "Roadmap");
assert_eq!(parse_markdown_links(note)[0].target, "Meeting Notes.md");
```

`Parser::new(vault_root).parse_file(path, content)` returns a `VaultFile` with the path, metadata and every element above, which is what the rest of TurboVault works from.

## License

MIT
