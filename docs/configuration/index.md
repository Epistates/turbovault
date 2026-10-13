# Configuration Guide

Configure TurboVault for your specific needs and deployment scenarios.

## Configuration Profiles

`--profile` sets the log level (`RUST_LOG` overrides it) and the settings every vault starts from. An unknown name stops startup rather than falling back.

| Profile | Log level | Also |
|---------|-----------|------|
| `development` (default) | debug | |
| `production` | info | |
| `readonly` (or `read-only`) | warn | Every tool not annotated read-only is hidden and refused, the same gate as `--require-read-only-tools` |
| `high-performance` | warn | |
| `minimal` | error | Doesn't reconcile with edits made outside TurboVault |
| `multi-vault`, `collaboration` | info | |

**Usage:**
```bash
mcp-obsidian --vault /path/to/vault --profile production
```

## Vault Configuration

### Single Vault Setup

```bash
# Basic vault configuration
mcp-obsidian --vault /path/to/vault --init
```

### Multi-Vault Setup

Multi-vault support requires using the `MultiVaultManager` API (CLI support coming soon):

```rust
use TurboVault_core::MultiVaultManager;

let manager = MultiVaultManager::new();

// Add vaults
manager.add_vault("personal", "/vaults/personal").await?;
manager.add_vault("work", "/vaults/work").await?;

// Set active vault
manager.set_active_vault("personal").await?;
```

## Environment Variables

```bash
# Vault path (alternative to --vault CLI arg)
export OBSIDIAN_VAULT_PATH=/path/to/vault

# Logging level
export RUST_LOG=info,TurboVault=debug

# OpenTelemetry endpoint (if using OTLP export)
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
```

## Configuration File

