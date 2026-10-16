//! The network transports carry real MCP sessions.
//!
//! HTTP, WebSocket and TCP compiled but nothing drove them (#88), so a
//! transport could break without a failing test. Each test here serves the
//! real server on a local port and talks to it with the first-party client:
//! initialize, list the tools, and call one.

#![cfg(any(feature = "http", feature = "websocket", feature = "tcp"))]

use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use turbomcp::{McpServerExt, ProtocolConfig};
use turbovault::ObsidianMcpServer;

/// A port nothing is listening on right now.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind an ephemeral port")
        .local_addr()
        .expect("local addr")
        .port()
}

/// How the background server stopped, once it has.
type Stopped = Arc<OnceLock<String>>;

/// Serve a fresh server over `transport` in the background. The returned
/// handle records why the server stopped, so a server that failed to start
/// (a port taken between `free_port` and the bind, say) fails the test with
/// its own error instead of a connect timeout.
fn serve(transport: turbomcp::Transport) -> Stopped {
    let stopped: Stopped = Arc::default();
    let record = stopped.clone();
    let server = ObsidianMcpServer::new().expect("server");
    tokio::spawn(async move {
        let outcome = server
            .builder()
            .with_protocol(ProtocolConfig::multi_version())
            .transport(transport)
            .serve()
            .await;
        let _ = record.set(match outcome {
            Ok(()) => "serve() returned".to_string(),
            Err(error) => format!("serve() failed: {error}"),
        });
    });
    stopped
}

/// Start a server on a free port, choosing another when something takes the
/// port between `free_port` and the server's bind, which Windows readily
/// does with a port just let go. A failed bind is reported almost at once,
/// so a server still up a moment later has its port.
async fn start_on_free_port(start: impl Fn(u16) -> Stopped) -> (String, Stopped) {
    for _ in 0..5 {
        let port = free_port();
        let stopped = start(port);
        tokio::time::sleep(Duration::from_millis(250)).await;
        match stopped.get() {
            Some(why) if why.contains("bind") => continue,
            _ => return (format!("127.0.0.1:{port}"), stopped),
        }
    }
    panic!("no free port could be bound in 5 tries");
}

/// Retry `connect` until the server is accepting, for up to 30 seconds of
/// wall clock. Counted in time rather than attempts: a refused connection
/// fails in microseconds on Unix but takes about two seconds on Windows. Each
/// attempt gets five seconds, because a socket that accepts and never answers
/// (another process that took the port, say) otherwise holds an HTTP attempt
/// for the client's whole 30-second request timeout.
async fn connect_when_ready<C, F, Fut>(stopped: &Stopped, mut connect: F) -> C
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<C, String>>,
{
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(why) = stopped.get() {
            panic!("the server stopped before accepting a connection: {why}");
        }
        let last_error = match tokio::time::timeout(Duration::from_secs(5), connect()).await {
            Ok(Ok(client)) => return client,
            Ok(Err(error)) => error,
            Err(_) => "the attempt got no answer within 5s".to_string(),
        };
        if Instant::now() >= deadline {
            panic!("server never accepted a connection in 30s; last error: {last_error}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The shared check: the session lists the core tools and runs one.
async fn assert_session_works<T>(client: &turbomcp_client::Client<T>)
where
    T: turbomcp_transport::Transport + 'static,
{
    let tools = client.list_tools().await.expect("tools/list");
    let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
    assert!(names.contains(&"read_note"), "{names:?}");
    assert!(names.contains(&"list_vaults"), "{names:?}");

    let result = client
        .call_tool("list_vaults", None, None)
        .await
        .expect("tools/call");
    assert_ne!(result.is_error, Some(true), "{result:?}");
}

#[cfg(feature = "http")]
#[tokio::test]
async fn http_serves_an_mcp_session() {
    let (addr, stopped) =
        start_on_free_port(|port| serve(turbomcp::Transport::http(format!("127.0.0.1:{port}"))))
            .await;

    let url = format!("http://{addr}");
    let client = connect_when_ready(&stopped, || {
        let url = url.clone();
        async move {
            turbomcp_client::Client::connect_http(url)
                .await
                .map_err(|e| e.to_string())
        }
    })
    .await;
    assert_session_works(&client).await;
}

#[cfg(feature = "tcp")]
#[tokio::test]
async fn tcp_serves_an_mcp_session() {
    let (addr, stopped) =
        start_on_free_port(|port| serve(turbomcp::Transport::tcp(format!("127.0.0.1:{port}"))))
            .await;

    let client = connect_when_ready(&stopped, || {
        let addr = addr.clone();
        async move {
            turbomcp_client::Client::connect_tcp(addr)
                .await
                .map_err(|e| e.to_string())
        }
    })
    .await;
    assert_session_works(&client).await;
}

#[cfg(feature = "websocket")]
#[tokio::test]
async fn websocket_serves_an_mcp_session() {
    use turbomcp_transport::websocket_bidirectional::{
        WebSocketBidirectionalConfig, WebSocketBidirectionalTransport,
    };

    let (addr, stopped) = start_on_free_port(|port| {
        serve(turbomcp::Transport::websocket(format!("127.0.0.1:{port}")))
    })
    .await;

    let url = format!("ws://{addr}/ws");
    let client = connect_when_ready(&stopped, || {
        let config = WebSocketBidirectionalConfig {
            url: Some(url.clone()),
            ..Default::default()
        };
        async move {
            let transport = WebSocketBidirectionalTransport::new(config)
                .await
                .map_err(|e| e.to_string())?;
            let client = turbomcp_client::Client::new(transport);
            client.initialize().await.map_err(|e| e.to_string())?;
            Ok(client)
        }
    })
    .await;
    assert_session_works(&client).await;
}

/// Send one HTTP/1.1 request to `addr` and return the status line and
/// headers, or `None` while nothing is listening yet.
#[cfg(feature = "http")]
async fn raw_post(addr: &str, authorization: Option<&str>) -> Option<String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(addr).await.ok()?;
    let body = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"probe","version":"0"}}}"#;
    let auth = authorization
        .map(|value| format!("Authorization: {value}\r\n"))
        .unwrap_or_default();
    let request = format!(
        "POST /mcp HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\n\
         Accept: application/json, text/event-stream\r\n{auth}Content-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.ok()?;
    let mut response = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response)).await;
    let text = String::from_utf8_lossy(&response).into_owned();
    Some(
        text.split("\r\n\r\n")
            .next()
            .unwrap_or_default()
            .to_string(),
    )
}

