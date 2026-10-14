//! Who can reach the server and which directories it will serve.
//!
//! Settings come from the `security` section of the TurboVault YAML config,
//! with the matching command-line flags (and their environment variables)
//! taking precedence:
//!
//! ```yaml
//! security:
//!   # Directories add_vault may register vaults inside.
//!   allowed_vault_roots: ["~/Vaults"]
//!   auth:
//!     mode: jwt                        # none (default), bearer or jwt
//!     resource: https://vault.example.com/mcp
//!     issuer: https://auth.example.com
//!     jwks_uri: https://auth.example.com/.well-known/jwks.json
//!     required_scopes: ["vault:read"]
//!   rate_limit:
//!     requests: 120
//!     window_secs: 60
//!   allowed_origins: ["https://app.example.com"]
//!   trusted_proxies: ["10.0.0.0/8"]
//! ```
//!
//! The rules they feed:
//!
//! - **Vault registration.** With `allowed_vault_roots` set, `add_vault`
//!   (registering or creating) only accepts directories inside those roots. Without it
//!   they accept any directory on the stdio transport, where the caller is
//!   the local user's own client, and none on a network transport, where the
//!   caller could be anyone who can reach the port. A filesystem root, the
//!   home directory and the hidden and application-data directories inside
//!   it are refused under every policy; see [`VaultRootPolicy`].
//! - **Network exposure.** The HTTP transport may listen beyond loopback only
//!   with authentication configured. WebSocket and TCP have no
//!   authentication, so they listen on loopback only. Either refusal can be
//!   overridden with `allow_unauthenticated_network` for a deployment that
//!   authenticates in front of TurboVault, and it is logged loudly.
//! - **Authentication** (HTTP only) makes the server an OAuth 2.1 protected
//!   resource: every request needs `Authorization: Bearer <token>`, and the
//!   server publishes its protected resource metadata (RFC 9728). `jwt`
//!   validates tokens from an authorization server, including their
//!   audience; `bearer` accepts one of a fixed set of shared secrets, for a
//!   client configured with the token directly.
//! - **Rate limiting** (HTTP only) is per client. It is off on loopback
//!   unless asked for, and on at [`DEFAULT_NETWORK_RATE_LIMIT`] requests a
//!   minute when the server is reachable from the network.

use std::net::IpAddr;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use turbovault_tools::VaultRootPolicy;

/// Requests a minute each client may make when the HTTP server listens
/// beyond loopback and no rate limit was configured.
pub const DEFAULT_NETWORK_RATE_LIMIT: u32 = 600;

/// Minimum length of a static bearer token, matching TurboMCP's API key
/// validation.
pub const MIN_BEARER_TOKEN_LENGTH: usize = 32;

/// The `security` section of the TurboVault YAML config.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecuritySettings {
    /// Directories runtime vault registration may point inside.
    pub allowed_vault_roots: Vec<PathBuf>,
    /// HTTP authentication.
    pub auth: AuthSettings,
    /// HTTP rate limit.
    pub rate_limit: Option<RateLimitSettings>,
    /// Browser origins the HTTP transport accepts, besides localhost.
    pub allowed_origins: Vec<String>,
    /// Reverse proxies whose forwarded client address headers are trusted.
    pub trusted_proxies: Vec<String>,
    /// Listen beyond loopback without authentication. Only for a deployment
    /// that authenticates in front of TurboVault.
    pub allow_unauthenticated_network: bool,
}

/// How HTTP requests are authenticated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode {
    /// No authentication.
    #[default]
    None,
    /// One of a fixed set of shared secret tokens.
    Bearer,
    /// JWTs from an OAuth authorization server, validated against its JWKS.
    Jwt,
}

