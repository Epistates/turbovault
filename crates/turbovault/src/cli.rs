//! TurboVault command-line parsing and runtime orchestration.

use crate::ObsidianMcpServer;
use crate::tool_visibility::{
    ToolVisibilityOverrides, ToolVisibilitySettings, TurboVaultConfigFile, default_config_path,
};
use clap::Parser;
use std::path::PathBuf;
use turbomcp::telemetry::TelemetryConfig;
use turbomcp::{McpServerExt, ProtocolConfig, VisibilityLayer};
use turbovault_core::cache::VaultCache;
use turbovault_core::config::{VaultGitConfig, WriteBackend};
use turbovault_tools::{OutputFormat, VaultLifecycleTools, direct_over_git_repo_warning};

/// TurboVault Server - AI-powered vault management
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path to the Obsidian vault directory
    #[arg(short, long, env = "OBSIDIAN_VAULT_PATH")]
    vault: Option<PathBuf>,

    /// Write backend for the --vault shorthand: `direct` (default) or `git`
    /// (turbovault-kdq). Vaults registered from --config carry their own
    /// `write_backend` key and ignore this.
    #[arg(long, env = "TURBOVAULT_VAULT_WRITE_BACKEND")]
    vault_write_backend: Option<WriteBackend>,

    /// Settings for the --vault shorthand's selected write backend, as JSON
    /// (e.g. '{"branch":"main","require_commit_message":true}'). Only the git
    /// backend has any, so this requires --vault-write-backend git.
    #[arg(long, env = "TURBOVAULT_VAULT_BACKEND_OPTS")]
    vault_backend_opts: Option<String>,

    /// Configuration profile to use (development, production, etc.)
    #[arg(short, long, default_value = "development", env = "TURBOVAULT_PROFILE")]
    profile: String,

    /// Transport mode (stdio, http, websocket, tcp, unix)
    #[arg(short, long, default_value = "stdio", env = "TURBOVAULT_TRANSPORT")]
    transport: String,

    /// Host/interface to bind for network transports (http, websocket, tcp)
    #[arg(long, default_value = "127.0.0.1", env = "TURBOVAULT_HOST")]
    host: String,

    /// Port to bind for network transports (http, websocket, tcp)
    #[arg(long, default_value = "3000", env = "TURBOVAULT_PORT")]
    port: u16,

    /// Socket path for the unix transport
    #[arg(
        long,
        default_value = "/tmp/turbovault.sock",
        env = "TURBOVAULT_SOCKET_PATH"
    )]
    socket_path: String,

    /// Output format for non-STDIO transports (json, human, text)
    /// Note: STDIO transport always uses JSON per MCP protocol specification
    #[arg(long, default_value = "json", env = "TURBOVAULT_OUTPUT_FORMAT")]
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

    /// Comma-separated tags — all tools carrying any of these tags are disabled.
    #[arg(long, value_delimiter = ',', env = "TURBOVAULT_DISABLED_TAGS")]
    disabled_tags: Vec<String>,

    /// Comma-separated tags — hidden from tools/list but callable by name.
    /// NOTE: no-op until turbomcp hide_tags() is available; a warning is logged at startup.
    #[arg(long, value_delimiter = ',', env = "TURBOVAULT_HIDDEN_TAGS")]
    hidden_tags: Vec<String>,

    /// Hide all tools not annotated as read-only by TurboMCP.
    #[arg(
        long,
        env = "TURBOVAULT_REQUIRE_READ_ONLY_TOOLS",
        action = clap::ArgAction::SetTrue
    )]
    require_read_only_tools: bool,
}

/// Parse process arguments and run the CLI.
pub async fn run_from_env() -> Result<(), Box<dyn std::error::Error>> {
    run(Args::parse()).await
}

