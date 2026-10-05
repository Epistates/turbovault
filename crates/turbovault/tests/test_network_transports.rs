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
    let addr = format!("127.0.0.1:{}", free_port());
    let stopped = serve(turbomcp::Transport::http(&addr));

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
    let addr = format!("127.0.0.1:{}", free_port());
    let stopped = serve(turbomcp::Transport::tcp(&addr));

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

    let addr = format!("127.0.0.1:{}", free_port());
    let stopped = serve(turbomcp::Transport::websocket(&addr));

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
