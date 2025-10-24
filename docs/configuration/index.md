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

## Configuration File (Future)

Configuration files are planned for future releases:

```yaml
# config.yaml (future)
profiles:
  default: production

vaults:
  - name: personal
    path: /vaults/personal
    is_default: true
    watch_for_changes: true
    cache_ttl: 3600

  - name: work
    path: /vaults/work
    excluded_paths:
      - .obsidian
      - private/

observability:
  log_level: info
  otlp_endpoint: http://localhost:4317
  enable_metrics: true
  enable_tracing: true
```

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