/// With `--auth-mode bearer`, a request without the token is refused with a
/// challenge that points at the protected resource metadata, and one with it
/// is served.
#[cfg(feature = "http")]
#[tokio::test]
async fn http_bearer_auth_refuses_requests_without_the_token() {
    use turbovault::security::{AuthMode, AuthSettings, SecuritySettings};

    let token = "k".repeat(turbovault::security::MIN_BEARER_TOKEN_LENGTH);
    let dir = tempfile::TempDir::new().unwrap();
    let token_file = dir.path().join("tokens");
    std::fs::write(&token_file, format!("{token}\n")).unwrap();
    let settings = SecuritySettings {
        auth: AuthSettings {
            mode: AuthMode::Bearer,
            token_file: Some(token_file),
            ..AuthSettings::default()
        },
        ..SecuritySettings::default()
    };
    let (addr, _stopped) = start_on_free_port(|port| {
        let config =
            turbovault::security::http_server_config(&settings, "127.0.0.1", port).unwrap();
        let stopped: Stopped = Arc::default();
        let record = stopped.clone();
        let server = ObsidianMcpServer::new().expect("server");
        tokio::spawn(async move {
            let outcome = server
                .builder()
                .with_config(config)
                .transport(turbomcp::Transport::http(format!("127.0.0.1:{port}")))
                .serve()
                .await;
            let _ = record.set(match outcome {
                Ok(()) => "serve() returned".to_string(),
                Err(error) => format!("serve() failed: {error}"),
            });
        });
        stopped
    })
    .await;

    let deadline = Instant::now() + Duration::from_secs(30);
    let refused = loop {
        if let Some(head) = raw_post(&addr, None).await {
            break head;
        }
        assert!(Instant::now() < deadline, "server never started");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(refused.starts_with("HTTP/1.1 401"), "{refused}");
    assert!(
        refused
            .to_ascii_lowercase()
            .contains("www-authenticate: bearer"),
        "{refused}"
    );

    let wrong = raw_post(&addr, Some(&format!("Bearer {}", "x".repeat(40))))
        .await
        .unwrap();
    assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");

    let served = raw_post(&addr, Some(&format!("Bearer {token}")))
        .await
        .unwrap();
    assert!(served.starts_with("HTTP/1.1 200"), "{served}");
}
