# Configuration Guide

Configure TurboVault for your specific needs and deployment scenarios.

## Configuration Profiles

Pre-built profiles optimized for different use cases:

| Profile | Use Case | Features |
|---------|----------|----------|
| `development` | Local development | Verbose logging, file watching enabled, permissive validation |
| `production` | Production deployments | Info logging, security auditing, performance monitoring |
| `readonly` | Read-only access | Disables all write operations, audit logging enabled |
| `high-performance` | Large vaults (10k+ notes) | Aggressive caching, disabled file watching, optimized for speed |
| `minimal` | Resource-constrained environments | Minimal caching, basic features only |

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

TurboVault reads `~/.turbovault/config.yaml` automatically when present. Use `--config <PATH>` or `TURBOVAULT_CONFIG` to point at a different YAML file. The currently supported server-level section is `tool_visibility`.

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