/// Run TurboVault using already-parsed CLI arguments.
pub async fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let tool_visibility = load_tool_visibility(&args).await?;

    // Validate output format (unless STDIO transport, which always uses JSON)
    let output_format = resolve_output_format(&args)?;
    validate_transport(&args.transport)?;

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
                    // turbovault-74p: a cached Direct vault over a git repo is
                    // the same bypass, restored — warn on every recovery too.
                    if let Some(warning) = direct_over_git_repo_warning(&vault_config) {
                        log::warn!("{warning}");
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

    register_configured_vaults(&server, &args).await?;

    // Optionally add a vault at startup (for convenience)
    if let Some(vault_path) = args.vault {
        // Expand tilde and environment variables in the path
        let vault_path = expand_vault_path(&vault_path)?;
        let write_backend = args.vault_write_backend.unwrap_or_default();
        let backend_opts = args
            .vault_backend_opts
            .as_deref()
            .map(serde_json::from_str::<VaultGitConfig>)
            .transpose()
            .map_err(|error| format!("Invalid --vault-backend-opts JSON: {error}"))?;
        log::info!("Adding vault from CLI argument: {:?}", vault_path);
        register_default_vault(&server, &vault_path, write_backend, backend_opts).await?;
    } else {
        log::info!("No vault path provided. Use add_vault MCP tool to register a vault.");
        log::info!("Available tools: add_vault, list_vaults, set_active_vault");
    }

    if args.init {
        log::info!("Scanning registered vaults and building derived indexes...");
        server.initialize_registered_vaults().await?;
    }
    server.log_orphan_fanouts_warnings().await;
    // After vault registration so a plugin's first reconciliation pass sees
    // the vaults it is meant to index, and before serving so no tool call
    // arrives at a plugin whose worker has not started.
    server.start_plugins().await?;

    // Cleanup must run on BOTH exit paths: a termination signal (the common
    // case for network transports and for a client killing the process) and a
    // normal `serve()` return (stdio, when the client closes stdin). The signal
    // task owns its own clone because `server` is consumed by the visibility
    // layer below.
    spawn_shutdown_signal_handler(server.clone());
    let shutdown_handle = server.clone();

    // Start server with multi-version protocol support.
    // Accepts both MCP 2025-06-18 and 2025-11-25 clients.
    log::info!("Starting TurboVault Server (multi-version MCP protocol)");
    if tool_visibility.has_rules() {
        log::info!(
            "Tool visibility configured: allowed={} hidden={} disabled={} \
             disabled_tags={} hidden_tags={} require_read_only={}",
            tool_visibility.allowed.len(),
            tool_visibility.hidden.len(),
            tool_visibility.disabled.len(),
            tool_visibility.disabled_tags.len(),
            tool_visibility.hidden_tags.len(),
            tool_visibility.require_read_only
        );
    }

    let server = tool_visibility.apply_to_layer(VisibilityLayer::new(server));

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
            let addr = format!("{}:{}", args.host, args.port);
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
            let addr = format!("{}:{}", args.host, args.port);
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
            let addr = format!("{}:{}", args.host, args.port);
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
            log::info!("Running Unix socket server on {}", args.socket_path);
            log::info!("Output format: {:?}", output_format);
            server
                .builder()
                .with_protocol(ProtocolConfig::multi_version())
                .transport(turbomcp::Transport::unix(&args.socket_path))
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

    shutdown_handle.shutdown().await;
    Ok(())
}

/// Run [`ObsidianMcpServer::shutdown`] when the process is asked to terminate,
/// then exit. Without this, an interrupted session leaks every fanout worktree
/// it registered (the startup scan reports them as orphans on the next run) and
/// plugin hook subscribers never observe the bus closing.
fn spawn_shutdown_signal_handler(server: ObsidianMcpServer) {
    tokio::spawn(async move {
        wait_for_termination_signal().await;
        log::info!("Termination signal received; running graceful shutdown");
        server.shutdown().await;
        std::process::exit(0);
    });
}

#[cfg(unix)]
async fn wait_for_termination_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    // A failure to install a handler must not take the server down; fall back
    // to interrupt-only so the process still cleans up on Ctrl-C.
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            log::warn!("Could not install SIGTERM handler: {error}");
            let _ = tokio::signal::ctrl_c().await;
            return;
        }
    };
    tokio::select! {
        _ = terminate.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
    }
}

#[cfg(not(unix))]
async fn wait_for_termination_signal() {
    let _ = tokio::signal::ctrl_c().await;
}

async fn load_tool_visibility(
    args: &Args,
) -> Result<ToolVisibilitySettings, Box<dyn std::error::Error>> {
    let default_path = default_config_path();
    load_tool_visibility_with_default(args, default_path.as_deref()).await
}

