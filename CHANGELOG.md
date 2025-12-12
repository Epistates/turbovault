# Changelog

All notable changes to TurboVault will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [1.2.4] - 2025-12-12

### Added

- **Plain text extraction**: New `to_plain_text()` API for extracting visible text from markdown content, stripping all syntax. Useful for:
  - Search indexing (index only searchable text)
  - Accurate match counts (fixes treemd search mismatch where `[Overview](#overview)` counted URL chars)
  - Word counts
  - Accessibility text extraction
- `InlineElement::to_plain_text(&self) -> &str` - Extract text from inline elements (links return link text, images return alt text)
- `ListItem::to_plain_text(&self) -> String` - Extract text from list items including nested blocks
- `ContentBlock::to_plain_text(&self) -> String` - Extract text from any content block recursively
- `to_plain_text(markdown: &str) -> String` - Standalone function to parse and extract plain text in one call
- Exported `to_plain_text` from `turbovault_parser` crate and prelude
- **Search result metrics**: `SearchResultInfo` now includes `word_count` and `char_count` fields for content size estimation
- **Export readability metrics**: `VaultStatsRecord` now includes `total_words`, `total_readable_chars`, and `avg_words_per_note`

### Changed

- **Search engine uses plain text**: Tantivy index now indexes plain text content instead of raw markdown, improving search relevance
- **Keyword extraction uses plain text**: `find_related()` now extracts keywords from visible text only, excluding URLs and markdown syntax
- **Search previews use plain text**: Search result previews and snippets now show human-readable text without markdown formatting

## [1.2.3] - 2025-12-10

### Fixed

- Updated turbomcp dependency to 2.3.3 for compatibility with latest MCP server framework

## [1.2.2] - 2025-12-09

### Added

- Dependency version bump to turbomcp 2.3.2

### Changed

- Updated all workspace dependencies to latest compatible versions

### Fixed

- Optimized binary search in excluded ranges for improved performance
- Removed unused dependencies to reduce binary size

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

[1.2.3]: https://github.com/epistates/turbovault/compare/v1.2.2...v1.2.3
[1.2.2]: https://github.com/epistates/turbovault/compare/v1.2.1...v1.2.2
[1.2.0]: https://github.com/epistates/turbovault/compare/v1.1.8...v1.2.0
[1.1.8]: https://github.com/epistates/turbovault/compare/v1.1.0...v1.1.8
[1.1.0]: https://github.com/epistates/turbovault/releases/tag/v1.1.0