/// `security.auth`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthSettings {
    /// Authentication mode.
    pub mode: AuthMode,
    /// This server's canonical URL, the audience tokens must be issued for.
    /// Defaults to `http://<host>:<port>/mcp`.
    pub resource: Option<String>,
    /// Authorization servers advertised in the protected resource metadata.
    /// Defaults to the JWT issuer.
    pub authorization_servers: Vec<String>,
    /// `bearer`: file holding the accepted tokens, one per line.
    pub token_file: Option<PathBuf>,
    /// `jwt`: the expected `iss`.
    pub issuer: Option<String>,
    /// `jwt`: where the issuer's signing keys are published.
    pub jwks_uri: Option<String>,
    /// `jwt`: the expected `aud`. Defaults to `resource`.
    pub audience: Option<String>,
    /// `jwt`: scopes every token must carry.
    pub required_scopes: Vec<String>,
}

/// `security.rate_limit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitSettings {
    /// Requests allowed per window, per client. `0` turns rate limiting off.
    pub requests: u32,
    /// Window length in seconds.
    #[serde(default = "default_window_secs")]
    pub window_secs: u64,
}

fn default_window_secs() -> u64 {
    60
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SecurityConfigFile {
    security: SecuritySettings,
}

impl SecuritySettings {
    /// Read the `security` section of a TurboVault YAML config. Other
    /// sections are ignored; an unknown key inside `security` is an error,
    /// so a misspelled setting is not silently left at its default.
    pub fn from_yaml_str(yaml: &str) -> anyhow::Result<Self> {
        use anyhow::Context;
        let file: SecurityConfigFile = yaml_serde::from_str(yaml)
            .context("invalid `security` section in TurboVault config")?;
        Ok(file.security)
    }

    /// [`Self::from_yaml_str`] for a file on disk.
    pub async fn load(path: &Path) -> anyhow::Result<Self> {
        use anyhow::Context;
        let yaml = tokio::fs::read_to_string(path)
            .await
            .with_context(|| format!("failed to read TurboVault config {}", path.display()))?;
        Self::from_yaml_str(&yaml)
            .with_context(|| format!("failed to parse TurboVault config {}", path.display()))
    }

    /// The registration policy for `transport`. See the module docs.
    pub fn vault_root_policy(&self, transport: &str) -> Result<VaultRootPolicy, String> {
        if !self.allowed_vault_roots.is_empty() {
            let roots: Vec<PathBuf> = self
                .allowed_vault_roots
                .iter()
                .map(|root| expand_operator_path(root))
                .collect::<Result<_, _>>()?;
            return VaultRootPolicy::within(roots).map_err(|error| error.to_string());
        }
        Ok(if transport == "stdio" {
            VaultRootPolicy::anywhere()
        } else {
            VaultRootPolicy::closed()
        })
    }

    /// Refuse a listening configuration that would expose the server to the
    /// network without authentication. See the module docs.
    pub fn check_exposure(&self, transport: &str, host: &str) -> Result<(), String> {
        if self.auth.mode != AuthMode::None && transport != "http" {
            return Err(format!(
                "authentication is only supported on the http transport, not {transport}"
            ));
        }
        if !matches!(transport, "http" | "websocket" | "tcp") || is_loopback(host) {
            return Ok(());
        }
        if self.allow_unauthenticated_network {
            if self.auth.mode == AuthMode::None {
                tracing::warn!(
                    "Listening on {host} over {transport} WITHOUT AUTHENTICATION because \
                     allow_unauthenticated_network is set: anyone who can reach this port can \
                     read and write every registered vault"
                );
            }
            return Ok(());
        }
        match transport {
            "http" if self.auth.mode != AuthMode::None => Ok(()),
            "http" => Err(format!(
                "refusing to listen on {host} without authentication: anyone who can reach \
                 the port could read and write every registered vault. Configure \
                 --auth-mode bearer or jwt, or listen on 127.0.0.1"
            )),
            _ => Err(format!(
                "the {transport} transport has no authentication, so it only listens on \
                 loopback (127.0.0.1 or ::1), not {host}. Use the http transport with \
                 --auth-mode for network access"
            )),
        }
    }

    /// The rate limit to apply on the HTTP transport, as (requests, window).
    pub fn effective_rate_limit(&self, host: &str) -> Option<(u32, std::time::Duration)> {
        match self.rate_limit {
            Some(RateLimitSettings { requests: 0, .. }) => None,
            Some(RateLimitSettings {
                requests,
                window_secs,
            }) => Some((requests, std::time::Duration::from_secs(window_secs.max(1)))),
            None if is_loopback(host) => None,
            None => Some((
                DEFAULT_NETWORK_RATE_LIMIT,
                std::time::Duration::from_secs(60),
            )),
        }
    }
}

/// Whether `host` only accepts connections from this machine.
pub fn is_loopback(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

/// Expand `~` and `$VAR` in a path the operator wrote.
pub fn expand_operator_path(path: &Path) -> Result<PathBuf, String> {
    shellexpand::full(&path.to_string_lossy())
        .map(|expanded| PathBuf::from(expanded.into_owned()))
        .map_err(|error| format!("Failed to expand path {}: {error}", path.display()))
}

/// Read static bearer tokens from `path`: one per line, blank lines and
/// lines starting with `#` ignored. Each must be at least
/// [`MIN_BEARER_TOKEN_LENGTH`] characters.
pub fn read_bearer_tokens(path: &Path) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read token file {}: {error}", path.display()))?;
    parse_bearer_tokens(&text).map_err(|error| format!("token file {}: {error}", path.display()))
}

fn parse_bearer_tokens(text: &str) -> Result<Vec<String>, String> {
    let tokens: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();
    if tokens.is_empty() {
        return Err("no tokens".to_string());
    }
    if let Some(index) = tokens
        .iter()
        .position(|token| token.len() < MIN_BEARER_TOKEN_LENGTH)
    {
        return Err(format!(
            "token {} is shorter than {MIN_BEARER_TOKEN_LENGTH} characters",
            index + 1
        ));
    }
    Ok(tokens)
}

/// Where the unix transport listens when no socket path is given: a
/// private (0700) `turbovault` directory under `$XDG_RUNTIME_DIR`, or under
/// `~/.turbovault/run` without one. Creates the directory.
#[cfg(unix)]
pub fn default_socket_path() -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    let dir = match std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty()) {
        Some(runtime) => PathBuf::from(runtime).join("turbovault"),
        None => {
            let home = shellexpand::tilde("~");
            if home == "~" {
                return Err(
                    "cannot choose a private socket directory: neither XDG_RUNTIME_DIR nor HOME \
                     is set. Pass --socket-path"
                        .to_string(),
                );
            }
            PathBuf::from(home.as_ref()).join(".turbovault").join("run")
        }
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .map_err(|error| format!("cannot create socket directory {}: {error}", dir.display()))?;
    // An existing directory keeps whatever mode it had; this one is ours.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).map_err(|error| {
        format!(
            "cannot restrict socket directory {}: {error}",
            dir.display()
        )
    })?;
    Ok(dir.join("turbovault.sock"))
}

