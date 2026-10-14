# turbovault-export

[![Crates.io](https://img.shields.io/crates/v/turbovault-export.svg)](https://crates.io/crates/turbovault-export)
[![Docs.rs](https://docs.rs/turbovault-export/badge.svg)](https://docs.rs/turbovault-export)
[![License](https://img.shields.io/crates/l/turbovault-export.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

The report formats behind [TurboVault](https://github.com/epistates/turbovault)'s `export` tool: plain record types and exporters that write them as JSON or CSV.

| Record | Exporter | Contents |
|---|---|---|
| `HealthReport` | `HealthReportExporter` | Health score, note and link counts, broken links, orphans, connectivity, recommendations |
| `BrokenLinkRecord` | `BrokenLinksExporter` | Source note, target, line and suggested fixes, one row per broken link |
| `VaultStatsRecord` | `VaultStatsExporter` | File, link, orphan and word counts |
| `AnalysisReport` | `AnalysisReportExporter` | A health report plus counts and recommendations |

Each exporter has `to_json` and `to_csv`. CSV cells are escaped, and a cell starting with `=`, `+`, `-` or `@` is prefixed so a spreadsheet won't run it as a formula.

## Example

```rust
use turbovault_export::{BrokenLinkRecord, BrokenLinksExporter};

fn main() -> turbovault_core::Result<()> {
    let links = vec![BrokenLinkRecord {
        source_file: "notes/plan.md".into(),
        target: "Roadmap".into(),
        line: 3,
        suggestions: vec!["projects/Roadmap.md".into()],
    }];

    println!("{}", BrokenLinksExporter::to_csv(&links)?);
    Ok(())
}
```

The records are filled by the caller; in TurboVault, `turbovault-tools` builds them from the link graph and vault statistics.

## License

MIT
