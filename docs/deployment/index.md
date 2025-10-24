# Deployment Guide

Deploy TurboVault in production environments with proper security and monitoring.

## Docker Deployment

### docker-compose.yml

```yaml
version: '3.8'

services:
  mcp-obsidian:
    build:
      context: .
      dockerfile: Dockerfile
    image: TurboVault:latest
    container_name: mcp-obsidian
    user: obsidian
    volumes:
      # Mount your vault (read-write)
      - /path/to/your/vault:/var/obsidian-vault
    environment:
      - RUST_LOG=info
      - OBSIDIAN_VAULT_PATH=/var/obsidian-vault
    healthcheck:
      test: ["CMD", "/usr/local/bin/mcp-obsidian", "--help"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s
    restart: unless-stopped
    stdin_open: true
    tty: true
```

### Commands

```bash
# Build and start
make docker-build
make docker-up

# View logs
make docker-logs

# Stop
make docker-down
```

## systemd Service (Linux)

### Service File: `/etc/systemd/system/mcp-obsidian.service`

```ini
[Unit]
Description=MCP Obsidian Server
After=network.target
Wants=network-online.target

[Service]
Type=simple
User=youruser
Group=yourgroup
WorkingDirectory=/home/youruser/TurboVault
ExecStart=/usr/local/bin/mcp-obsidian \
  --vault /home/youruser/Documents/ObsidianVault \
  --profile production \
  --init
Restart=on-failure
RestartSec=10s
StandardOutput=journal
StandardError=journal
SyslogIdentifier=mcp-obsidian

# Security hardening
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict
ProtectHome=read-only
ReadWritePaths=/home/youruser/Documents/ObsidianVault

# Environment
Environment="RUST_LOG=info,TurboVault=debug"

[Install]
WantedBy=multi-user.target
```

### Setup

```bash
# 1. Copy binary to system path
sudo cp target/release/mcp-obsidian /usr/local/bin/
sudo chmod 755 /usr/local/bin/mcp-obsidian

# 2. Create service file (as shown above)
sudo nano /etc/systemd/system/mcp-obsidian.service

# 3. Reload systemd
sudo systemctl daemon-reload

# 4. Enable and start service
sudo systemctl enable mcp-obsidian
sudo systemctl start mcp-obsidian

# 5. Check status
sudo systemctl status mcp-obsidian

# 6. View logs
sudo journalctl -u mcp-obsidian -f
```

## Security Considerations

### Readonly Access

For untrusted environments or public demos:

```bash
# Use readonly profile
mcp-obsidian \
  --vault /path/to/vault \
  --profile readonly \
  --init
```

**What's Disabled:**
- All write operations (`write_note`, `delete_note`, `move_note`)
- Batch operations that modify files
- Template creation

**What Works:**
- All read operations (`read_note`, `list_files`)
- Search and discovery (`search`, `advanced_search`)
- Link graph analysis (`get_backlinks`, `get_hub_notes`)
- Health checks and exports (`export_health_report`)

### Security Features

- **Path Traversal Protection**: All paths validated against vault root
- **Input Validation**: Type-safe deserialization, no code execution
- **File Size Limits**: Prevents DoS via large files (default 5MB)
- **Security Auditing**: All operations logged in production mode
- **No Shell Commands**: Pure Rust, no external command execution
- **Principle of Least Privilege**: Docker runs as non-root user

## Monitoring and Observability

### Logging

```bash
# Environment variable controls logging
export RUST_LOG=info                # Info and above (production)
export RUST_LOG=debug               # All debug logs (development)
export RUST_LOG=warn                # Warnings and errors only
export RUST_LOG=TurboVault=debug   # Debug for TurboVault only
```

### Metrics

Built-in metrics include:
- Request count by tool
- Request duration (p50, p95, p99)
- Error rate by tool and error type
- Vault size (files, links, orphans)
- Cache hit/miss ratio

### Distributed Tracing

Set up tracing with Jaeger:

```bash
# Run Jaeger (Docker)
docker run -d \
  -p 16686:16686 \
  -p 4317:4317 \
  jaegertracing/all-in-one:latest

# Configure server
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
mcp-obsidian --vault /path/to/vault --profile production

# View traces: Open http://localhost:16686
```

## Performance Optimization

### Memory Usage

- **Base**: ~50MB for server infrastructure
- **Search Index**: ~1MB per 1000 notes
- **Link Graph**: ~500KB per 1000 notes
- **File Cache**: Configurable, TTL-based eviction

**Total for 10,000 note vault**: ~80MB

### Latency Characteristics

| Operation | Latency | Notes |
|-----------|---------|-------|
| File Read | <10ms | Direct filesystem access |
| File Write | <20ms | Atomic write via temp file |
| Simple Search | <50ms | In-memory Tantivy index |
| Advanced Search | <100ms | With filters and ranking |
| Graph Analysis | <200ms | Full vault traversal |
| Health Check | <300ms | Comprehensive metrics |

## Backup and Recovery

TurboVault doesn't manage backups. Use your preferred backup tool:

- **Git**: Version control for your vault
- **rsync**: Incremental backups
- **Cloud storage**: Automated sync (Dropbox, Google Drive, etc.)

The server only reads/writes markdown files, so any backup tool that works with files will work with TurboVault.