/// Warn when a socket's directory lets other users reach it. TurboMCP binds
/// the socket with the process umask, so the directory is what keeps other
/// local users out.
#[cfg(unix)]
pub fn warn_if_socket_dir_is_shared(socket: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let Some(dir) = socket.parent() else { return };
    if let Ok(metadata) = std::fs::metadata(dir)
        && metadata.permissions().mode() & 0o077 != 0
    {
        tracing::warn!(
            "The socket directory {} is accessible to other users, who may be able to \
             connect to this server and use every registered vault. Put the socket in a \
             private (0700) directory",
            dir.display()
        );
    }
}

/// HTTP authorization, built from [`AuthSettings`].
#[cfg(feature = "http")]
pub mod http_auth {
    use super::{AuthMode, AuthSettings, read_bearer_tokens};
    use turbomcp::{BearerRejection, BearerTokenValidator, HttpAuthorization, ValidationFuture};

    /// Accepts any one of a fixed set of tokens, compared in constant time.
    pub struct StaticBearerValidator {
        tokens: Vec<String>,
    }

    impl StaticBearerValidator {
        /// Accept `tokens`, each at least
        /// [`super::MIN_BEARER_TOKEN_LENGTH`] characters.
        pub fn new(tokens: Vec<String>) -> Self {
            Self { tokens }
        }
    }

