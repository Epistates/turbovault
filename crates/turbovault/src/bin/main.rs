//! TurboVault Server CLI

use clap::Parser;
use std::path::PathBuf;
use turbomcp::telemetry::TelemetryConfig;
use turbomcp::{McpServerExt, ProtocolConfig, VisibilityLayer};
use turbovault::ObsidianMcpServer;
use turbovault::tool_visibility::{
    ToolVisibilityOverrides, ToolVisibilitySettings, TurboVaultConfigFile, default_config_path,
};
use turbovault_core::VaultConfig;
use turbovault_core::cache::VaultCache;
use turbovault_tools::OutputFormat;

/// TurboVault Server - AI-powered vault management
/// `--version` string. On debug builds it appends the build's short git SHA
/// (best-effort, captured by `build.rs`) so a dev/dogfood binary self-identifies
/// which commit it was built from; release builds show the bare crate version.
#[cfg(debug_assertions)]
const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "+", env!("TURBOVAULT_GIT_SHA"));
#[cfg(not(debug_assertions))]
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(all(test, debug_assertions))]
mod version_tests {
    #[test]
    fn debug_version_appends_git_sha() {
        assert!(
            super::VERSION.starts_with(env!("CARGO_PKG_VERSION")),
            "VERSION starts with the crate version: {}",
            super::VERSION
        );
        assert!(
            super::VERSION.contains('+'),
            "debug VERSION appends +<sha>: {}",
            super::VERSION
        );
        assert!(
            !super::VERSION.ends_with('+'),
            "a sha must follow the +: {}",
            super::VERSION
        );
    }
}

#[derive(Parser, Debug)]
#[command(author, version = VERSION, about, long_about = None)]
struct Args {
    /// Path to the Obsidian vault directory
    #[arg(short, long, env = "OBSIDIAN_VAULT_PATH")]
    vault: Option<PathBuf>,

    /// Configuration profile to use (development, production, etc.)
    #[arg(short, long, default_value = "development")]
    profile: String,

    /// Transport mode (stdio, http, websocket)
    #[arg(short, long, default_value = "stdio")]
    transport: String,

    /// HTTP server port (for http transport)
    #[arg(long, default_value = "3000")]
    port: u16,

    /// Output format for non-STDIO transports (json, human, text)
    /// Note: STDIO transport always uses JSON per MCP protocol specification
    #[arg(long, default_value = "json")]
    output_format: String,

    /// Initialize vault on startup (scan and build graph)
    #[arg(long, action = clap::ArgAction::SetTrue)]
    init: bool,

    /// TurboVault YAML config path. Reads the `tool_visibility` section if present.
    #[arg(long, env = "TURBOVAULT_CONFIG")]
    config: Option<PathBuf>,

    /// Comma-separated exact tool names to expose. If set, only these tools are listed/callable.
    #[arg(long, value_delimiter = ',', env = "TURBOVAULT_ALLOWED_TOOLS")]
    allowed_tools: Vec<String>,

    /// Comma-separated exact tool names to hide from tools/list while keeping direct calls allowed.
    #[arg(long, value_delimiter = ',', env = "TURBOVAULT_HIDDEN_TOOLS")]
    hidden_tools: Vec<String>,

    /// Comma-separated exact tool names to remove from tools/list and reject on direct calls.
    #[arg(long, value_delimiter = ',', env = "TURBOVAULT_DISABLED_TOOLS")]
    disabled_tools: Vec<String>,

