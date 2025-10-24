# Quick Start Guide

Get TurboVault running in just a few minutes!

## 📦 Installation

### System Requirements

- **Rust**: 1.90.0 or later
- **OS**: macOS, Linux, or Windows
- **Obsidian**: Any recent version
- **Memory**: 512 MB minimum

### Install the Binary

```bash
# Latest from crates.io (recommended)
cargo install turbovault

# Or with features
cargo install turbovault --features "http,websocket"
```

Verify installation:
```bash
turbovault --version
# turbovault 0.1.1
```

### Build from Source

```bash
git clone https://github.com/epistates/turbovault.git
cd turbovault
cargo build --release
./target/release/turbovault --help
```

---

## ⚙️ Configuration

### 1. Locate Your Vault

Find your Obsidian vault directory. Typically:
- **macOS**: `~/Documents/ObsidianVault` or similar
- **Linux**: `~/ObsidianVault` or custom path
- **Windows**: `C:\Users\YourName\Documents\ObsidianVault`

### 2. Create Config (Optional)

```bash
turbovault --vault /path/to/your/vault --profile production
```

Or create `~/.turbovault/config.yaml`:

```yaml
vaults:
  default:
    path: /path/to/your/vault
    read_only: false

observability:
  log_level: info
```

---

## 🚀 Usage

### Option 1: Claude Desktop (Easiest)

1. **Get turbovault path**:
```bash
which turbovault
# /Users/you/.cargo/bin/turbovault
```

2. **Update Claude Desktop config**:
- **macOS/Linux**: `~/.config/claude/claude_desktop_config.json`
- **Windows**: `%APPDATA%/Claude/claude_desktop_config.json`

```json
{
  "mcpServers": {
    "turbovault": {
      "command": "/Users/you/.cargo/bin/turbovault",
      "args": ["--vault", "/path/to/your/vault"]
    }
  }
}
```

3. **Restart Claude Desktop** - Done! 🎉

### Option 2: Direct CLI

```bash
turbovault --vault /path/to/your/vault
```

---

## 🔍 Your First Query

With TurboVault connected to Claude, try:

> "Show me all my notes tagged with #project/alpha"
> "Find broken links in my vault"
> "Create a new daily note with today's date"
> "Search for notes about 'machine learning'"

---

## 📚 Available Tools (Sample)

TurboVault exposes 38 tools including:

**File Operations**
- `read_note` - Read note content
- `write_note` - Create/update notes
- `delete_note` - Remove notes

**Search**
- `search_notes` - Full-text search
- `search_by_tag` - Filter by tags
- `search_by_metadata` - Query frontmatter

**Graph Analysis**
- `get_backlinks` - Show what links to this note
- `find_hubs` - Identify hub notes
- `analyze_vault_health` - Get health report

**Templates**
- `list_templates` - Available templates
- `create_from_template` - Generate notes from templates

**Batch Operations**
- `batch_rename` - Rename multiple notes
- `batch_create` - Create many notes at once

---

## 🎓 Next Steps

### For Users
- [Complete Configuration Guide](../configuration/index.md)
- [Claude Desktop Setup](../deployment/claude-desktop.md)
- [API Reference](../api-reference/index.md)

### For Developers
- [Architecture Guide](../development/architecture.md)
- [Building from Source](../development/building.md)

---

## ❓ Troubleshooting

### "turbovault: command not found"
```bash
# Check installation
which turbovault

# If not found, reinstall
cargo install turbovault --force
```

### "Connection refused"
- Ensure Claude Desktop has been restarted
- Check file paths are correct
- Verify vault exists

### "Permission denied"
- Ensure vault directory is readable
- Check file permissions: `ls -la /path/to/vault`

See [Troubleshooting Guide](../troubleshooting/index.md) for more help.

---

## 🔗 Resources

- **GitHub**: https://github.com/epistates/turbovault
- **Docs**: https://docs.rs/turbovault
- **Issues**: https://github.com/epistates/turbovault/issues

---

**Ready?** Start using TurboVault with Claude now! 🚀