    impl BearerTokenValidator for StaticBearerValidator {
        fn validate<'a>(&'a self, token: &'a str) -> ValidationFuture<'a> {
            Box::pin(async move {
                // Every token is compared, so the time taken doesn't depend
                // on which one matched.
                let matched = self.tokens.iter().enumerate().fold(None, |found, (i, t)| {
                    let hit = turbomcp::auth::api_key_validation::validate_api_key(token, t);
                    found.or(hit.then_some(i))
                });
                match matched {
                    Some(index) => Ok(turbomcp_core::auth::Principal::new(format!(
                        "static-token-{}",
                        index + 1
                    ))),
                    None => Err(BearerRejection::InvalidToken(
                        "unrecognized token".to_string(),
                    )),
                }
            })
        }
    }

    /// The authorization to configure, or `None` for mode `none`.
    /// `default_resource` is used when `settings.resource` is not set;
    /// `env_token` is a token from `TURBOVAULT_AUTH_TOKEN`.
    pub fn build(
        settings: &AuthSettings,
        default_resource: &str,
        env_token: Option<String>,
    ) -> Result<Option<HttpAuthorization>, String> {
        let resource = settings
            .resource
            .clone()
            .unwrap_or_else(|| default_resource.to_string());
        let authorization = match settings.mode {
            AuthMode::None => return Ok(None),
            AuthMode::Bearer => {
                let mut tokens = match &settings.token_file {
                    Some(path) => read_bearer_tokens(path)?,
                    None => Vec::new(),
                };
                if let Some(token) = env_token {
                    tokens.extend(
                        super::parse_bearer_tokens(&token)
                            .map_err(|error| format!("TURBOVAULT_AUTH_TOKEN: {error}"))?,
                    );
                }
                if tokens.is_empty() {
                    return Err("--auth-mode bearer needs tokens: set --auth-token-file or \
                         TURBOVAULT_AUTH_TOKEN"
                        .to_string());
                }
                // A shared secret has no authorization server; the metadata
                // still has to name one, so it names this resource.
                let server = settings
                    .authorization_servers
                    .first()
                    .cloned()
                    .unwrap_or_else(|| resource.clone());
                HttpAuthorization::new(&resource, server, StaticBearerValidator::new(tokens))
            }
            AuthMode::Jwt => {
                let issuer = settings
                    .issuer
                    .clone()
                    .ok_or("--auth-mode jwt needs --auth-issuer")?;
                let jwks_uri = settings
                    .jwks_uri
                    .clone()
                    .ok_or("--auth-mode jwt needs --auth-jwks-uri")?;
                let audience = settings
                    .audience
                    .clone()
                    .unwrap_or_else(|| resource.clone());
                let validator = turbomcp::auth::jwt::JwtValidator::with_jwks_uri(
                    issuer.clone(),
                    audience,
                    jwks_uri,
                );
                let server = settings
                    .authorization_servers
                    .first()
                    .cloned()
                    .unwrap_or(issuer);
                HttpAuthorization::new(
                    &resource,
                    server,
                    turbomcp::auth::server::JwtBearerValidator::new(validator)
                        .with_required_scopes(settings.required_scopes.iter().cloned()),
                )
                .with_scopes_supported(settings.required_scopes.iter().cloned())
            }
        };
        Ok(Some(
            settings
                .authorization_servers
                .iter()
                .skip(1)
                .fold(authorization, |authorization, server| {
                    authorization.with_authorization_server(server)
                }),
        ))
    }
}

