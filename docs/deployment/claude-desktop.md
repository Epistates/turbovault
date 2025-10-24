# Claude Desktop Integration

Step-by-step guide to integrate TurboVault with Claude Desktop.

## Installation

1. Install TurboVault:
```bash
cargo install turbovault
```

2. Find the binary:
```bash
which turbovault
# /Users/you/.cargo/bin/turbovault
```

## Configuration

### macOS/Linux

Edit `~/.config/claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "turbovault": {
      "command": "/Users/you/.cargo/bin/turbovault",
      "args": ["--vault", "/path/to/your/vault", "--profile", "production"]
    }
  }
}
```

### Windows

Edit `%APPDATA%\Claude\claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "turbovault": {
      "command": "C:\\Users\\You\\.cargo\\bin\\turbovault.exe",
      "args": ["--vault", "C:\\Users\\You\\Documents\\Vault"]
    }
  }
}
```

## Restart & Verify

1. Restart Claude Desktop completely
2. In Claude, ask: "What tools do you have available?"
3. You should see TurboVault tools listed

## Usage

Try commands like:
- "Read my note about project alpha"
- "Search for notes with the tag #work"
- "Show me the health of my vault"
- "Create a new note with today's date"

## Troubleshooting

**Tools not showing up?**
- Verify config file syntax (JSON must be valid)
- Restart Claude completely
- Check file paths are correct

**Permission denied?**
- Ensure vault directory is readable
- Check executable permissions: `chmod +x $(which turbovault)`

**Connection errors?**
- Check the vault path exists
- Verify turbovault binary works: `turbovault --help`
