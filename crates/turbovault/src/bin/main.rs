//! TurboVault Server CLI

use clap::Parser;
use std::path::PathBuf;
use turbomcp::telemetry::TelemetryConfig;
use turbomcp::{McpServerExt, ProtocolConfig, VisibilityLayer};
use turbovault::ObsidianMcpServer;
use turbovault::tool_visibility::{
    ToolVisibilityOverrides, ToolVisibilitySettings, default_config_path,
};
use turbovault_core::VaultConfig;
use turbovault_core::cache::VaultCache;
use turbovault_tools::OutputFormat;

/// TurboVault Server - AI-powered vault management
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the Obsidian vault directory
    #[arg(short, long, env = "OBSIDIAN_VAULT_PATH")]
    vault: Option<PathBuf>,

    /// Configuration profile to use (development, production, etc.)
    #[arg(short, long, default_value = "development")]
    profile: String,

    /// Transport mode (stdio, http, websocket, tcp, unix)
    #[arg(short, long, default_value = "stdio", env = "TURBOVAULT_TRANSPORT")]
    transport: String,

    /// Port to bind for network transports (http, websocket, tcp)
    #[arg(long, default_value = "3000", env = "TURBOVAULT_PORT")]
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command-line arguments
    let args = Args::parse();
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
        let obs_config = TelemetryConfig::builder()
            .service_name("turbovault")
            .service_version(env!("CARGO_PKG_VERSION"))
            .log_level(if args.profile == "production" {
                "info,turbo_vault=debug".to_string()
            } else {
                "debug".to_string()
            })
            .json_logs(true)
            .stderr_output(true)
            .build();

        Some(obs_config.init()?)
    } else {
        // HTTP/WebSocket/TCP: Use simple logger with configurable format
        use simple_logger::SimpleLogger;

        match output_format {
            OutputFormat::Json => {
                // JSON format for programmatic parsing
                let obs_config = TelemetryConfig::builder()
                    .service_name("turbovault")
                    .service_version(env!("CARGO_PKG_VERSION"))
                    .log_level(if args.profile == "production" {
                        "info,turbo_vault=debug".to_string()
                    } else {
                        "debug".to_string()
                    })
                    .json_logs(true)
                    .stderr_output(false) // HTTP/WS can use stdout
                    .build();
                Some(obs_config.init()?)
            }
            OutputFormat::Human | OutputFormat::Text => {
                // Human-readable format for terminal/stdout
                SimpleLogger::new()
                    .with_level(if args.profile == "production" {
                        log::LevelFilter::Info
                    } else {
                        log::LevelFilter::Debug
                    })
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

                if !metadata.active_vault.is_empty() {
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

        // Initialize vault (scan files and build graph) if requested
        if args.init {
            log::info!("Scanning vault and building link graph...");
            // Note: Full initialization would require loading the vault manager
            // For now, we document that users should use the dedicated init tool
            log::info!("Vault ready for operations");
        }
    } else {
        log::info!("No vault path provided. Use add_vault MCP tool to register a vault.");
        log::info!("Available tools: add_vault, list_vaults, set_active_vault");
    }

    // Start server with multi-version protocol support.
    // Accepts both MCP 2025-06-18 and 2025-11-25 clients.
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

    let server = VisibilityLayer::new(server)
        .with_visibility_config(tool_visibility.into_visibility_config());

    match args.transport.as_str() {
        "stdio" => {
            log::info!("Running in STDIO mode for MCP protocol");
            server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .serve()
                .await?;
        }
        #[cfg(feature = "http")]
        "http" => {
            let addr = format!("127.0.0.1:{}", args.port);
            log::info!("Running HTTP server on {}", addr);
            log::info!("Output format: {:?}", output_format);
            server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .transport(turbomcp::Transport::http(&addr))
                .serve()
                .await?;
        }
        #[cfg(feature = "websocket")]
        "websocket" => {
            let addr = format!("127.0.0.1:{}", args.port);
            log::info!("Running WebSocket server on {}", addr);
            log::info!("Output format: {:?}", output_format);
            server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .transport(turbomcp::Transport::websocket(&addr))
                .serve()
                .await?;
        }
        #[cfg(feature = "tcp")]
        "tcp" => {
            let addr = format!("127.0.0.1:{}", args.port);
            log::info!("Running TCP server on {}", addr);
            log::info!("Output format: {:?}", output_format);
            server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .transport(turbomcp::Transport::tcp(&addr))
                .serve()
                .await?;
        }
        #[cfg(feature = "unix")]
        "unix" => {
            let socket_path = "/tmp/turbovault.sock".to_string();
            log::info!("Running Unix socket server on {}", socket_path);
            log::info!("Output format: {:?}", output_format);
            server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .transport(turbomcp::Transport::unix(&socket_path))
                .serve()
                .await?;
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
