# TurboVault Documentation

**Version 0.1.3** - Production-Ready Release

Welcome to the TurboVault documentation. This comprehensive guide will help you install, configure, and use TurboVault with your Obsidian vault.

## 📚 Table of Contents

### Getting Started
- [Quick Start Guide](getting-started/quick-start.md) - Get up and running in minutes
- [Installation Guide](getting-started/installation.md) - Detailed installation instructions

### Configuration & Setup
- [Configuration Guide](configuration/index.md) - Configure TurboVault for your environment
- [Multi-Vault Setup](configuration/multi-vault.md) - Managing multiple vaults

### Usage & API
- [API Reference](api-reference/index.md) - Complete reference for all MCP tools
- [Tool Categories](api-reference/tools.md) - Organized tool reference
- [Obsidian Flavored Markdown](api-reference/ofm.md) - OFM syntax guide

### Deployment
- [Deployment Guide](deployment/index.md) - Deploy to production
- [Claude Desktop Integration](deployment/claude-desktop.md) - Setup with Claude
- [Docker Deployment](deployment/docker.md) - Containerized deployment

### Development
- [Development Guide](development/index.md) - Contributing to TurboVault
- [Architecture](development/architecture.md) - System design and crate structure
- [Building from Source](development/building.md) - Build instructions

### Security
- [mcp-scanner](security/mcp-scanner.md) - MCP Scanner results

### Support
- [Troubleshooting Guide](troubleshooting/index.md) - Common issues and solutions
- [FAQ](troubleshooting/faq.md) - Frequently asked questions

---

## 🏗️ Architecture Overview

TurboVault is built as a modular Rust workspace with **8 specialized crates**:

| Crate | Purpose | Status |
|-------|---------|--------|
| **turbovault** | Main MCP server binary | ✅ v0.1.3 |
| **turbovault-core** | Core models & types | ✅ v0.1.3 |
| **turbovault-parser** | Obsidian Flavored Markdown parser | ✅ v0.1.3 |
| **turbovault-graph** | Link graph analysis & health | ✅ v0.1.3 |
| **turbovault-vault** | File I/O, caching, validation | ✅ v0.1.3 |
| **turbovault-batch** | Atomic multi-file operations | ✅ v0.1.3 |
| **turbovault-export** | Data export utilities | ✅ v0.1.3 |
| **turbovault-tools** | MCP tools implementation | ✅ v0.1.3 |

---

## ✨ Key Features

### 🛠️ Comprehensive Tooling
- **38 specialized MCP tools** for vault management
- Full CRUD operations on notes
- Template generation and management
- Batch operations with atomic transactions

### 🔍 Intelligent Search
- **Full-text search** powered by Tantivy
- TF-IDF ranking algorithm
- Tag and metadata filtering
- Advanced query support

### 📊 Graph Analysis
- **Link graph visualization** and analysis
- Backlink tracking and discovery
- Hub/spoke pattern detection
- Broken link identification
- Vault health scoring

### 📑 Structured Data
- **YAML frontmatter** parsing and validation
- **Obsidian Flavored Markdown** support
- Block references and embeds
- Wikilinks with aliases
- Callouts and admonitions

### 🚀 Production Ready
- **OpenTelemetry** observability
- Structured JSON logging
- Performance metrics
- Error handling and resilience

---

## 🚀 Quick Installation

### Latest Release (v0.1.3)

```bash
# Minimal install (STDIO only, 7.0 MB - perfect for Claude Desktop)
cargo install turbovault

# With HTTP server support
cargo install turbovault --features http

# With all transports (HTTP, WebSocket, TCP, Unix sockets)
cargo install turbovault --features full
```

### Verify Installation

```bash
turbovault --version
turbovault --help
```

---

## 🔗 Links

### Official Resources
- **GitHub**: https://github.com/epistates/turbovault
- **Crates.io**: https://crates.io/crates/turbovault
- **Documentation**: https://docs.rs/turbovault
- **Issues**: https://github.com/epistates/turbovault/issues

### Related
- **MCP Protocol**: https://modelcontextprotocol.io
- **Obsidian**: https://obsidian.md
- **Claude Desktop**: https://claude.ai/download

---

## 📖 Next Steps

1. **New to TurboVault?** → Start with the [Quick Start Guide](getting-started/quick-start.md)
2. **Ready to deploy?** → See the [Deployment Guide](deployment/index.md)
3. **Need API details?** → Check the [API Reference](api-reference/index.md)
4. **Contributing?** → Read the [Development Guide](development/index.md)

---

## ❓ Need Help?

- Check the [Troubleshooting Guide](troubleshooting/index.md)
- Review the [FAQ](troubleshooting/faq.md)
- Open an issue on [GitHub](https://github.com/epistates/turbovault/issues)

---

**Last Updated**: 2025-01-24  
**Version**: 0.1.3  
**Status**: Production Ready ✅