/// The Streamable HTTP server configuration: protocol versions, origin
/// policy, rate limit and authorization.
#[cfg(feature = "http")]
pub fn http_server_config(
    security: &SecuritySettings,
    host: &str,
    port: u16,
) -> Result<turbomcp_server::ServerConfig, String> {
    let authorization = crate::security::http_auth::build(
        &security.auth,
        &format!("http://{}/mcp", host_port(host, port)),
        std::env::var("TURBOVAULT_AUTH_TOKEN").ok(),
    )?;
    let mut builder = turbomcp_server::ServerConfig::builder()
        .protocol(turbomcp::ProtocolConfig::multi_version())
        .origin_validation(turbomcp_server::OriginValidationConfig {
            allowed_origins: security.allowed_origins.iter().cloned().collect(),
            trusted_proxies: security.trusted_proxies.clone(),
            // Non-browser clients send no Origin. Admit them only when every
            // request has to authenticate anyway.
            allow_missing_origin: authorization.is_some() || security.allow_unauthenticated_network,
            ..turbomcp_server::OriginValidationConfig::default()
        });
    if let Some((requests, window)) = security.effective_rate_limit(host) {
        tracing::info!("Rate limit: {requests} requests per {window:?} per client");
        builder = builder.rate_limit(turbomcp_server::RateLimitConfig::new(requests, window));
    }
    match authorization {
        Some(authorization) => {
            tracing::info!(
                "HTTP authorization required ({:?}) for resource {}",
                security.auth.mode,
                authorization.resource()
            );
            builder = builder.authorization(authorization);
        }
        None => tracing::warn!(
            "HTTP transport is unauthenticated: any local process can use every registered \
             vault. Set --auth-mode to require a token"
        ),
    }
    Ok(builder.build())
}

