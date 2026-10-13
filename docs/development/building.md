# Building from Source

Complete guide to building TurboVault from source.

## Prerequisites

- Rust 1.90.0 or later
- Git
- Approximately 1 GB free disk space

## Clone Repository

```bash
git clone https://github.com/epistates/turbovault.git
cd turbovault
```

## Build

### Development Build
```bash
cargo build
# Binary: target/debug/turbovault
```

### Release Build
```bash
cargo build --release
# Binary: target/release/turbovault (7.0 MB)
```

### With Features
```bash
cargo build --release --features http,websocket
```

## Testing

```bash
# Run all tests
cargo test --all

# Run specific crate tests
cargo test -p turbovault-parser

# Run with output
cargo test -- --nocapture
```

## Benchmarking

```bash
# Every benchmark (tool benchmarks plus the 10,000-note vault)
cargo bench -p turbovault

# Only the 10,000-note vault benchmarks, at another size
TV_BENCH_NOTES=2000 cargo bench -p turbovault --bench vault_10k

# Compare two saved runs
cargo bench -p turbovault --bench vault_10k -- --save-baseline before
# ...make a change...
cargo bench -p turbovault --bench vault_10k -- --save-baseline after
python3 scripts/bench_compare.py target/criterion before after
```

The scheduled workflow runs `vault_10k` nightly and fails when a benchmark is
more than 25% slower than the last good night.

## Code Quality

```bash
# Format check
cargo fmt --check

# Linting
cargo clippy --all-targets

# Fix issues
cargo fmt && cargo clippy --fix
```

## Project Structure

```
turbovault/
├── crates/
│   ├── turbovault/              # Main binary
│   ├── turbovault-core/         # Core types
│   ├── turbovault-parser/       # OFM parser
│   ├── turbovault-graph/        # Graph analysis
│   ├── turbovault-vault/        # File I/O
│   ├── turbovault-batch/        # Batch ops
│   ├── turbovault-export/       # Export utils
│   ├── turbovault-tools/        # MCP tools
│   └── turbovault-plugin-api/   # Stable compiled-in plugin boundary
├── docs/                        # Documentation
├── tests/                       # Integration tests
└── Cargo.toml                   # Workspace
```

## Contributing

1. Fork the repository
2. Create a feature branch
3. Make changes and test
4. Submit a pull request

See [Development Guide](index.md) for more details.
