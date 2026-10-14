# Installation

## Prebuilt binaries

Each [GitHub release](https://github.com/Epistates/turbovault/releases) has an archive per platform, built with every feature (all transports, SQL, the vector search plugin):

| Platform | Archive |
|---|---|
| Linux x86_64 | `turbovault-x86_64-unknown-linux-gnu.tar.gz` (or `-musl`) |
| Linux aarch64 | `turbovault-aarch64-unknown-linux-gnu.tar.gz` (or `-musl`) |
| macOS Apple silicon | `turbovault-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `turbovault-x86_64-apple-darwin.tar.gz` |
| Windows x86_64 | `turbovault-x86_64-pc-windows-msvc.exe.zip` |

Each archive has a `.sha256` beside it. The release also has a combined `SHA256SUMS`, a CycloneDX SBOM and signed build provenance. Verify an archive with the GitHub CLI:

```bash
gh attestation verify turbovault-aarch64-apple-darwin.tar.gz --repo Epistates/turbovault
```

Native binaries embed their dependency list, so `cargo audit bin turbovault` works on them.

## From crates.io

```bash
cargo install turbovault                       # stdio only
cargo install turbovault --features release    # what the prebuilt binaries have
```

| Feature | Adds |
|---|---|
| (default) | stdio, enough for Claude Desktop and Claude Code |
| `http` | The HTTP transport, with bearer or JWT authentication |
| `websocket`, `tcp` | Those transports (loopback only) |
| `unix` | The Unix socket transport (not on Windows) |
| `full` | `http`, `websocket` and `tcp` |
| `all-transports-unix` | `full` plus `unix` |
| `sql` | `inspect_frontmatter` and `query_frontmatter_sql` |
| `vector-search` | The `vector_search` plugin (enable it at runtime with `--plugins vector_search`) |
| `release` | `full`, `sql` and `vector-search` |

Building needs Rust 1.90 or later.

## Docker

Multi-arch images (amd64, arm64) are published to GitHub Container Registry with each release, tagged with the version (`4.0.0`, `4.0` and `4`):

```bash
docker run -i --rm -v /path/to/vault:/var/obsidian-vault ghcr.io/epistates/turbovault:4
```

See [Docker](../deployment/docker.md) for MCP client configuration and network transports.

## From source

```bash
git clone https://github.com/epistates/turbovault.git
cd turbovault
cargo build --release
./target/release/turbovault --version
```

See [Building from source](../development/building.md).

## Next steps

- [Quick start](quick-start.md)
- [Configuration guide](../configuration/index.md)