#[cfg(feature = "http")]
fn host_port(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_hosts() {
        for host in ["127.0.0.1", "::1", "[::1]", "localhost", "127.0.0.2"] {
            assert!(is_loopback(host), "{host}");
        }
        for host in ["0.0.0.0", "::", "192.168.1.5", "example.com"] {
            assert!(!is_loopback(host), "{host}");
        }
    }

    #[test]
    fn network_http_needs_authentication() {
        let open = SecuritySettings::default();
        assert!(open.check_exposure("http", "127.0.0.1").is_ok());
        assert!(open.check_exposure("stdio", "0.0.0.0").is_ok());
        let error = open.check_exposure("http", "0.0.0.0").unwrap_err();
        assert!(error.contains("--auth-mode"), "{error}");

        let authed = SecuritySettings {
            auth: AuthSettings {
                mode: AuthMode::Bearer,
                ..AuthSettings::default()
            },
            ..SecuritySettings::default()
        };
        assert!(authed.check_exposure("http", "0.0.0.0").is_ok());
    }

    #[test]
    fn unauthenticated_transports_stay_on_loopback() {
        let settings = SecuritySettings::default();
        for transport in ["websocket", "tcp"] {
            assert!(settings.check_exposure(transport, "127.0.0.1").is_ok());
            assert!(settings.check_exposure(transport, "0.0.0.0").is_err());
        }
        let overridden = SecuritySettings {
            allow_unauthenticated_network: true,
            ..SecuritySettings::default()
        };
        assert!(overridden.check_exposure("tcp", "0.0.0.0").is_ok());
    }

    #[test]
    fn authentication_is_http_only() {
        let settings = SecuritySettings {
            auth: AuthSettings {
                mode: AuthMode::Jwt,
                ..AuthSettings::default()
            },
            ..SecuritySettings::default()
        };
        assert!(settings.check_exposure("websocket", "127.0.0.1").is_err());
    }

    #[test]
    fn registration_closes_on_network_transports_without_roots() {
        let settings = SecuritySettings::default();
        assert!(
            settings
                .vault_root_policy("stdio")
                .unwrap()
                .allows_registration()
        );
        for transport in ["http", "websocket", "tcp", "unix"] {
            assert!(
                !settings
                    .vault_root_policy(transport)
                    .unwrap()
                    .allows_registration(),
                "{transport}"
            );
        }

        let root = tempfile::TempDir::new().unwrap();
        let rooted = SecuritySettings {
            allowed_vault_roots: vec![root.path().to_path_buf()],
            ..SecuritySettings::default()
        };
        let policy = rooted.vault_root_policy("http").unwrap();
        assert!(policy.allows_registration());
        assert!(policy.roots().is_some());
    }

    #[test]
    fn network_rate_limit_defaults_on() {
        let settings = SecuritySettings::default();
        assert_eq!(settings.effective_rate_limit("127.0.0.1"), None);
        assert_eq!(
            settings.effective_rate_limit("0.0.0.0"),
            Some((
                DEFAULT_NETWORK_RATE_LIMIT,
                std::time::Duration::from_secs(60)
            ))
        );
        let off = SecuritySettings {
            rate_limit: Some(RateLimitSettings {
                requests: 0,
                window_secs: 60,
            }),
            ..SecuritySettings::default()
        };
        assert_eq!(off.effective_rate_limit("0.0.0.0"), None);
    }

    #[test]
    fn bearer_tokens_must_be_long() {
        assert!(parse_bearer_tokens("# comment\n\nshort\n").is_err());
        assert!(parse_bearer_tokens("").is_err());
        let token = "a".repeat(MIN_BEARER_TOKEN_LENGTH);
        assert_eq!(
            parse_bearer_tokens(&format!("# tokens\n{token}\n")).unwrap(),
            vec![token]
        );
    }

    #[test]
    fn security_section_parses_and_rejects_typos() {
        let settings = SecuritySettings::from_yaml_str(
            "tool_visibility: {}\nsecurity:\n  allowed_vault_roots: [/srv/vaults]\n  auth:\n    mode: jwt\n    issuer: https://auth.example.com\n  rate_limit:\n    requests: 10\n",
        )
        .unwrap();
        assert_eq!(settings.allowed_vault_roots, [PathBuf::from("/srv/vaults")]);
        assert_eq!(settings.auth.mode, AuthMode::Jwt);
        assert_eq!(
            settings.rate_limit,
            Some(RateLimitSettings {
                requests: 10,
                window_secs: 60
            })
        );
        assert!(SecuritySettings::from_yaml_str("security:\n  allowed_roots: [/x]\n").is_err());
        assert!(
            SecuritySettings::from_yaml_str("vaults: []\n")
                .unwrap()
                .allowed_vault_roots
                .is_empty()
        );
    }

    #[cfg(unix)]
    #[test]
    fn default_socket_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let Ok(path) = default_socket_path() else {
            return;
        };
        let mode = std::fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "socket directory is shared: {mode:o}");
    }

    #[cfg(feature = "http")]
    #[tokio::test]
    async fn static_bearer_tokens_are_checked() {
        use turbomcp::BearerTokenValidator;
        let token = "t".repeat(MIN_BEARER_TOKEN_LENGTH);
        let validator = http_auth::StaticBearerValidator::new(vec![token.clone()]);
        assert!(validator.validate(&token).await.is_ok());
        assert!(
            validator
                .validate(&"u".repeat(MIN_BEARER_TOKEN_LENGTH))
                .await
                .is_err()
        );
        assert!(validator.validate("").await.is_err());
    }

    #[cfg(feature = "http")]
    #[test]
    fn auth_modes_need_their_settings() {
        let bearer = AuthSettings {
            mode: AuthMode::Bearer,
            ..AuthSettings::default()
        };
        assert!(http_auth::build(&bearer, "http://127.0.0.1:3000/mcp", None).is_err());
        assert!(
            http_auth::build(
                &bearer,
                "http://127.0.0.1:3000/mcp",
                Some("x".repeat(MIN_BEARER_TOKEN_LENGTH))
            )
            .unwrap()
            .is_some()
        );
        let jwt = AuthSettings {
            mode: AuthMode::Jwt,
            issuer: Some("https://auth.example.com".to_string()),
            ..AuthSettings::default()
        };
        let error = http_auth::build(&jwt, "http://127.0.0.1:3000/mcp", None).unwrap_err();
        assert!(error.contains("--auth-jwks-uri"), "{error}");
        assert!(
            http_auth::build(&AuthSettings::default(), "http://x/mcp", None)
                .unwrap()
                .is_none()
        );
    }
}