    /// Hide all tools not annotated as read-only by TurboMCP.
    #[arg(
        long,
        env = "TURBOVAULT_REQUIRE_READ_ONLY_TOOLS",
        action = clap::ArgAction::SetTrue
    )]
    require_read_only_tools: bool,

    /// turbovault-bna: enable the Prometheus metrics exporter, binding
    /// `127.0.0.1:<port>/metrics`. Off unless set. Exposes
    /// `turbovault_apply_transaction_{seconds,total}` (substrate-op latency +
    /// outcome). Requires JSON log format (stdio, or http/ws with
    /// --output-format json) — the human-readable SimpleLogger path can't
    /// carry the exporter.
    #[arg(long, env = "TURBOVAULT_METRICS_PORT")]
    metrics_port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command-line arguments
    let mut args = Args::parse();
    // turbovault-01n: `--config` / TURBOVAULT_CONFIG was bare PathBuf; `~`
    // and env-var references in the supplied path weren't expanded
    // (asymmetric with --vault which already does this). Resolve up-front
    // so every downstream consumer sees the absolute path.
    if let Some(raw) = args.config.as_ref() {
        let expanded = shellexpand::full(&raw.to_string_lossy())
            .map_err(|e| format!("Failed to expand --config path: {}", e))?
            .into_owned();
        args.config = Some(PathBuf::from(expanded));
    }
    let tool_visibility = load_tool_visibility(&args).await?;

    // Validate output format (unless STDIO transport, which always uses JSON)
    let output_format = if args.transport == "stdio" {
        OutputFormat::Json
    } else {
        args.output_format.parse::<OutputFormat>()?
    };

    // Initialize logging based on transport
    // STDIO: Must use structured JSON logging to stderr (TurboMCP observability)
    // HTTP/WebSocket/TCP: Can use human-readable stdout logging
    let _observability_guard = if args.transport == "stdio" {
        // STDIO: Use TurboMCP's structured observability (JSON to stderr)
        Some(telemetry_config(&args.profile, true, args.metrics_port).init()?)
    } else {
        // HTTP/WebSocket/TCP: Use simple logger with configurable format
        use simple_logger::SimpleLogger;

        match output_format {
            OutputFormat::Json => {
                // JSON format for programmatic parsing (HTTP/WS can use stdout)
                Some(telemetry_config(&args.profile, false, args.metrics_port).init()?)
            }
            OutputFormat::Human | OutputFormat::Text => {
                // turbovault-bna: the human/text path uses SimpleLogger, which
                // can't carry the Prometheus exporter — warn loudly rather than
                // silently dropping the operator's --metrics-port request.
                if let Some(port) = args.metrics_port {
                    log::warn!(
                        "--metrics-port {} ignored: the {:?} output format uses SimpleLogger, which can't host the Prometheus exporter. Use --output-format json (or stdio transport) for metrics.",
                        port,
                        output_format
                    );
                }
                // Human-readable format for terminal/stdout.
                // turbovault-foy: honor RUST_LOG for SimpleLogger too; the
                // env_logger-style filter strings (e.g. "info,turbo_vault=debug")
                // are SimpleLogger-compatible at the comma-separated module:level
                // shape but with_level wants a single LevelFilter — so we parse
                // the FIRST component as the global threshold and ignore
                // module-specific overrides (they would need module_loggers()
                // which SimpleLogger supports but is more verbose).
                let level = parse_simplelogger_level(&args.profile);
                SimpleLogger::new()
                    .with_level(level)
                    .with_utc_timestamps()
                    .init()
                    .map_err(|e| format!("Failed to initialize logger: {}", e))?;
                None
            }
        }
    };

    log::info!("Turbo Vault MCP Server v{}", env!("CARGO_PKG_VERSION"));
    log::info!(
        "Transport: {} | Log format: {:?}",
        args.transport,
        output_format
    );
    // turbovault-bna: confirm the metrics endpoint when enabled (the exporter
    // is wired into the observability guard above for the JSON-log paths).
    if let Some(port) = args.metrics_port
        && (args.transport == "stdio" || matches!(output_format, OutputFormat::Json))
    {
        log::info!(
            "Prometheus metrics exporter enabled on http://127.0.0.1:{}/metrics",
            port
        );
    }

    // Create vault-agnostic server instance (no vault required at startup)
    let server =
        ObsidianMcpServer::new().map_err(|e| format!("Failed to create MCP server: {}", e))?;

    log::info!("MCP Server created (vault-agnostic mode)");

    // Initialize persistent cache in the server
    if let Err(e) = server.init_cache().await {
        log::warn!(
            "Failed to initialize server cache: {}. Cache persistence will be unavailable.",
            e
        );
    }

    // ---- Vault registration precedence (turbovault-xj8) ----
    // 1. `--config <path>` vaults are canonical. Parse errors are LOUD (return Err,
    //    nonzero exit) — never silently fall through to cache recovery.
    // 2. Cache-recovered vaults fill in only names not already registered from
    //    `--config`.
    // 3. `--vault <path>` CLI arg is shorthand for a single "default" vault and
    //    is added only if the name "default" is still free after (1) and (2).
    // 4. Active vault: a `is_default: true` entry in `--config` wins. Otherwise
    //    cache metadata's `active_vault` is honored.
    //
    // The vault block lives under `vaults:` in the same TurboVault YAML file
    // that `tool_visibility:` is read from (single canonical shape — both
    // consumers go through `TurboVaultConfigFile`). xj8-followon / wbk.
    let mut config_vault_names: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut config_default_vault: Option<String> = None;
    if let Some(config_path) = args.config.as_deref() {
        let parsed = TurboVaultConfigFile::load(config_path)
            .await
            .map_err(|e| format!("Failed to load --config {}: {}", config_path.display(), e))?;
        log::info!(
            "Loaded {} vault(s) from --config {}",
            parsed.vaults.len(),
            config_path.display()
        );
        for vault_config in parsed.vaults {
            let name = vault_config.name.clone();
            let is_default = vault_config.is_default;
            match server.multi_vault().add_vault(vault_config).await {
                Ok(_) => {
                    log::info!("Registered vault from --config: '{}'", name);
                    if is_default && config_default_vault.is_none() {
                        config_default_vault = Some(name.clone());
                    }
                    config_vault_names.insert(name);
                }
                Err(e) => {
                    return Err(format!(
                        "Failed to register vault '{}' from --config: {}",
                        name, e
                    )
                    .into());
                }
            }
        }
        if let Some(active) = config_default_vault.as_deref() {
            if let Err(e) = server.multi_vault().set_active_vault(active).await {
                log::warn!(
                    "Failed to set active vault from --config is_default '{}': {}",
                    active,
                    e
                );
            } else {
                log::info!("Active vault set from --config is_default: '{}'", active);
            }
        }
    }

    // CACHE RECOVERY: Load previously registered vaults for this project
    match VaultCache::init().await {
        Ok(cache) => {
            log::info!(
                "Project cache initialized: {} | Cache dir: {}",
                cache.project_id(),
                cache.project_cache_dir().display()
            );

            // Load cached vaults
            let cached_vaults = cache.load_vaults().await.unwrap_or_else(|e| {
                log::warn!("Failed to load cached vaults: {}", e);
                vec![]
            });

            if !cached_vaults.is_empty() {
                log::info!(
                    "Recovering {} cached vaults for project {}",
                    cached_vaults.len(),
                    cache.project_id()
                );

                // Add each cached vault to the multi-vault manager
                for vault_config in cached_vaults {
                    if config_vault_names.contains(&vault_config.name) {
                        log::info!(
                            "Skipping cached vault '{}' — name already registered from --config",
                            vault_config.name
                        );
                        continue;
                    }
                    match server.multi_vault().add_vault(vault_config.clone()).await {
                        Ok(_) => {
                            log::info!(
                                "Restored vault from cache: '{}' -> {}",
                                vault_config.name,
                                vault_config.path.display()
                            );
                        }
                        Err(e) => {
                            log::warn!(
                                "Failed to restore vault '{}': {}. Skipping.",
                                vault_config.name,
                                e
                            );
                        }
                    }
                }

                // Restore active vault
                let metadata = cache.load_metadata().await.unwrap_or_else(|e| {
                    log::warn!("Failed to load cache metadata: {}", e);
                    turbovault_core::cache::CacheMetadata {
                        active_vault: String::new(),
                        last_updated: 0,
                        version: 1,
                        project_id: cache.project_id().to_string(),
                        working_dir: cache.working_dir().to_string_lossy().to_string(),
                    }
                });

                if !metadata.active_vault.is_empty() && config_default_vault.is_none() {
                    match server
                        .multi_vault()
                        .set_active_vault(&metadata.active_vault)
                        .await
                    {
                        Ok(_) => {
                            log::info!(
                                "Restored active vault from cache: '{}'",
                                metadata.active_vault
                            );
                        }
                        Err(e) => {
                            log::warn!(
                                "Failed to restore active vault '{}': {}",
                                metadata.active_vault,
                                e
                            );
                        }
                    }
                }
            } else {
                log::info!("No cached vaults found for this project");
            }
        }
        Err(e) => {
            log::warn!(
                "Failed to initialize cache: {}. Continuing without cache recovery.",
                e
            );
        }
    }

    // Optionally add a vault at startup (for convenience)
    if let Some(vault_path) = args.vault {
        // Expand tilde and environment variables in the path
        let vault_path = PathBuf::from(
            shellexpand::full(&vault_path.to_string_lossy())
                .map_err(|e| format!("Failed to expand vault path: {}", e))?
                .into_owned(),
        );
        log::info!("Adding vault from CLI argument: {:?}", vault_path);

        // Check if a vault named "default" already exists (e.g., from cache recovery)
        let vault_exists = server.multi_vault().vault_exists("default").await;

        if vault_exists {
            // Vault already exists - check if it's the same path
            match server.multi_vault().get_vault_config("default").await {
                Ok(existing_config) => {
                    // Canonicalize paths for comparison (handles symlinks, relative paths, etc.)
                    let existing_canonical = existing_config.path.canonicalize().ok();
                    let new_canonical = vault_path.canonicalize().ok();

                    if existing_canonical == new_canonical {
                        log::info!(
                            "Vault 'default' already registered from cache with same path. Skipping CLI vault addition."
                        );
                    } else {
                        log::warn!(
                            "Vault 'default' already exists with different path. Cached: {:?}, CLI: {:?}. Using cached vault.",
                            existing_config.path,
                            vault_path
                        );
                    }
                }
                Err(e) => {
                    log::warn!(
                        "Could not verify existing vault config: {}. Skipping CLI vault addition.",
                        e
                    );
                }
            }
        } else {
            // No existing vault named "default" - add it
            let vault_config = VaultConfig::builder("default", &vault_path)
                .build()
                .map_err(|e| format!("Failed to create vault config: {}", e))?;

            server
                .multi_vault()
                .add_vault(vault_config)
                .await
                .map_err(|e| format!("Failed to add vault: {}", e))?;

            log::info!("Vault registered: default -> {:?}", vault_path);
        }

        // (--init handling moved outside this CLI-vault block so it
        // covers every registered vault, not only the one passed via
        // --vault. See below.)
    } else {
        log::info!("No vault path provided. Use add_vault MCP tool to register a vault.");
        log::info!("Available tools: add_vault, list_vaults, set_active_vault");
    }

    // Start server with multi-version protocol support.
    // Accepts both MCP 2025-06-18 and 2025-11-25 clients.
    // turbovault-z5c: honor `--init` by walking every registered vault
    // (cache-recovered, --config, AND --vault forms) and running the
    // VaultManager scan + link-graph build. Without this flag, vaults
    // are lazy-initialized on first use; the flag is for operators
    // who want the build cost paid up-front (faster first query).
    if args.init {
        log::info!("--init: initializing registered vaults up-front...");
        if let Err(e) = server.initialize_registered_vaults().await {
            log::warn!("--init failed: {} — continuing with lazy init", e);
        }
    }

    log::info!("Starting TurboVault Server (multi-version MCP protocol)");
    if tool_visibility.has_rules() {
        log::info!(
            "Tool visibility configured: allowed={} hidden={} disabled={} require_read_only={}",
            tool_visibility.allowed.len(),
            tool_visibility.hidden.len(),
            tool_visibility.disabled.len(),
            tool_visibility.require_read_only
        );
    }

    // turbovault-84k: scan every git-backend vault for orphan `wip-*`
    // worktrees from prior sessions. Log a warning per detection so the
    // operator knows about residue; list_orphan_fanouts MCP tool can
    // enumerate at runtime.
    server.log_orphan_fanouts_warnings().await;

    // turbovault-84k: clone the server BEFORE the VisibilityLayer wrap so
    // the shutdown handler has access to `active_fanouts` for best-effort
    // abandon. ObsidianMcpServer is #[derive(Clone)] — all fields are
    // already Arc-wrapped, so cloning is cheap.
    let shutdown_handle = server.clone();

    let server = VisibilityLayer::new(server)
        .with_visibility_config(tool_visibility.into_visibility_config());

    match args.transport.as_str() {
        "stdio" => {
            log::info!("Running in STDIO mode for MCP protocol");
            let serve_fut = server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .serve();
            tokio::select! {
                res = serve_fut => res?,
                _ = shutdown_signal() => {
                    log::info!("Shutdown signal received; abandoning active fanouts...");
                    shutdown_handle.shutdown_fanouts_best_effort().await;
                }
            }
        }
        #[cfg(feature = "http")]
        "http" => {
            let addr = format!("127.0.0.1:{}", args.port);
            log::info!("Running HTTP server on {}", addr);
            log::info!("Output format: {:?}", output_format);
            let serve_fut = server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .transport(turbomcp::Transport::http(&addr))
                .serve();
            tokio::select! {
                res = serve_fut => res?,
                _ = shutdown_signal() => {
                    log::info!("Shutdown signal received; abandoning active fanouts...");
                    shutdown_handle.shutdown_fanouts_best_effort().await;
                }
            }
        }
        #[cfg(feature = "websocket")]
        "websocket" => {
            let addr = format!("127.0.0.1:{}", args.port);
            log::info!("Running WebSocket server on {}", addr);
            log::info!("Output format: {:?}", output_format);
            let serve_fut = server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .transport(turbomcp::Transport::websocket(&addr))
                .serve();
            tokio::select! {
                res = serve_fut => res?,
                _ = shutdown_signal() => {
                    log::info!("Shutdown signal received; abandoning active fanouts...");
                    shutdown_handle.shutdown_fanouts_best_effort().await;
                }
            }
        }
        #[cfg(feature = "tcp")]
        "tcp" => {
            let addr = format!("127.0.0.1:{}", args.port);
            log::info!("Running TCP server on {}", addr);
            log::info!("Output format: {:?}", output_format);
            let serve_fut = server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .transport(turbomcp::Transport::tcp(&addr))
                .serve();
            tokio::select! {
                res = serve_fut => res?,
                _ = shutdown_signal() => {
                    log::info!("Shutdown signal received; abandoning active fanouts...");
                    shutdown_handle.shutdown_fanouts_best_effort().await;
                }
            }
        }
        #[cfg(feature = "unix")]
        "unix" => {
            let socket_path = "/tmp/turbovault.sock".to_string();
            log::info!("Running Unix socket server on {}", socket_path);
            log::info!("Output format: {:?}", output_format);
            let serve_fut = server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .transport(turbomcp::Transport::unix(&socket_path))
                .serve();
            tokio::select! {
                res = serve_fut => res?,
                _ = shutdown_signal() => {
                    log::info!("Shutdown signal received; abandoning active fanouts...");
                    shutdown_handle.shutdown_fanouts_best_effort().await;
                }
            }
        }
        transport => {
            #[cfg(not(feature = "http"))]
            if transport == "http" {
                return Err("HTTP transport not enabled. Rebuild with --features http".into());
            }
            #[cfg(not(feature = "websocket"))]
            if transport == "websocket" {
                return Err(
                    "WebSocket transport not enabled. Rebuild with --features websocket".into(),
                );
            }
            #[cfg(not(feature = "tcp"))]
            if transport == "tcp" {
                return Err("TCP transport not enabled. Rebuild with --features tcp".into());
            }
            #[cfg(not(feature = "unix"))]
            if transport == "unix" {
                return Err(
                    "Unix socket transport not enabled. Rebuild with --features unix".into(),
                );
            }
            return Err(format!(
                "Unknown transport '{}'. Valid options: stdio{}{}{}{}",
                transport,
                if cfg!(feature = "http") { ", http" } else { "" },
                if cfg!(feature = "websocket") {
                    ", websocket"
                } else {
                    ""
                },
                if cfg!(feature = "tcp") { ", tcp" } else { "" },
                if cfg!(feature = "unix") { ", unix" } else { "" },
            )
            .into());
        }
    }

    Ok(())
}

