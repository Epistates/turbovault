# Changelog

All notable changes to TurboVault will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.2.0] - 2024-12-08

### Added

- **`Anchor` LinkType variant**: Distinguishes same-document anchors (`#section`) from cross-file heading references (`file.md#section`). This is a breaking change for exhaustive match statements on `LinkType`.
- **`BlockRef` detection**: Wikilinks with block references (`[[Note#^blockid]]`) now correctly return `LinkType::BlockRef` instead of `LinkType::HeadingRef`.
- **Block-level parsing**: New `parse_blocks()` function for full markdown AST parsing, including:
  - `ContentBlock` enum: Heading, Paragraph, Code, List, Blockquote, Table, Image, HorizontalRule, Details
  - `InlineElement` enum: Text, Strong, Emphasis, Code, Link, Image, Strikethrough
  - `ListItem` struct with task checkbox support
  - `TableAlignment` enum for table column alignment
- **Shared link utilities**: New `parsers::link_utils` module with `classify_url()` and `classify_wikilink()` functions for consistent link type classification.
- **Re-exported core types from turbovault-parser**: `ContentBlock`, `InlineElement`, `LinkType`, `ListItem`, `TableAlignment`, `LineIndex`, `SourcePosition` are now directly accessible from `turbovault_parser`, eliminating the need for consumers to depend on `turbovault-core` separately.

### Changed

- **Heading anchor generation**: Now uses improved `slugify()` function that properly collapses consecutive hyphens and handles edge cases per Obsidian's behavior.
- **Consolidated duplicate code**: Removed duplicate `classify_url()` implementations from engine.rs and markdown_links.rs in favor of shared utility.

### Fixed

- **Code block awareness**: Patterns inside fenced code blocks, inline code, and HTML blocks are no longer incorrectly extracted as links/tags/embeds.
- **Image parsing in blocks**: Fixed bug where inline images inside paragraphs were causing empty blocks.

## [1.1.8] - 2024-12-07

### Added

- Regression tests for CLI vault deduplication (PR #3)

### Fixed

- Skip CLI vault addition when vault already exists from cache recovery

## [1.1.0] - 2024-12-01

### Added

- Initial public release
- 44 MCP tools for Obsidian vault management
- Multi-vault support with runtime vault addition
- Unified ParseEngine with pulldown-cmark integration
- Link graph analysis with petgraph
- Atomic file operations with rollback support
- Configuration profiles (development, production, readonly, high-performance)

[1.2.0]: https://github.com/epistates/turbovault/compare/v1.1.8...v1.2.0
[1.1.8]: https://github.com/epistates/turbovault/compare/v1.1.0...v1.1.8
[1.1.0]: https://github.com/epistates/turbovault/releases/tag/v1.1.0
