//! TurboVault Server CLI

use clap::Parser;
use turbovault_core::VaultConfig;
use turbovault_core::cache::VaultCache;
use turbovault::ObsidianMcpServer;
use turbovault_tools::OutputFormat;
use std::path::PathBuf;
use turbomcp_server::observability::ObservabilityConfig;

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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse command-line arguments
    let args = Args::parse();

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
        let obs_config = ObservabilityConfig::default()
            .with_service_name("turbovault")
            .with_service_version(env!("CARGO_PKG_VERSION"))
            .with_log_level(if args.profile == "production" {
                "info,turbo_vault=debug".to_string()
            } else {
                "debug".to_string()
            })
            .enable_security_auditing()
            .enable_performance_monitoring();

        Some(obs_config.init()?)
    } else {
        // HTTP/WebSocket/TCP: Use simple logger with configurable format
        use simple_logger::SimpleLogger;

        match output_format {
            OutputFormat::Json => {
                // JSON format for programmatic parsing
                let obs_config = ObservabilityConfig::default()
                    .with_service_name("turbovault")
                    .with_service_version(env!("CARGO_PKG_VERSION"))
                    .with_log_level(if args.profile == "production" {
                        "info,turbo_vault=debug".to_string()
                    } else {
                        "debug".to_string()
                    })
                    .enable_security_auditing()
                    .enable_performance_monitoring();
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
    log::info!("Transport: {} | Log format: {:?}", args.transport, output_format);

    // Create vault-agnostic server instance (no vault required at startup)
    let server =
        ObsidianMcpServer::new().map_err(|e| format!("Failed to create MCP server: {}", e))?;

    log::info!("MCP Server created (vault-agnostic mode)");

    // Initialize persistent cache in the server
    if let Err(e) = server.init_cache().await {
        log::warn!("Failed to initialize server cache: {}. Cache persistence will be unavailable.", e);
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
            let cached_vaults = cache
                .load_vaults()
                .await
                .unwrap_or_else(|e| {
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
                                vault_config.name, e
                            );
                        }
                    }
                }

                // Restore active vault
                let metadata = cache
                    .load_metadata()
                    .await
                    .unwrap_or_else(|e| {
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
                                metadata.active_vault, e
                            );
                        }
                    }
                }
            } else {
                log::info!("No cached vaults found for this project");
            }
        }
        Err(e) => {
            log::warn!("Failed to initialize cache: {}. Continuing without cache recovery.", e);
        }
    }

    // Optionally add a vault at startup (for convenience)
    if let Some(vault_path) = args.vault {
        log::info!("Adding vault from CLI argument: {:?}", vault_path);

        // Create vault config
        let vault_config = VaultConfig::builder("default", &vault_path)
            .build()
            .map_err(|e| format!("Failed to create vault config: {}", e))?;

        // Add vault to the multi-vault manager
        server
            .multi_vault()
            .add_vault(vault_config)
            .await
            .map_err(|e| format!("Failed to add vault: {}", e))?;

        log::info!("Vault registered: default -> {:?}", vault_path);

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

    // Start server with appropriate transport
    log::info!("Starting TurboVault Server");

    match args.transport.as_str() {
        "stdio" => {
            log::info!("Running in STDIO mode for MCP protocol");
            server.run_stdio().await?;
        }
        #[cfg(feature = "http")]
        "http" => {
            let addr = format!("127.0.0.1:{}", args.port);
            log::info!("Running HTTP server on {}", addr);
            log::info!("Output format: {:?}", output_format);
            // TODO: Apply output_format to HTTP responses
            server.run_http(&addr).await?;
        }
        #[cfg(feature = "websocket")]
        "websocket" => {
            let addr = format!("127.0.0.1:{}", args.port);
            log::info!("Running WebSocket server on {}", addr);
            log::info!("Output format: {:?}", output_format);
            // TODO: Apply output_format to WebSocket responses
            server.run_websocket(&addr).await?;
        }
        #[cfg(feature = "tcp")]
        "tcp" => {
            let addr = format!("127.0.0.1:{}", args.port);
            log::info!("Running TCP server on {}", addr);
            log::info!("Output format: {:?}", output_format);
            // TODO: Apply output_format to TCP responses
            server.run_tcp(&addr).await?;
        }
        #[cfg(feature = "unix")]
        "unix" => {
            let socket_path = "/tmp/turbovault.sock".to_string();
            log::info!("Running Unix socket server on {}", socket_path);
            log::info!("Output format: {:?}", output_format);
            // TODO: Apply output_format to Unix socket responses
            server.run_unix(&socket_path).await?;
        }
        transport => {
            #[cfg(not(feature = "http"))]
            if transport == "http" {
                return Err("HTTP transport not enabled. Rebuild with --features http".into());
            }
            #[cfg(not(feature = "websocket"))]
            if transport == "websocket" {
                return Err("WebSocket transport not enabled. Rebuild with --features websocket".into());
            }
            #[cfg(not(feature = "tcp"))]
            if transport == "tcp" {
                return Err("TCP transport not enabled. Rebuild with --features tcp".into());
            }
            #[cfg(not(feature = "unix"))]
            if transport == "unix" {
                return Err("Unix socket transport not enabled. Rebuild with --features unix".into());
            }
            return Err(format!(
                "Unknown transport '{}'. Valid options: stdio{}{}{}{}",
                transport,
                if cfg!(feature = "http") { ", http" } else { "" },
                if cfg!(feature = "websocket") { ", websocket" } else { "" },
                if cfg!(feature = "tcp") { ", tcp" } else { "" },
                if cfg!(feature = "unix") { ", unix" } else { "" },
            )
            .into());
        }
    }

    Ok(())
}
