# turbovault

[![Crates.io](https://img.shields.io/crates/v/turbovault.svg)](https://crates.io/crates/turbovault)
[![Docs.rs](https://docs.rs/turbovault/badge.svg)](https://docs.rs/turbovault)
[![License](https://img.shields.io/crates/l/turbovault.svg)](https://github.com/epistates/turbovault/blob/main/LICENSE)

Production-grade MCP server for Obsidian vault management.

The main executable binary that exposes 44 MCP tools for AI agents to autonomously manage Obsidian vaults. This is the entry point for end users - it orchestrates all vault operations by integrating the core, parser, graph, vault, batch, export, and tools crates into a unified Model Context Protocol server.

## What This Is

`turbovault` is the **main binary** that end users run to expose their Obsidian vault to AI agents via the Model Context Protocol (MCP). It provides:

- **44 MCP Tools**: Complete vault management API (read, write, search, analyze, templates, batch operations)
- **STDIO Transport**: Standard MCP-compliant communication over stdin/stdout
- **Full-Text Search**: Tantivy-powered search with TF-IDF ranking
- **Link Graph Analysis**: Backlinks, hubs, orphans, cycles, health scoring
- **Template System**: Pre-built templates with field validation
- **Batch Operations**: Atomic multi-file transactions
- **Export & Reporting**: JSON/CSV exports for health reports, stats, broken links
- **Production Observability**: OpenTelemetry, structured logging, metrics, tracing

**For AI agents (Claude, GPT, etc.)**: This server makes your Obsidian vault "programmable" through a type-safe, discoverable API.

**For end users**: Install once, configure your vault path, and connect to Claude or other MCP clients.

## Quick Start

### 1. Build the Binary

```bash
# From project root
make release

# Binary is at: target/release/turbovault
```

### 2. Run with Your Vault

```bash
# Simplest usage (single vault, STDIO mode)
./target/release/turbovault \
  --vault /path/to/your/obsidian/vault \
  --init

# The --init flag scans the vault and builds the link graph on startup
```

### 3. Connect to Claude

Add to your `~/.config/claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "obsidian": {
      "command": "/path/to/turbovault",
      "args": [
        "--vault", "/path/to/your/vault",
        "--profile", "production",
        "--init"
      ]
    }
  }
}
```

Restart Claude Desktop. The server will now be available to Claude.

## Installation

### Option 1: cargo install (Recommended for End Users)

Install directly from crates.io (after publishing):

```bash
# Minimal install (STDIO only, ~7.0 MB)
# Perfect for Claude Desktop
cargo install turbovault

# With HTTP server support (~8.2 MB)
cargo install turbovault --features http

# With HTTP + WebSocket (~8.5 MB)
cargo install turbovault --features "http,websocket"

# With all transports (~8.8 MB)
cargo install turbovault --features full

# Binary installed to: ~/.cargo/bin/turbovault
turbovault --help
```

**Available Feature Flags (a la carte):**

| Feature | Binary Size | Use Case |
|---------|-------------|----------|
| `(none)` | **7.0 MB** | Default: STDIO only, Claude Desktop standard |
| `http` | 8.2 MB | Add HTTP server (+1.2 MB) |
| `websocket` | 8.3 MB | Add WebSocket support |
| `tcp` | 7.2 MB | Add TCP server |
| `unix` | 7.2 MB | Add Unix socket support |
| `http-full` | 8.5 MB | HTTP + WebSocket (convenience bundle) |
| `all-transports` | 8.8 MB | All transports enabled |
| `full` | 8.8 MB | Alias for all-transports |

**Mix and Match Examples:**

```bash
# HTTP + TCP (no WebSocket or Unix)
cargo install turbovault --features "http,tcp"

# WebSocket + Unix socket
cargo install turbovault --features "websocket,unix"

# Just HTTP
cargo install turbovault --features http
```

**Why Choose Minimal?**
- **Faster downloads** (~20% smaller)
- **Lower disk usage** on constrained systems
- **Faster startup** (less code to initialize)
- **Claude Desktop only needs STDIO** (HTTP/WebSocket/TCP/Unix unused)

**When to Add Features:**
- `http`: Building a web interface or REST API
- `websocket`: Real-time browser-based clients
- `tcp`: Network-based MCP clients
- `unix`: Local IPC with Unix domain sockets

### Option 2: Build from Source

```bash
# Clone the repository
git clone https://github.com/epistates/TurboVault.git
cd TurboVault

# Build with default features (STDIO only, ~7.0 MB)
make release

# Or build with specific features
cargo build -p turbovault-server --release --features "http,websocket"

# Install to /usr/local/bin (optional)
sudo cp target/release/turbovault /usr/local/bin/

# Verify installation
turbovault --help
```

### Option 3: Docker

```bash
# Build Docker image
make docker-build

# Run with docker-compose (see docker-compose.yml for config)
make docker-up

# View logs
make docker-logs
```

### Option 3: systemd Service (Linux)

See [Deployment Guide](../../docs/deployment/index.md) for systemd setup.

## Configuration

### Configuration Profiles

Pre-built configuration profiles optimized for different use cases:

| Profile | Use Case | Features |
|---------|----------|----------|
| `development` | Local development | Verbose logging, file watching enabled, permissive validation |
| `production` | Production deployments | Info logging, security auditing, performance monitoring |
| `readonly` | Read-only access | Disables all write operations, audit logging enabled |
| `high-performance` | Large vaults (10k+ notes) | Aggressive caching, disabled file watching, optimized for speed |
| `minimal` | Resource-constrained environments | Minimal caching, basic features only |

**Usage:**

```bash
# Use a profile via CLI
turbovault --vault /path/to/vault --profile production

# Default is "development" if not specified
```

### Vault Configuration

Vaults are configured programmatically via `VaultConfig::builder()`:

```rust
use TurboVault_core::{VaultConfig, ConfigProfile};

// Create configuration
let mut config = ConfigProfile::Production.create_config();

// Add vault with defaults
let vault_config = VaultConfig::builder("my-vault", "/path/to/vault")
    .build()?;
config.vaults.push(vault_config);

// Add vault with custom settings
let custom_vault = VaultConfig::builder("research", "/path/to/research")
    .as_default()
    .with_watch_enabled(true)
    .with_max_file_size(10 * 1024 * 1024)  // 10MB
    .with_cache_enabled(true)
    .with_cache_ttl(3600)  // 1 hour
    .with_excluded_paths(vec![".obsidian", ".trash"])
    .build()?;
config.vaults.push(custom_vault);
```

**Available Options:**

- `name`: Unique identifier for the vault
- `path`: Filesystem path to vault root
- `is_default`: Mark as default vault (first vault is default if not specified)
- `watch_for_changes`: Enable file watching for live updates (default: true)
- `max_file_size`: Maximum file size in bytes (default: 5MB)
- `allowed_extensions`: File extensions to process (default: `.md`)
- `excluded_paths`: Paths to exclude from scanning (default: `.obsidian`, `.trash`)
- `enable_caching`: Enable file content caching (default: true)
- `cache_ttl`: Cache time-to-live in seconds (default: 300 = 5 minutes)
- `template_dirs`: Additional template directories (default: vault root)
- `allowed_operations`: Restrict operations (default: all allowed)

### Environment Variables

```bash
# Vault path (alternative to --vault CLI arg)
export OBSIDIAN_VAULT_PATH=/path/to/vault

# Logging level (default: info for production, debug for development)
export RUST_LOG=info,TurboVault=debug

# OpenTelemetry endpoint (if using OTLP export)
export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
```

## Usage Scenarios

### 1. Single Vault with Claude Desktop

**Goal**: Connect your personal Obsidian vault to Claude.

**Steps:**

```bash
# 1. Build the server
cd TurboVault
make release

# 2. Test it works
./target/release/turbovault \
  --vault ~/Documents/ObsidianVault \
  --init

# You should see:
# [INFO] Observability initialized
# [INFO] Initializing vault at: ~/Documents/ObsidianVault
# [INFO] Scanning vault and building link graph...
# [INFO] Vault initialization complete
# [INFO] Server initialized with vault
# [INFO] Starting TurboVault Server
# [INFO] Running in STDIO mode for MCP protocol

# 3. Configure Claude Desktop
# Edit: ~/.config/claude/claude_desktop_config.json
```

```json
{
  "mcpServers": {
    "obsidian": {
      "command": "/Users/yourname/TurboVault/target/release/turbovault",
      "args": [
        "--vault", "/Users/yourname/Documents/ObsidianVault",
        "--profile", "production",
        "--init"
      ]
    }
  }
}
```

```bash
# 4. Restart Claude Desktop
# The server is now available to Claude!
```

**Verification:**

Ask Claude:
- "What tools do you have available?"
- "Can you list the files in my vault?"
- "Search for notes about machine learning"
- "What's the health score of my vault?"

### 2. Multi-Vault Setup

**Goal**: Manage multiple vaults (personal, work, research) with a single server.

**Implementation:**

Multi-vault support requires using the `MultiVaultManager` API (currently requires code changes, CLI support coming soon):

```rust
// Create multi-vault configuration
use TurboVault_core::MultiVaultManager;

let manager = MultiVaultManager::new();

// Add vaults
manager.add_vault("personal", "/vaults/personal").await?;
manager.add_vault("work", "/vaults/work").await?;
manager.add_vault("research", "/vaults/research").await?;

// Set active vault
manager.set_active_vault("personal").await?;

// Initialize server
server.initialize_multi_vault(manager).await;
```

**Use Cases:**

- Separate personal and work knowledge bases
- Isolate research projects
- Team collaboration with shared vaults
- Client-specific vaults for consulting

**Switching Vaults (via MCP tools):**

Claude can use:
- `list_vaults()` - See all registered vaults
- `get_active_vault()` - Check current vault
- `set_active_vault("work")` - Switch to different vault
- `get_vault_config("research")` - View vault settings

### 3. Docker Deployment

**Goal**: Run the server in a container for isolation and reproducibility.

**docker-compose.yml:**

```yaml
version: '3.8'

services:
  turbovault:
    build:
      context: .
      dockerfile: Dockerfile
    image: TurboVault:latest
    container_name: turbovault
    user: obsidian
    volumes:
      # Mount your vault (read-write)
      - /path/to/your/vault:/var/obsidian-vault
    environment:
      - RUST_LOG=info
      - OBSIDIAN_VAULT_PATH=/var/obsidian-vault
    healthcheck:
      test: ["CMD", "/usr/local/bin/turbovault", "--help"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s
    restart: unless-stopped
    stdin_open: true
    tty: true
```

**Commands:**

```bash
# Build and start
make docker-build
make docker-up

# View logs
make docker-logs

# Stop
make docker-down

# Connect to running container
docker exec -it turbovault /bin/sh
```

**Benefits:**

- Isolated dependencies (no Rust toolchain needed on host)
- Reproducible environment
- Easy rollback (just change image tag)
- Works on any platform (Linux, macOS, Windows with WSL)

### 4. Readonly Access (Security)

**Goal**: Expose vault to AI agents without allowing modifications.

**Configuration:**

```bash
# Use readonly profile
turbovault \
  --vault /path/to/vault \
  --profile readonly \
  --init
```

**What's Disabled:**

- All write operations (`write_note`, `delete_note`, `move_note`)
- Batch operations that modify files
- Template creation
- File operations fail with permission errors

**What Works:**

- All read operations (`read_note`, `list_files`)
- Search and discovery (`search`, `advanced_search`)
- Link graph analysis (`get_backlinks`, `get_hub_notes`)
- Health checks and exports (`export_health_report`)

**Use Cases:**

- Public demo environments
- Shared vault access with untrusted agents
- Audit mode (agents can observe but not modify)
- Testing agent behavior safely

## CLI Reference

### Command Line Arguments

```bash
turbovault [OPTIONS]
```

**Options:**

| Flag | Environment Variable | Default | Description |
|------|---------------------|---------|-------------|
| `--vault <PATH>` | `OBSIDIAN_VAULT_PATH` | (required) | Path to Obsidian vault directory |
| `--profile <PROFILE>` | - | `development` | Configuration profile: `development`, `production`, `readonly`, `high-performance`, `minimal` |
| `--transport <MODE>` | - | `stdio` | Transport mode (only `stdio` is MCP-compliant) |
| `--port <PORT>` | - | `3000` | HTTP server port (for future http transport) |
| `--init` | - | `false` | Initialize vault on startup (scan files, build graph) |
| `--help` | - | - | Show help message |
| `--version` | - | - | Show version |

### Examples

```bash
# Minimal usage (development mode, no init)
turbovault --vault /path/to/vault

# Production mode with initialization
turbovault \
  --vault /path/to/vault \
  --profile production \
  --init

# Readonly mode (no modifications allowed)
turbovault \
  --vault /path/to/vault \
  --profile readonly

# High-performance mode (large vaults)
turbovault \
  --vault /path/to/vault \
  --profile high-performance \
  --init

# Use environment variable for vault path
export OBSIDIAN_VAULT_PATH=/path/to/vault
turbovault --profile production --init
```

### Exit Codes

- `0` - Success
- `1` - General error (vault not found, invalid config, etc.)
- `2` - Vault initialization failed
- `3` - Server startup failed

## Claude Integration

### Setup

1. **Install Claude Desktop**: Download from [Anthropic](https://claude.ai/download)

2. **Build turbovault-server**:
   ```bash
   cd TurboVault
   make release
   ```

3. **Configure Claude Desktop**:

   **macOS/Linux**: `~/.config/claude/claude_desktop_config.json`
   **Windows**: `%APPDATA%\Claude\claude_desktop_config.json`

   ```json
   {
     "mcpServers": {
       "obsidian": {
         "command": "/absolute/path/to/turbovault",
         "args": [
           "--vault", "/absolute/path/to/your/vault",
           "--profile", "production",
           "--init"
         ]
       }
     }
   }
   ```

4. **Restart Claude Desktop**

5. **Verify Connection**:
   - Open Claude Desktop
   - Look for the "MCP" indicator in the UI
   - Ask: "What MCP tools do you have?"
   - Claude should list 44 Obsidian tools

### Example Workflows with Claude

#### 1. Search and Summarize

**You:** "Search my vault for notes about Rust async programming and summarize the key concepts."

**Claude will:**
1. Use `search(query="rust async programming")`
2. Read the top results with `read_note(path=...)`
3. Summarize the content

#### 2. Vault Health Analysis

**You:** "Analyze the health of my vault and suggest improvements."

**Claude will:**
1. Use `quick_health_check()` to get overall score
2. Use `get_broken_links()` to find issues
3. Use `get_hub_notes()` to identify important notes
4. Provide actionable recommendations

#### 3. Create Structured Notes

**You:** "Create a task note for implementing user authentication with high priority."

**Claude will:**
1. Use `list_templates()` to find available templates
2. Use `create_from_template(template_id="task", path="tasks/user-auth.md", fields={"title":"User Authentication","priority":"high"})`
3. Confirm creation and suggest related notes to link

#### 4. Organize and Refactor

**You:** "Find all completed project notes and move them to the archive folder."

**Claude will:**
1. Use `query_metadata(pattern='status: "completed"')`
2. Build a list of files to move
3. Use `batch_execute()` to atomically move files and update backlinks
4. Report the results

### Troubleshooting Claude Connection

**Problem: Claude doesn't see the server**

1. Check Claude Desktop logs:
   ```bash
   # macOS
   tail -f ~/Library/Logs/Claude/mcp*.log

   # Linux
   tail -f ~/.config/Claude/logs/mcp*.log
   ```

2. Verify server runs standalone:
   ```bash
   /path/to/turbovault --vault /path/to/vault --init
   ```

3. Check config file syntax (must be valid JSON)

4. Use absolute paths (not `~` or relative paths)

**Problem: Server starts but tools fail**

- Check vault path is correct and accessible
- Verify vault contains `.obsidian` folder (valid Obsidian vault)
- Check file permissions (server needs read/write access)
- Review server logs in Claude's MCP logs

## Observability

The server includes production-grade observability via OpenTelemetry.

### Logging

**Log Levels:**

```bash
# Environment variable controls logging
export RUST_LOG=debug              # All debug logs
export RUST_LOG=info               # Info and above
export RUST_LOG=warn               # Warnings and errors only
export RUST_LOG=TurboVault=debug  # Debug for TurboVault only

# Multi-crate filtering
export RUST_LOG=info,TurboVault=debug,turbomcp=trace
```

**Log Output:**

```
[2025-10-16T10:30:00Z INFO  TurboVault] Observability initialized
[2025-10-16T10:30:00Z INFO  TurboVault] Initializing vault at: /path/to/vault
[2025-10-16T10:30:01Z INFO  TurboVault::vault] Scanning vault files...
[2025-10-16T10:30:02Z INFO  TurboVault::graph] Building link graph (1250 nodes)...
[2025-10-16T10:30:03Z INFO  TurboVault] Vault initialization complete
[2025-10-16T10:30:03Z INFO  TurboVault] Server initialized with vault
[2025-10-16T10:30:03Z INFO  TurboVault] Starting TurboVault Server
[2025-10-16T10:30:03Z INFO  TurboVault] Running in STDIO mode for MCP protocol
```

### Metrics

**Built-in Metrics:**

- Request count by tool
- Request duration (p50, p95, p99)
- Error rate by tool and error type
- Vault size (files, links, orphans)
- Cache hit/miss ratio
- File I/O operations
- Graph analysis performance

**Accessing Metrics:**

Metrics are emitted via OpenTelemetry and can be exported to:

- **Prometheus**: Scrape endpoint (future HTTP transport)
- **OTLP Collector**: Push to collector for aggregation
- **Cloud Providers**: AWS CloudWatch, GCP Monitoring, Azure Monitor

**Example Prometheus Queries:**

```promql
# Average request duration by tool
rate(mcp_request_duration_seconds_sum[5m]) / rate(mcp_request_duration_seconds_count[5m])

# Error rate
rate(mcp_request_errors_total[5m])

# Cache efficiency
mcp_cache_hits_total / (mcp_cache_hits_total + mcp_cache_misses_total)
```

### Tracing

**Distributed Tracing:**

The server emits OpenTelemetry spans for:

- Each MCP tool invocation
- File operations (read, write, delete)
- Search queries
- Graph analysis
- Batch transactions

**Viewing Traces:**

1. **Set up OTLP collector**:
   ```bash
   # docker-compose.yml
   services:
     jaeger:
       image: jaegertracing/all-in-one:latest
       ports:
         - "16686:16686"  # Jaeger UI
         - "4317:4317"    # OTLP receiver
   ```

2. **Configure server**:
   ```bash
   export OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4317
   turbovault --vault /path/to/vault --profile production
   ```

3. **View traces**: Open http://localhost:16686

**Trace Example:**

```
Span: search_notes
├─ Span: parse_query (0.1ms)
├─ Span: tantivy_search (45ms)
│  ├─ Span: index_lookup (30ms)
│  └─ Span: score_results (15ms)
└─ Span: format_results (2ms)
Total: 47.1ms
```

### Security Auditing

When `enable_security_auditing()` is enabled (production profile):

- All file operations are logged with user context
- Path traversal attempts are logged and blocked
- Invalid access attempts are recorded
- Configuration changes are audited

**Audit Log Example:**

```json
{
  "timestamp": "2025-10-16T10:30:05Z",
  "level": "WARN",
  "message": "Path traversal attempt blocked",
  "fields": {
    "requested_path": "../../etc/passwd",
    "vault_root": "/path/to/vault",
    "operation": "read_note",
    "client": "claude-desktop"
  }
}
```

## Performance Tuning

### For Small Vaults (<1000 notes)

**Use development profile:**

```bash
turbovault --vault /path/to/vault --profile development
```

**Defaults are fine** - caching, file watching, full indexing.

### For Medium Vaults (1k-10k notes)

**Use production profile:**

```bash
turbovault --vault /path/to/vault --profile production --init
```

**Tuning:**

- Enable caching with 1-hour TTL
- Use `--init` to build graph once on startup
- Limit search results to 20-50 per query
- Use `quick_health_check()` instead of `full_health_analysis()`

### For Large Vaults (10k+ notes)

**Use high-performance profile:**

```bash
turbovault --vault /path/to/vault --profile high-performance --init
```

**Tuning:**

- Disable file watching (reduces CPU overhead)
- Aggressive caching with long TTLs
- Limit search results to 10-20
- Use pagination for large result sets
- Consider splitting into multiple vaults
- Increase cache size (code change required)

**Memory Optimization:**

```rust
// In code (future CLI options)
config.max_cache_entries = 10000;      // Increase from default 1000
config.cache_ttl = 7200;               // 2 hours instead of 5 minutes
config.enable_lazy_loading = true;     // Load files on-demand
```

**Disk I/O Optimization:**

- Use SSD for vault storage
- Exclude large asset directories (images, PDFs) if not needed
- Use `.ignore` file to exclude temporary files

### Benchmarking

```bash
# Run benchmarks (if available)
cargo bench -p turbovault-server

# Profile with perf (Linux)
perf record --call-graph dwarf ./target/release/turbovault --vault /path/to/vault
perf report

# Memory profiling with valgrind (Linux)
valgrind --tool=massif ./target/release/turbovault --vault /path/to/vault
```

## Troubleshooting

### Common Issues

#### 1. Vault Not Found

**Error:**
```
Error: Failed to create vault config: Vault path does not exist: /path/to/vault
```

**Solution:**
- Verify the path is correct: `ls -la /path/to/vault`
- Use absolute paths (not relative or `~`)
- Ensure vault directory exists and contains `.obsidian/` folder
- Check file permissions

#### 2. Server Starts But No Tools Available

**Symptoms:**
- Server runs without errors
- Claude doesn't see any tools
- MCP connection shows but no tools listed

**Solution:**
- Check server was initialized with vault:
  ```bash
  # Look for these log lines:
  [INFO] Server initialized with vault
  [INFO] Vault: /path/to/vault
  ```
- If missing, use `--vault` flag:
  ```bash
  turbovault --vault /path/to/vault --init
  ```
- Verify vault is valid (contains `.obsidian/` and `.md` files)

#### 3. Permission Denied Errors

**Error:**
```
Error: Permission denied (os error 13)
```

**Solution:**
- Check vault directory permissions:
  ```bash
  ls -ld /path/to/vault
  # Should show read/write for your user
  ```
- Fix permissions:
  ```bash
  chmod -R u+rw /path/to/vault
  ```
- For Docker: ensure volume mount is correct and user has access

#### 4. Link Graph Build Fails

**Error:**
```
[ERROR] Failed to initialize vault: Link parsing error
```

**Solution:**
- Check for malformed wikilinks in notes:
  ```bash
  # Find suspicious links
  grep -r '\[\[.*\[\[' /path/to/vault/*.md
  ```
- Look for unclosed links: `[[link` (missing `]]`)
- Check for special characters in link targets
- Review server logs for specific file causing issues

#### 5. Search Returns No Results

**Symptoms:**
- `search()` tool returns empty array
- You know matching content exists

**Solution:**
- Ensure vault was initialized with `--init` flag
- Check search query syntax (use simple keywords first)
- Verify files have content (not just frontmatter)
- Check excluded paths (might be filtering out results)
- Review search index build in logs:
  ```
  [INFO] Building search index... (1250 files)
  [INFO] Search index ready
  ```

#### 6. High Memory Usage

**Symptoms:**
- Server using excessive RAM (>500MB for <10k notes)
- System becomes sluggish

**Solution:**
- Use `high-performance` profile (disables file watching)
- Reduce cache TTL:
  ```bash
  # Shorten cache lifetime
  turbovault --vault /path/to/vault --profile production
  ```
- Check for memory leak (shouldn't happen, but report if found)
- Restart server periodically (systemd handles this)

#### 7. Slow Performance

**Symptoms:**
- Operations take >5 seconds
- Search is slow (>1 second)

**Solution:**
- Use `--init` to build graph once on startup (not on-demand)
- Profile the slow operation:
  ```bash
  export RUST_LOG=debug
  # Look for slow operations in logs
  ```
- Check disk I/O (vault on slow HDD?)
- Reduce vault size or split into multiple vaults
- Disable file watching in `high-performance` profile

### Debug Mode

**Enable detailed logging:**

```bash
export RUST_LOG=debug
turbovault --vault /path/to/vault --init 2>&1 | tee debug.log
```

**Send debug logs when reporting issues:**

```bash
# Sanitize logs (remove sensitive paths)
sed 's|/Users/yourname/|/home/user/|g' debug.log > debug-sanitized.log
# Attach debug-sanitized.log to issue report
```

### Getting Help

1. **Check logs**: Look for ERROR or WARN messages
2. **Search issues**: https://github.com/epistates/TurboVault/issues
3. **Create issue**: Include:
   - Server version (`turbovault --version`)
   - OS and Rust version (`rustc --version`)
   - Vault size (number of files)
   - Minimal reproduction steps
   - Relevant log output (sanitized)

## Development

### Building from Source

```bash
# Clone repository
git clone https://github.com/epistates/TurboVault.git
cd TurboVault

# Build debug binary (fast compile, slower runtime)
cargo build -p turbovault-server

# Build release binary (slow compile, optimized runtime)
cargo build -p turbovault-server --release

# Run tests
cargo test -p turbovault-server

# Run with cargo
cargo run -p turbovault-server -- --vault /path/to/vault --init
```

### Project Structure

```
crates/turbovault-server/
├── src/
│   ├── bin/
│   │   └── main.rs           # CLI entry point, arg parsing, server startup
│   ├── lib.rs                # Re-exports for public API
│   └── tools.rs              # MCP tool implementations (44 tools)
├── tests/
│   └── integration_test.rs   # Integration tests
├── Cargo.toml                # Dependencies and binary config
└── README.md                 # This file
```

### Adding a New Tool

1. **Implement in `tools.rs`**:

   ```rust
   #[tool("my_new_tool")]
   async fn my_new_tool(&self, param: String) -> McpResult<String> {
       let manager = self.get_manager().await?;
       let tools = MyTools::new(manager);
       let result = tools.my_operation(&param).await.map_err(to_mcp_error)?;
       Ok(result)
   }
   ```

2. **Add to turbovault-tools** (if reusable logic):

   ```rust
   // crates/turbovault-tools/src/my_tools.rs
   pub struct MyTools {
       pub manager: Arc<VaultManager>,
   }

   impl MyTools {
       pub async fn my_operation(&self, param: &str) -> Result<String> {
           // Implementation
       }
   }
   ```

3. **Write tests**:

   ```rust
   #[tokio::test]
   async fn test_my_new_tool() {
       let (_temp, manager) = create_test_vault().await;
       let server = ObsidianMcpServer::new();
       server.initialize(manager).await;

       // Test tool invocation
       let result = server.my_new_tool("test".to_string()).await;
       assert!(result.is_ok());
   }
   ```

4. **Update documentation** in this README

### Running Tests

```bash
# All server tests
cargo test -p turbovault-server

# Specific test
cargo test -p turbovault-server test_server_initialization

# With output
cargo test -p turbovault-server -- --nocapture

# Integration tests only
cargo test -p turbovault-server --test integration_test
```

### Code Quality

```bash
# Format code
cargo fmt --all

# Run linter
cargo clippy --all -- -D warnings

# Check compilation without building
cargo check -p turbovault-server
```

## Architecture

### System Overview

```
┌─────────────────────────────────────────────────────────┐
│                    AI Agent (Claude)                     │
└───────────────────────────┬─────────────────────────────┘
                            │ MCP Protocol (STDIO)
┌───────────────────────────▼─────────────────────────────┐
│              turbovault-server (THIS CRATE)            │
│                                                          │
│  ┌────────────────────────────────────────────────┐    │
│  │  main.rs - CLI Entry Point                     │    │
│  │  - Parse args (vault path, profile)            │    │
│  │  - Initialize observability (OTLP)             │    │
│  │  - Create VaultManager                          │    │
│  │  - Start MCP server (STDIO transport)          │    │
│  └────────────────────────────────────────────────┘    │
│                                                          │
│  ┌────────────────────────────────────────────────┐    │
│  │  tools.rs - MCP Tool Implementations           │    │
│  │  - ObsidianMcpServer struct                    │    │
│  │  - 44 #[tool] annotated methods                │    │
│  │  - Error conversion (Error → McpError)         │    │
│  └────────────────────────────────────────────────┘    │
└───────────────────────────┬─────────────────────────────┘
                            │
        ┌───────────────────┼───────────────────┐
        │                   │                   │
┌───────▼──────┐   ┌───────▼──────┐   ┌───────▼──────┐
│ TurboVault- │   │ TurboVault- │   │  turbomcp    │
│   tools      │   │   vault      │   │  (MCP SDK)   │
│              │   │              │   │              │
│ - FileTools  │   │ VaultManager │   │ - Protocol   │
│ - SearchEng  │   │ - File I/O   │   │ - Transport  │
│ - Templates  │   │ - Caching    │   │ - Macros     │
│ - GraphTools │   │ - Validation │   │ - Observ.    │
└──────────────┘   └──────────────┘   └──────────────┘
        │                   │
        └───────────────────┼───────────────────┐
                            │                   │
                  ┌─────────▼─────────┐ ┌───────▼──────┐
                  │ turbovault-parser │ │ TurboVault- │
                  │                    │ │   graph      │
                  │ - OFM parsing      │ │              │
                  │ - Wikilink extract │ │ - Link graph │
                  │ - Frontmatter      │ │ - Analysis   │
                  └────────────────────┘ └──────────────┘
```

### Component Responsibilities

| Component | Responsibility | Lines of Code |
|-----------|---------------|---------------|
| `main.rs` | CLI parsing, initialization, startup | ~100 LOC |
| `tools.rs` | MCP tool wrappers, error conversion | ~500 LOC |
| `lib.rs` | Public API exports | ~10 LOC |

**Key Design Decisions:**

1. **Thin Server Layer**: Business logic lives in `turbovault-tools`, server just wraps it
2. **Single Binary**: All crates compile into one executable for easy deployment
3. **STDIO Transport Only**: MCP standard, simplifies deployment (no network ports)
4. **Observability First**: Production-grade logging/metrics/tracing from day one
5. **Profile-Based Config**: Common use cases have pre-tuned profiles

### Data Flow: MCP Request → Response

```
1. Claude sends JSON-RPC request via STDIO
   ↓
2. turbomcp deserializes request
   ↓
3. Server routes to tool method (e.g., `read_note()`)
   ↓
4. Tool method gets VaultManager
   ↓
5. Tool method creates domain tool (e.g., FileTools)
   ↓
6. Domain tool performs operation (read file, parse, etc.)
   ↓
7. Result or error returned
   ↓
8. Server converts Error → McpError
   ↓
9. turbomcp serializes response to JSON-RPC
   ↓
10. Response sent to Claude via STDIO
```

**Latency Breakdown (typical):**

- JSON-RPC parsing: <1ms
- Tool routing: <1ms
- Domain operation: 5-100ms (depends on operation)
- Response serialization: <1ms
- **Total: 7-102ms**

### Crate Dependencies

```
turbovault-server depends on:
├── turbovault-core (types, config, errors)
├── turbovault-vault (file I/O, VaultManager)
├── turbovault-tools (all 11 tool categories)
├── turbomcp (MCP protocol + macros)
│   ├── turbomcp-protocol (JSON-RPC)
│   └── turbomcp-server (server infrastructure)
├── tokio (async runtime)
├── clap (CLI parsing)
├── serde + serde_json (serialization)
├── config (configuration loading - future use)
├── anyhow (error handling)
├── log + env_logger (logging)
└── tracing + tracing-subscriber (structured logging)
```

### Security Architecture

**Path Validation:**
- All file paths validated against vault root
- Symlinks are NOT followed (prevents escaping vault)
- `..` components rejected
- Absolute paths outside vault rejected

**Input Validation:**
- All MCP inputs deserialized via serde (type-safe)
- String inputs sanitized for shell injection (not passed to shell)
- File sizes checked before reading (max 5MB default)
- Frontmatter YAML parsed safely (no code execution)

**Error Handling:**
- No panics in tool methods (all errors returned as `Result`)
- Sensitive paths redacted from error messages
- Stack traces only in debug mode

**Audit Trail:**
- All file operations logged with timestamp
- Security events (path traversal attempts) logged at WARN level
- Configuration changes recorded

## References

For more details on specific components:

- **Core Types & Config**: See `../turbovault-core/README.md`
- **Parser (OFM)**: See `../turbovault-parser/README.md`
- **Link Graph**: See `../turbovault-graph/README.md`
- **Vault Operations**: See `../turbovault-vault/README.md`
- **Batch Transactions**: See `../turbovault-batch/README.md`
- **Export Tools**: See `../turbovault-export/README.md`
- **MCP Tools (44 tools)**: See `../turbovault-tools/README.md`
- **Deployment Guide**: See `/docs/deployment/index.md` (project root)
- **Code Quality Audit**: See `/DILIGENCE_PASS_COMPLETE.md` (project root)

## License

Part of the TurboVault project. See project root for license information.

## Support

- **Issues**: https://github.com/epistates/TurboVault/issues
- **Documentation**: This README + component READMEs
- **Examples**: See `tests/integration_test.rs` for usage examples

---

**Production Status**: ✅ Ready for production use with Claude Desktop and MCP-compliant clients.