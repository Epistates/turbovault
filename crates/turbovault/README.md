# turbovault

[![Crates.io](https://img.shields.io/crates/v/turbovault.svg)](https://crates.io/crates/turbovault)
[![Docs.rs](https://docs.rs/turbovault/badge.svg)](https://docs.rs/turbovault)
[![License](https://img.shields.io/crates/l/turbovault.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

The [TurboVault](https://github.com/epistates/turbovault) MCP server: safe, fast access to Obsidian vaults for AI agents.

- MCP Registry name: `mcp-name: io.github.epistates/turbovault`

## Install

```bash
cargo install turbovault                       # stdio only
cargo install turbovault --features release    # every transport, SQL, vector search
```

Prebuilt, attested binaries for Linux, macOS and Windows are on the [releases page](https://github.com/epistates/turbovault/releases), and multi-arch images at `ghcr.io/epistates/turbovault`.

## Run

```bash
# Claude Code
claude mcp add turbovault -- turbovault --vault ~/Documents/Vault

# Directly, or for Claude Desktop's "command" and "args"
turbovault --vault ~/Documents/Vault --profile production
```

With no `--vault`, the agent registers vaults itself with `add_vault`; registered vaults are remembered between sessions. A config file (`~/.turbovault/config.yaml`, or `--config`) can register several vaults, choose a Git write backend, make a vault read-only, and set tool visibility and security.

## What it serves

- **52 tools**: notes, links and the graph, search and similarity, frontmatter and tags, canvases, Bases, tasks and periodic notes, quality and validation, templates, vaults, audit and rollback, export. 30 are listed by default and 22 advanced ones on request (`--show-advanced-tools`). Every vault-scoped tool takes an optional `vault` argument.
- **Every tool** has parameter descriptions, annotations and a typed `outputSchema`; lists page with `limit` and `offset`; errors carry a category and a next step.
- **Resources**: every note as `obsidian://note/{vault}/{path}`, subscribable for change notifications.
- **Prompts**: `triage_vault`, `repair_broken_links`, `review_period`, `summarize_note`, `ground_draft`.
- **Progress and cancellation** on slow tools, and **confirmation** before destructive calls when the client supports elicitation.

## Transports and security

stdio by default. With the matching features, `--transport http`, `websocket`, `tcp` or `unix`. HTTP takes bearer or JWT authentication (`--auth-mode`) with per-client rate limiting, and refuses to listen beyond loopback without it; WebSocket and TCP stay on loopback. Runtime vault registration is confined (`--allowed-vault-roots`), and `.obsidian`, `.git` and `.turbovault` are unreachable through the tools.

## Features

| Feature | Adds |
|---|---|
| `http`, `websocket`, `tcp`, `unix` | Those transports |
| `full` | `http`, `websocket`, `tcp` |
| `all-transports-unix` | `full` plus `unix` |
| `sql` | `inspect_frontmatter` and `query_frontmatter_sql` |
| `vector-search` | The `vector_search` plugin (enable with `--plugins vector_search`) |
| `plugin-api` | The compiled-in plugin host |
| `release` | `full`, `sql` and `vector-search` |

## Documentation

- [Configuration guide](https://github.com/epistates/turbovault/blob/main/docs/configuration/index.md): every flag, the config file, security, observability
- [Tools](https://github.com/epistates/turbovault/blob/main/docs/api-reference/tools.md) and [API conventions](https://github.com/epistates/turbovault/blob/main/docs/api-reference/index.md)
- [Deployment](https://github.com/epistates/turbovault/blob/main/docs/deployment/index.md)
- [Migrating from 3.x](https://github.com/epistates/turbovault/blob/main/docs/migration/4.0.md)

## License

MIT
