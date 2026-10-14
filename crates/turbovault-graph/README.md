# turbovault-graph

[![Crates.io](https://img.shields.io/crates/v/turbovault-graph.svg)](https://crates.io/crates/turbovault-graph)
[![Docs.rs](https://docs.rs/turbovault-graph/badge.svg)](https://docs.rs/turbovault-graph)
[![License](https://img.shields.io/crates/l/turbovault-graph.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

The link graph of an Obsidian vault, and the health analysis built on it, for [TurboVault](https://github.com/epistates/turbovault).

## What it does

- **`LinkGraph`** keeps notes as nodes and resolved links as edges (a petgraph `StableDiGraph`). `add_file` adds a note and `update_links` sets its outgoing links, one note at a time; adding a note re-resolves only the broken links that could name it, and `remove_file` turns links to it into broken ones.
- **Resolution** follows Obsidian: a link resolves by path suffix, by basename, or through an alias in the target's frontmatter, case-insensitively. Of several notes with the same name, a bare link means the one in the fewest folders, then the first by path.
- **Queries**: backlinks and forward links, related notes within N hops, orphans, cycles, connected components, degrees, and the broken links still waiting for a target.
- **`HealthAnalyzer`** scores the graph from 0 to 100 and lists broken links (with suggested targets), orphans, dead ends, hubs and isolated clusters. Each list has its own cheap pass, so counting doesn't require the full analysis.

## Example

```rust
use std::path::{Path, PathBuf};
use turbovault_graph::{HealthAnalyzer, LinkGraph};
use turbovault_parser::Parser;

fn main() -> turbovault_core::Result<()> {
    let parser = Parser::new(PathBuf::from("/vault"));
    let notes = [
        parser.parse_file(Path::new("/vault/a.md"), "See [[b]] and [[missing]].")?,
        parser.parse_file(Path::new("/vault/b.md"), "Back to [[a]].")?,
    ];

    // Add every note first, then its links, so each link can find its target.
    let mut graph = LinkGraph::new();
    for note in &notes {
        graph.add_file(note)?;
    }
    for note in &notes {
        graph.update_links(note)?;
    }

    let backlinks = graph.backlinks(&PathBuf::from("/vault/b.md"))?;
    println!("{} note(s) link to b", backlinks.len());

    let report = HealthAnalyzer::new(&graph).analyze()?;
    println!("health {} with {} broken link(s)", report.health_score, report.broken_links.len());
    Ok(())
}
```

In TurboVault, `VaultManager` (turbovault-vault) owns the graph and keeps it in step with the vault, including edits made outside TurboVault.

## License

MIT