/// turbovault-84k: wait for SIGINT (Ctrl-C) or SIGTERM (kill / process
/// supervisor stop). On non-unix platforms only Ctrl-C is honored.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                log::warn!("Failed to install SIGTERM handler: {}", e);
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => log::info!("Received SIGINT"),
            _ = term.recv() => log::info!("Received SIGTERM"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        log::info!("Received Ctrl+C");
    }
}

/// turbovault-bna: build the observability `TelemetryConfig`, optionally
/// enabling the Prometheus metrics exporter (binds `127.0.0.1:<port>/metrics`)
/// when `metrics_port` is `Some`. The exporter installs a `metrics`-crate
/// recorder; the substrate emits `turbovault_apply_transaction_*` at the
/// GitFileTools chokepoint. `init()` fails loudly if the port can't bind.
fn telemetry_config(
    profile: &str,
    stderr_output: bool,
    metrics_port: Option<u16>,
) -> TelemetryConfig {
    let mut builder = TelemetryConfig::builder()
        .service_name("turbovault")
        .service_version(env!("CARGO_PKG_VERSION"))
        .log_level(resolve_log_level(profile))
        .json_logs(true)
        .stderr_output(stderr_output);
    if let Some(port) = metrics_port {
        builder = builder
            .prometheus_port(port)
            .prometheus_bind_addr(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    }
    builder.build()
}

/// turbovault-foy: resolve the log-level filter string. Priority:
///   1. `RUST_LOG` env var (env_logger-compatible filter, e.g.
///      "warn,turbo_vault=trace").
///   2. `--profile production` default: `info,turbo_vault=debug`.
///   3. `--profile development` default (anything else): `debug`.
///
/// The README has advertised `RUST_LOG` since at least the first
/// public release; this lifts it from claim to truth.
fn resolve_log_level(profile: &str) -> String {
    if let Ok(env_value) = std::env::var("RUST_LOG")
        && !env_value.is_empty()
    {
        return env_value;
    }
    if profile == "production" {
        "info,turbo_vault=debug".to_string()
    } else {
        "debug".to_string()
    }
}

/// turbovault-foy: distill the resolved filter string into a single
/// `LevelFilter` for `SimpleLogger` (which doesn't accept env_logger
/// syntax). Walks the comma-separated components, ignores anything
/// containing `=` (module overrides), and parses the first bare token.
/// Defaults to `Info` if nothing parses.
fn parse_simplelogger_level(profile: &str) -> log::LevelFilter {
    let filter = resolve_log_level(profile);
    for part in filter.split(',') {
        let token = part.trim();
        if token.is_empty() || token.contains('=') {
            continue;
        }
        if let Ok(lvl) = token.parse::<log::LevelFilter>() {
            return lvl;
        }
    }
    log::LevelFilter::Info
}

async fn load_tool_visibility(
    args: &Args,
) -> Result<ToolVisibilitySettings, Box<dyn std::error::Error>> {
    let mut settings = match args.config.as_deref() {
        Some(path) => ToolVisibilitySettings::from_yaml_file(path).await?,
        None => match default_config_path().filter(|path| path.exists()) {
            Some(path) => ToolVisibilitySettings::from_yaml_file(path).await?,
            None => ToolVisibilitySettings::default(),
        },
    };

    settings.merge_cli(ToolVisibilityOverrides {
        allowed: args.allowed_tools.clone(),
        hidden: args.hidden_tools.clone(),
        disabled: args.disabled_tools.clone(),
        require_read_only: args.require_read_only_tools,
    });

    Ok(settings)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use turbovault_core::{MultiVaultManager, ServerConfig, WriteBackend};

    /// turbovault-xj8 (refined by xj8-followon): `--config` loads the
    /// `vaults:` block from the unified TurboVault YAML config — same file
    /// that holds `tool_visibility:`. Honors per-vault `write_backend: git`.
    ///
    /// Fixture matches `TurboVaultConfigFile`'s canonical shape (keyed
    /// `vaults:` section). Fields named here exercise the parse path only;
    /// behavioral coverage for each field belongs to its own implementing
    /// ticket. (turbovault-lri: `include_ignored` is parse-only today, so
    /// it is intentionally omitted from this fixture.)
    #[tokio::test]
    async fn config_load_registers_git_backend_vault() {
        let vault_tmp = TempDir::new().unwrap();
        let cfg_tmp = TempDir::new().unwrap();
        let cfg_path = cfg_tmp.path().join("config.yaml");

        let yaml = format!(
            "vaults:\n  - name: gvault\n    path: {}\n    is_default: true\n    write_backend: git\n    git:\n      branch: main\n      merge_strategy: fast-forward\n",
            vault_tmp.path().display()
        );
        tokio::fs::write(&cfg_path, yaml).await.unwrap();

        let parsed = TurboVaultConfigFile::load(&cfg_path).await.unwrap();
        assert_eq!(parsed.vaults.len(), 1);
        assert_eq!(parsed.vaults[0].name, "gvault");
        assert_eq!(parsed.vaults[0].write_backend, WriteBackend::Git);
        assert!(parsed.vaults[0].is_default);
        assert!(parsed.vaults[0].git.is_some());

        let mv = MultiVaultManager::empty(ServerConfig::default()).unwrap();
        for v in parsed.vaults {
            mv.add_vault(v).await.unwrap();
        }
        let registered = mv.get_vault_config("gvault").await.unwrap();
        assert_eq!(registered.write_backend, WriteBackend::Git);
    }

    /// `tool_visibility:` AND `vaults:` MUST load from the same file in one
    /// parse — that's the canonical shape both consumers use. Regression
    /// guard for the xj8-followon two-parser collision.
    #[tokio::test]
    async fn config_load_handles_unified_tool_visibility_and_vaults() {
        let vault_tmp = TempDir::new().unwrap();
        let cfg_tmp = TempDir::new().unwrap();
        let cfg_path = cfg_tmp.path().join("config.yaml");

        let yaml = format!(
            "tool_visibility:\n  allowed:\n    - read_note\n  require_read_only: true\nvaults:\n  - name: v\n    path: {}\n    is_default: true\n",
            vault_tmp.path().display()
        );
        tokio::fs::write(&cfg_path, yaml).await.unwrap();

        let visibility = ToolVisibilitySettings::from_yaml_file(&cfg_path)
            .await
            .unwrap();
        assert_eq!(visibility.allowed, vec!["read_note".to_string()]);
        assert!(visibility.require_read_only);

        let parsed = TurboVaultConfigFile::load(&cfg_path).await.unwrap();
        assert_eq!(parsed.vaults.len(), 1);
        assert_eq!(parsed.vaults[0].name, "v");
    }

    /// Parse failure on `--config` must propagate (loud failure contract).
    #[tokio::test]
    async fn config_load_parse_error_is_loud() {
        let cfg_tmp = TempDir::new().unwrap();
        let cfg_path = cfg_tmp.path().join("bad.yaml");
        tokio::fs::write(&cfg_path, "not: a: valid: vault: array\n")
            .await
            .unwrap();

        let res = TurboVaultConfigFile::load(&cfg_path).await;
        assert!(res.is_err(), "malformed yaml must surface as Err");
    }
}
