# TurboVault documentation

TurboVault is an MCP server for Obsidian vaults. Start with the [README](../README.md) for an overview.

## Getting started
- [Installation](getting-started/installation.md): binaries, crates.io, Docker, building from source
- [Quick start](getting-started/quick-start.md): connect Claude Desktop or Claude Code and make a first call

## Configuration
- [Configuration guide](configuration/index.md): profiles, every flag and environment variable, the config file, security, tool visibility, observability
- [Multiple vaults](configuration/multi-vault.md): registering vaults, the active vault, and the per-call `vault` argument

## Using the tools
- [API reference](api-reference/index.md): conventions every tool follows (output schemas, pagination, errors, `vault`), resources and prompts
- [Tools](api-reference/tools.md): the 52 tools, core and advanced
- [Obsidian Flavored Markdown](api-reference/ofm.md): the syntax TurboVault reads and writes
- [Migrating to 4.0](migration/4.0.md): every 3.x tool and parameter and its replacement

## Deployment
- [Deployment guide](deployment/index.md): stdio, HTTP with authentication, systemd, Kubernetes
- [Claude Desktop and Claude Code](deployment/claude-desktop.md)
- [Docker](deployment/docker.md)

## Development
- [Development guide](development/index.md): workflow, tests, snapshots, fuzzing, benchmarks, adding a tool
- [Architecture](development/architecture.md): crates and data flow
- [Building from source](development/building.md)
- [Plugins](development/plugins.md): the compiled-in plugin contract
- [Releasing](development/releasing.md)
- [Write-safety suite](write-safety-suite/README.md): the clobber-safety matrix and its pending cells

## Help
- [Troubleshooting](troubleshooting/index.md)
- [FAQ](troubleshooting/faq.md)
- [Security policy](../SECURITY.md)

Feature requests that were written up in the repository live in [feature-requests](feature-requests/).