async fn register_configured_vaults(
    server: &ObsidianMcpServer,
    args: &Args,
) -> Result<(), Box<dyn std::error::Error>> {
    let configured_path = args
        .config
        .clone()
        .or_else(|| default_config_path().filter(|path| path.exists()));
    let Some(path) = configured_path else {
        return Ok(());
    };

    let config = TurboVaultConfigFile::load(&path).await?;
    for mut vault in config.vaults {
        vault.path = expand_vault_path(&vault.path)?;
        if server.multi_vault().vault_exists(&vault.name).await {
            let existing = server.multi_vault().get_vault_config(&vault.name).await?;
            if paths_refer_to_same_vault(&existing.path, &vault.path) {
                continue;
            }
            log::info!(
                "Configured vault '{}' overrides cached path {}",
                vault.name,
                existing.path.display()
            );
            server.multi_vault().remove_vault(&vault.name).await?;
        }
        if vault.write_backend == turbovault_core::WriteBackend::Git
            && !turbovault_tools::VaultRepo::is_git_repo(&vault.path)
        {
            return Err(format!(
                "vault '{}' selects write_backend=git but {} is not a Git repository",
                vault.name,
                vault.path.display()
            )
            .into());
        }
        // turbovault-74p: the inverse mismatch — direct over a git repo — is a
        // silent bypass rather than an error, so it warns and registers.
        if let Some(warning) = direct_over_git_repo_warning(&vault) {
            log::warn!("{warning}");
        }
        log::info!(
            "Registering configured vault '{}' ({:?}) with write_backend={:?}",
            vault.name,
            vault.path,
            vault.write_backend
        );
        server.multi_vault().add_vault(vault).await?;
    }
    Ok(())
}

async fn load_tool_visibility_with_default(
    args: &Args,
    default_path: Option<&std::path::Path>,
) -> Result<ToolVisibilitySettings, Box<dyn std::error::Error>> {
    let mut settings = match args.config.as_deref() {
        Some(path) => ToolVisibilitySettings::from_yaml_file(path).await?,
        None => match default_path.filter(|path| path.exists()) {
            Some(path) => ToolVisibilitySettings::from_yaml_file(path).await?,
            None => ToolVisibilitySettings::default(),
        },
    };

    settings.merge_cli(ToolVisibilityOverrides {
        allowed: args.allowed_tools.clone(),
        hidden: args.hidden_tools.clone(),
        disabled: args.disabled_tools.clone(),
        disabled_tags: args.disabled_tags.clone(),
        hidden_tags: args.hidden_tags.clone(),
        require_read_only: args.require_read_only_tools,
    });

    Ok(settings)
}

fn resolve_output_format(args: &Args) -> Result<OutputFormat, String> {
    if args.transport == "stdio" {
        Ok(OutputFormat::Json)
    } else {
        args.output_format.parse()
    }
}

fn validate_transport(transport: &str) -> Result<(), String> {
    match transport {
        "stdio" => Ok(()),
        "http" if cfg!(feature = "http") => Ok(()),
        "websocket" if cfg!(feature = "websocket") => Ok(()),
        "tcp" if cfg!(feature = "tcp") => Ok(()),
        "unix" if cfg!(feature = "unix") => Ok(()),
        "http" => Err("HTTP transport not enabled. Rebuild with --features http".to_string()),
        "websocket" => {
            Err("WebSocket transport not enabled. Rebuild with --features websocket".to_string())
        }
        "tcp" => Err("TCP transport not enabled. Rebuild with --features tcp".to_string()),
        "unix" => {
            Err("Unix socket transport not enabled. Rebuild with --features unix".to_string())
        }
        unknown => Err(format!(
            "Unknown transport '{}'. Valid options: stdio{}{}{}{}",
            unknown,
            if cfg!(feature = "http") { ", http" } else { "" },
            if cfg!(feature = "websocket") {
                ", websocket"
            } else {
                ""
            },
            if cfg!(feature = "tcp") { ", tcp" } else { "" },
            if cfg!(feature = "unix") { ", unix" } else { "" },
        )),
    }
}

fn paths_refer_to_same_vault(existing: &std::path::Path, requested: &std::path::Path) -> bool {
    match (existing.canonicalize(), requested.canonicalize()) {
        (Ok(existing), Ok(requested)) => existing == requested,
        _ => existing == requested,
    }
}

fn expand_vault_path(path: &std::path::Path) -> Result<PathBuf, String> {
    shellexpand::full(&path.to_string_lossy())
        .map(|expanded| PathBuf::from(expanded.into_owned()))
        .map_err(|error| format!("Failed to expand vault path: {error}"))
}

