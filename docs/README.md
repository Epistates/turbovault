# Documentation

Welcome to the TurboVault documentation. This guide will help you get started with TurboVault and explore its full capabilities.

## Getting Started

- [Quick Start Guide](getting-started/quick-start.md) - Get up and running in minutes

## Configuration

- [Configuration Guide](configuration/index.md) - Configure TurboVault for your needs

## Deployment

- [Deployment Guide](deployment/index.md) - Deploy in production environments

## API Reference

- [API Reference](api-reference/index.md) - Complete reference for all 38 MCP tools

## Development

- [Development Guide](development/index.md) - Contributing to TurboVault

## Troubleshooting

- [Troubleshooting Guide](troubleshooting/index.md) - Common issues and solutions

## Architecture

TurboVault is built as a modular Rust workspace with 8 specialized crates:

- **turbovault-core** - Core types, errors, and configuration
- **turbovault-parser** - Obsidian Flavored Markdown (OFM) parsing
- **turbovault-graph** - Link graph analysis and health diagnostics
- **turbovault-vault** - File I/O, caching, and validation
- **turbovault-batch** - Atomic multi-file operations
- **turbovault-export** - Data export in JSON/CSV formats
- **turbovault-tools** - MCP tools implementation (38 tools)
- **turbovault-server** - CLI and MCP server

## Key Features

- **38 MCP Tools** - Complete vault management API
- **Full-Text Search** - Tantivy-powered search with TF-IDF ranking
- **Link Graph Analysis** - Health scoring, hub detection, broken link analysis
- **Template System** - Pre-built templates with field validation
- **Batch Operations** - Atomic multi-file transactions
- **Export & Reporting** - JSON/CSV exports for health reports
- **Production Ready** - OpenTelemetry, structured logging, metrics

## Quick Links

- [GitHub Repository](https://github.com/epistates/TurboVault)
- [Issues](https://github.com/epistates/TurboVault/issues)
- [MCP Protocol](https://modelcontextprotocol.io)
- [Obsidian](https://obsidian.md)
