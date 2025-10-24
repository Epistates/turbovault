# Installation Guide

Detailed installation instructions for TurboVault v0.1.2.

## System Requirements

- **Rust**: 1.90.0 or later
- **OS**: macOS, Linux, or Windows
- **Storage**: 500 MB for build artifacts
- **Memory**: 1 GB minimum for compilation

## Installation Methods

### Method 1: From crates.io (Recommended)

Install the latest published version:

```bash
# Minimal install (STDIO only, 7.0 MB)
cargo install turbovault

# With HTTP server
cargo install turbovault --features http

# With all features
cargo install turbovault --features full
```

### Method 2: From Source

```bash
git clone https://github.com/epistates/turbovault.git
cd turbovault
cargo build --release
./target/release/turbovault --help
```

### Method 3: Docker (Coming Soon)

```bash
docker run -v /path/to/vault:/vault epistates/turbovault:latest \
  turbovault --vault /vault
```

## Verification

```bash
turbovault --version
turbovault --help
```

## Features

Available installation variants:

- **default**: STDIO transport only (recommended for Claude Desktop)
- **http**: Add HTTP server support
- **websocket**: Add WebSocket server support
- **tcp**: Add TCP server support
- **unix**: Add Unix socket support (macOS/Linux)
- **full**: All transports

## Troubleshooting

### Build fails with "Rust version too old"
```bash
rustup update
cargo install turbovault
```

### "cargo: command not found"
Install Rust from https://rustup.rs

### Build is slow
First builds compile all dependencies. Subsequent installs are faster.

## Next Steps

- [Configuration Guide](../configuration/index.md)
- [Quick Start](quick-start.md)