#[derive(Debug, PartialEq, Eq)]
enum DefaultVaultRegistration {
    Added,
    AlreadyRegistered,
    KeptCached { existing_path: PathBuf },
    CouldNotVerify,
}

async fn register_default_vault(
    server: &ObsidianMcpServer,
    vault_path: &std::path::Path,
    write_backend: WriteBackend,
    backend_opts: Option<VaultGitConfig>,
) -> Result<DefaultVaultRegistration, Box<dyn std::error::Error>> {
    if server.multi_vault().vault_exists("default").await {
        return match server.multi_vault().get_vault_config("default").await {
            Ok(existing_config) if paths_refer_to_same_vault(&existing_config.path, vault_path) => {
                log::info!(
                    "Vault 'default' already registered from cache with same path. Skipping CLI vault addition."
                );
                Ok(DefaultVaultRegistration::AlreadyRegistered)
            }
            Ok(existing_config) => {
                log::warn!(
                    "Vault 'default' already exists with different path. Cached: {:?}, CLI: {:?}. Using cached vault.",
                    existing_config.path,
                    vault_path
                );
                Ok(DefaultVaultRegistration::KeptCached {
                    existing_path: existing_config.path,
                })
            }
            Err(error) => {
                log::warn!(
                    "Could not verify existing vault config: {}. Skipping CLI vault addition.",
                    error
                );
                Ok(DefaultVaultRegistration::CouldNotVerify)
            }
        };
    }

    // Through the same funnel the MCP tools use, so the shorthand cannot drift
    // on the backend selection — including the turbovault-74p footgun warning,
    // which --vault over an existing git repo is the live case of, and the
    // refusal of backend options for a backend that has none.
    let vault_config =
        VaultLifecycleTools::build_config("default", vault_path, write_backend, backend_opts)
            .map_err(|error| format!("Failed to create vault config: {error}"))?;
    server
        .multi_vault()
        .add_vault(vault_config)
        .await
        .map_err(|error| format!("Failed to add vault: {error}"))?;
    log::info!("Vault registered: default -> {:?}", vault_path);

    Ok(DefaultVaultRegistration::Added)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(extra: &[&str]) -> Args {
        let mut argv = vec!["turbovault"];
        argv.extend_from_slice(extra);
        Args::try_parse_from(argv).expect("CLI arguments")
    }

    #[test]
    fn parses_network_and_visibility_arguments() {
        let args = args(&[
            "--transport",
            "http",
            "--host",
            "0.0.0.0",
            "--port",
            "4312",
            "--output-format",
            "human",
            "--allowed-tools",
            "read_note,search",
            "--hidden-tools",
            "audit_stats",
            "--disabled-tags",
            "write,delete",
            "--require-read-only-tools",
        ]);

        assert_eq!(args.transport, "http");
        assert_eq!(args.host, "0.0.0.0");
        assert_eq!(args.port, 4312);
        assert_eq!(args.output_format, "human");
        assert_eq!(args.allowed_tools, ["read_note", "search"]);
        assert_eq!(args.hidden_tools, ["audit_stats"]);
        assert_eq!(args.disabled_tags, ["write", "delete"]);
        assert!(args.require_read_only_tools);
    }

    /// The --vault shorthand's backend selection is a wire-visible name with no
    /// golden catalog behind it, so pin the spelling here.
    #[test]
    fn parses_the_vault_backend_selection_flags() {
        let args = args(&[
            "--vault-write-backend",
            "git",
            "--vault-backend-opts",
            r#"{"branch":"main"}"#,
        ]);

        assert_eq!(args.vault_write_backend, Some(WriteBackend::Git));
        assert_eq!(
            args.vault_backend_opts.as_deref(),
            Some(r#"{"branch":"main"}"#)
        );
    }

    #[test]
    fn stdio_always_resolves_to_json_output() {
        let args = args(&[
            "--transport",
            "stdio",
            "--output-format",
            "not-a-real-format",
        ]);
        assert_eq!(resolve_output_format(&args).unwrap(), OutputFormat::Json);
    }

    #[test]
    fn network_output_format_is_validated() {
        let human = args(&["--transport", "http", "--output-format", "human"]);
        assert_eq!(resolve_output_format(&human).unwrap(), OutputFormat::Human);

        let invalid = args(&[
            "--transport",
            "http",
            "--output-format",
            "not-a-real-format",
        ]);
        assert!(
            resolve_output_format(&invalid)
                .unwrap_err()
                .contains("Unknown output format")
        );
    }

    #[test]
    fn transport_validation_reflects_compiled_features() {
        assert!(validate_transport("stdio").is_ok());

        for (name, enabled) in [
            ("http", cfg!(feature = "http")),
            ("websocket", cfg!(feature = "websocket")),
            ("tcp", cfg!(feature = "tcp")),
            ("unix", cfg!(feature = "unix")),
        ] {
            assert_eq!(
                validate_transport(name).is_ok(),
                enabled,
                "transport {name}"
            );
        }

        let unknown = validate_transport("carrier-pigeon").unwrap_err();
        assert!(unknown.contains("Unknown transport 'carrier-pigeon'"));
        assert!(unknown.contains("stdio"));
    }

    #[tokio::test]
    async fn explicit_visibility_config_merges_cli_overrides() {
        let temp = tempfile::TempDir::new().unwrap();
        let config = temp.path().join("config.yaml");
        tokio::fs::write(
            &config,
            "tool_visibility:\n  hidden: [audit_log]\n  disabled_tags: [delete]\n",
        )
        .await
        .unwrap();

        let args = args(&[
            "--config",
            config.to_str().unwrap(),
            "--hidden-tools",
            "audit_stats",
            "--disabled-tags",
            "admin,delete",
            "--require-read-only-tools",
        ]);
        let settings = load_tool_visibility_with_default(&args, None)
            .await
            .unwrap();

        assert_eq!(settings.hidden, ["audit_log", "audit_stats"]);
        assert_eq!(settings.disabled_tags, ["delete", "admin"]);
        assert!(settings.require_read_only);
    }

    #[tokio::test]
    async fn existing_default_visibility_config_is_loaded() {
        let temp = tempfile::TempDir::new().unwrap();
        let config = temp.path().join("config.yaml");
        tokio::fs::write(&config, "tool_visibility:\n  disabled: [delete_note]\n")
            .await
            .unwrap();

        let settings = load_tool_visibility_with_default(&args(&[]), Some(&config))
            .await
            .unwrap();
        assert_eq!(settings.disabled, ["delete_note"]);
    }

    #[tokio::test]
    async fn missing_default_visibility_config_uses_empty_settings() {
        let temp = tempfile::TempDir::new().unwrap();
        let missing = temp.path().join("missing.yaml");
        let settings = load_tool_visibility_with_default(&args(&[]), Some(&missing))
            .await
            .unwrap();
        assert_eq!(settings, ToolVisibilitySettings::default());
    }

    #[test]
    fn vault_path_comparison_does_not_conflate_missing_paths() {
        let temp = tempfile::TempDir::new().unwrap();
        let first = temp.path().join("missing-a");
        let second = temp.path().join("missing-b");

        assert!(paths_refer_to_same_vault(&first, &first));
        assert!(!paths_refer_to_same_vault(&first, &second));
    }

    #[test]
    fn vault_path_comparison_canonicalizes_existing_paths() {
        let temp = tempfile::TempDir::new().unwrap();
        let nested = temp.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let equivalent = nested.join("..").join("nested");

        assert!(paths_refer_to_same_vault(&nested, &equivalent));
    }

    #[test]
    fn absolute_vault_path_expansion_is_stable() {
        let temp = tempfile::TempDir::new().unwrap();
        assert_eq!(expand_vault_path(temp.path()).unwrap(), temp.path());
    }

    #[tokio::test]
    async fn default_vault_registration_adds_skips_and_preserves_cached_path() {
        let first = tempfile::TempDir::new().unwrap();
        let second = tempfile::TempDir::new().unwrap();
        let server = ObsidianMcpServer::new().unwrap();

        assert_eq!(
            register_default_vault(&server, first.path(), WriteBackend::Direct, None)
                .await
                .unwrap(),
            DefaultVaultRegistration::Added
        );
        assert_eq!(
            register_default_vault(&server, first.path(), WriteBackend::Direct, None)
                .await
                .unwrap(),
            DefaultVaultRegistration::AlreadyRegistered
        );
        assert_eq!(
            register_default_vault(&server, second.path(), WriteBackend::Direct, None)
                .await
                .unwrap(),
            DefaultVaultRegistration::KeptCached {
                existing_path: first.path().to_path_buf()
            }
        );

        let config = server
            .multi_vault()
            .get_vault_config("default")
            .await
            .unwrap();
        assert_eq!(config.path, first.path());
    }
}
