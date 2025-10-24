# Frequently Asked Questions

Common questions about TurboVault.

## Installation

**Q: Why doesn't `cargo install turbovault` work?**
A: Ensure Rust 1.90.0+ is installed. Run `rustup update` and try again.

**Q: Can I use an older version of Rust?**
A: No, TurboVault requires Rust 1.90.0 or later.

## Usage

**Q: How do I switch between vaults?**
A: Configure multiple vaults in `~/.turbovault/config.yaml` and ask Claude to switch.

**Q: Can TurboVault modify my vault?**
A: Yes, it can read, write, create, and delete notes. Set `read_only: true` in config for read-only access.

**Q: Is my vault backed up?**
A: TurboVault doesn't backup files. Use Obsidian's built-in sync or Git for backups.

## Performance

**Q: Why is the first search slow?**
A: First search indexes all notes. Subsequent searches are cached and fast.

**Q: Can it handle large vaults (10,000+ notes)?**
A: Yes, but indexing takes longer. Start with a subset if testing.

## Features

**Q: What Markdown syntax is supported?**
A: Obsidian Flavored Markdown (OFM) including wikilinks, callouts, tasks, etc. See [OFM Guide](../api-reference/ofm.md).

**Q: Can I use TurboVault without Claude?**
A: TurboVault is an MCP server, designed for Claude Desktop. Direct CLI support is planned.

## Troubleshooting

**Q: MCP tools aren't showing in Claude?**
A: Restart Claude Desktop completely after updating config.

**Q: Permission denied errors?**
A: Ensure vault directory is readable and turbovault has access.

**Q: "Connection refused" error?**
A: Claude Desktop may not have restarted. Try closing and reopening it.

## Development

**Q: Can I contribute to TurboVault?**
A: Yes! See [Development Guide](../development/index.md).

**Q: Where can I report bugs?**
A: GitHub Issues: https://github.com/epistates/turbovault/issues

**Q: Is there a roadmap?**
A: Check GitHub Discussions for planned features.

## Support

Still have questions? Open an issue on GitHub!
