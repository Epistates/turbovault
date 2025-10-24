# Multi-Vault Setup

Managing multiple Obsidian vaults with TurboVault.

## Configuration

Create `~/.turbovault/config.yaml`:

```yaml
vaults:
  work:
    path: /Users/you/Documents/Work-Vault
    read_only: false
  
  personal:
    path: /Users/you/Documents/Personal-Vault
    read_only: true
  
  archive:
    path: /Users/you/Documents/Archive-Vault
    read_only: true

observability:
  log_level: info
```

## Using Multiple Vaults

```bash
# Start with multi-vault support
turbovault

# Switch vaults via Claude
# "Switch to the work vault"
# "Show me files in the personal vault"
```

## Performance Notes

- Each vault is indexed separately
- Link graphs are computed per vault
- Search is vault-specific
- Batch operations work within one vault