TurboVault reads `~/.turbovault/config.yaml` automatically when present. Use `--config <PATH>` or `TURBOVAULT_CONFIG` to point at a different YAML file. The supported server-level sections are `tool_visibility`, `vaults` and `security` (see [Security](#security)).

```yaml
tool_visibility:
  # If non-empty, only these exact tool names are listed and callable.
  allowed:
    - read_note
    - search

  # Omit these tools from tools/list, but allow direct calls by exact name.
  hidden:
    - full_health_analysis
    - explain_vault

  # Omit these tools and reject direct calls.
  disabled:
    - delete_note

  # Hide tools that TurboMCP has not annotated as read-only.
  require_read_only: false
```

The same rules can be supplied with comma-separated CLI/env overrides: `--allowed-tools` / `TURBOVAULT_ALLOWED_TOOLS`, `--hidden-tools` / `TURBOVAULT_HIDDEN_TOOLS`, `--disabled-tools` / `TURBOVAULT_DISABLED_TOOLS`, and `--require-read-only-tools` / `TURBOVAULT_REQUIRE_READ_ONLY_TOOLS`.

## Security

The `security` section of the config file sets who can reach the server and which directories it will serve. Every key has a command-line flag and environment variable that take precedence. See `config.example.yaml` for the full section.

```yaml
security:
  allowed_vault_roots: [~/Vaults]
  auth:
    mode: jwt
    resource: https://vault.example.com/mcp
    issuer: https://auth.example.com
    jwks_uri: https://auth.example.com/.well-known/jwks.json
    required_scopes: [vault]
```

**Vault registration.** `add_vault` and `create_vault` check every path. A filesystem root, the home directory and anything containing it, hidden directories under home (`~/.ssh`, `~/.config`), application data (`~/Library` apart from iCloud and cloud storage folders, `AppData`) and system directories are never accepted. With `allowed_vault_roots` set, only directories inside those roots are. Without it, any other directory is accepted on stdio, and runtime registration is off on network transports. Vaults from `--vault` and the config file's `vaults:` list are the operator's choice and are not restricted; vaults restored from the cache are held to the same rules as a new registration.

**Network exposure.** The HTTP transport listens beyond loopback only with authentication. WebSocket and TCP have no authentication, so they listen on loopback only. `--allow-unauthenticated-network` lifts both refusals for a deployment that authenticates in front of TurboVault.

**Authentication (HTTP).** With `--auth-mode bearer` or `jwt`, every request needs `Authorization: Bearer <token>`, and the server publishes OAuth protected resource metadata (RFC 9728) at `/.well-known/oauth-protected-resource`. `bearer` accepts one of a fixed set of shared secrets of at least 32 characters, from `--auth-token-file` or `TURBOVAULT_AUTH_TOKEN`. `jwt` validates the issuer, audience (this server's `--auth-resource` unless `--auth-audience` says otherwise), expiry and signature against the issuer's JWKS, and requires `--auth-required-scopes`.

**Unix socket.** Without `--socket-path`, the socket is `turbovault.sock` in a private (0700) directory under `$XDG_RUNTIME_DIR`, or `~/.turbovault/run`.

## CLI Reference

### Command Line Arguments

```bash
mcp-obsidian [OPTIONS]
```

**Options:**

| Flag | Environment Variable | Default | Description |
|------|---------------------|---------|-------------|
| `--vault <PATH>` | `OBSIDIAN_VAULT_PATH` | (required) | Path to Obsidian vault directory |
| `--profile <PROFILE>` | - | `development` | Configuration profile |
| `--transport <MODE>` | - | `stdio` | Transport mode (only `stdio` is MCP-compliant) |
| `--config <PATH>` | `TURBOVAULT_CONFIG` | `~/.turbovault/config.yaml` if present | YAML config path |
| `--allowed-tools <NAMES>` | `TURBOVAULT_ALLOWED_TOOLS` | - | Comma-separated exact tool allowlist |
| `--hidden-tools <NAMES>` | `TURBOVAULT_HIDDEN_TOOLS` | - | Comma-separated tools hidden from `tools/list` but callable |
| `--disabled-tools <NAMES>` | `TURBOVAULT_DISABLED_TOOLS` | - | Comma-separated tools hidden and rejected |
| `--require-read-only-tools` | `TURBOVAULT_REQUIRE_READ_ONLY_TOOLS` | `false` | Hide non-read-only tools |
| `--init` | - | `false` | Initialize vault on startup (scan files, build graph) |
| `--allowed-vault-roots <DIRS>` | `TURBOVAULT_ALLOWED_VAULT_ROOTS` | - | Comma-separated directories runtime vault registration may point inside |
| `--auth-mode <MODE>` | `TURBOVAULT_AUTH_MODE` | `none` | HTTP authentication: `none`, `bearer` or `jwt` |
| `--auth-token-file <PATH>` | `TURBOVAULT_AUTH_TOKEN_FILE` | - | `bearer`: accepted tokens, one per line (or one token in `TURBOVAULT_AUTH_TOKEN`) |
| `--auth-resource <URL>` | `TURBOVAULT_AUTH_RESOURCE` | `http://<host>:<port>/mcp` | This server's canonical URL, the audience tokens are issued for |
| `--auth-issuer <URL>` | `TURBOVAULT_AUTH_ISSUER` | - | `jwt`: expected issuer |
| `--auth-jwks-uri <URL>` | `TURBOVAULT_AUTH_JWKS_URI` | - | `jwt`: the issuer's signing keys |
| `--auth-audience <AUD>` | `TURBOVAULT_AUTH_AUDIENCE` | `--auth-resource` | `jwt`: expected audience |
| `--auth-required-scopes <SCOPES>` | `TURBOVAULT_AUTH_REQUIRED_SCOPES` | - | `jwt`: comma-separated scopes every token needs |
| `--auth-authorization-servers <URLS>` | `TURBOVAULT_AUTH_AUTHORIZATION_SERVERS` | the issuer | Authorization servers to advertise in the resource metadata |
| `--rate-limit <N>` | `TURBOVAULT_RATE_LIMIT` | off on loopback, else 600 | HTTP requests per client per window; `0` turns it off |
| `--rate-limit-window-secs <S>` | `TURBOVAULT_RATE_LIMIT_WINDOW_SECS` | `60` | Rate limit window |
| `--allowed-origins <ORIGINS>` | `TURBOVAULT_ALLOWED_ORIGINS` | - | Browser origins accepted besides localhost |
| `--trusted-proxies <CIDRS>` | `TURBOVAULT_TRUSTED_PROXIES` | - | Proxies whose forwarded client address headers are trusted |
| `--allow-unauthenticated-network` | `TURBOVAULT_ALLOW_UNAUTHENTICATED_NETWORK` | `false` | Listen beyond loopback without authentication |
| `--socket-path <PATH>` | `TURBOVAULT_SOCKET_PATH` | private run directory | Unix socket path |
| `--help` | - | - | Show help message |
| `--version` | - | - | Show version |

### Examples

```bash
# Minimal usage (development mode, no init)
mcp-obsidian --vault /path/to/vault

# Production mode with initialization
mcp-obsidian --vault /path/to/vault --profile production --init

# Readonly mode (no modifications allowed)
mcp-obsidian --vault /path/to/vault --profile readonly

# High-performance mode (large vaults)
mcp-obsidian --vault /path/to/vault --profile high-performance --init
```

## Performance Tuning

### For Small Vaults (<1000 notes)
- Use `development` profile
- Defaults are fine

### For Medium Vaults (1k-10k notes)
- Use `production` profile
- Enable caching with 1-hour TTL
- Use `--init` to build graph once on startup

### For Large Vaults (10k+ notes)
- Use `high-performance` profile
- Disable file watching (reduces CPU overhead)
- Aggressive caching with long TTLs
- Limit search results to 10-20
- Consider splitting into multiple vaults
