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
cargo bench -p turbovault-server
```

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
│   └── turbovault-tools/        # MCP tools
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
