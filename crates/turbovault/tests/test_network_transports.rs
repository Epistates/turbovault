//! The network transports carry real MCP sessions.
//!
//! HTTP, WebSocket and TCP compiled but nothing drove them (#88), so a
//! transport could break without a failing test. Each test here serves the
//! real server on a local port and talks to it with the first-party client:
//! initialize, list the tools, and call one.

#![cfg(any(feature = "http", feature = "websocket", feature = "tcp"))]

use std::time::Duration;
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

/// Serve a fresh server over `transport` in the background.
fn serve(transport: turbomcp::Transport) {
    let server = ObsidianMcpServer::new().expect("server");
    tokio::spawn(async move {
        let _ = server
            .builder()
            .with_protocol(ProtocolConfig::multi_version())
            .transport(transport)
            .serve()
            .await;
    });
}

/// Retry `connect` until the server is accepting, for up to ten seconds.
async fn connect_when_ready<C, F, Fut>(mut connect: F) -> C
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<C>>,
{
    for _ in 0..100 {
        if let Some(client) = connect().await {
            return client;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("server never accepted a connection");
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
    serve(turbomcp::Transport::http(&addr));

    let url = format!("http://{addr}");
    let client = connect_when_ready(|| {
        let url = url.clone();
        async move { turbomcp_client::Client::connect_http(url).await.ok() }
    })
    .await;
    assert_session_works(&client).await;
}

#[cfg(feature = "tcp")]
#[tokio::test]
async fn tcp_serves_an_mcp_session() {
    let addr = format!("127.0.0.1:{}", free_port());
    serve(turbomcp::Transport::tcp(&addr));

    let client = connect_when_ready(|| {
        let addr = addr.clone();
        async move { turbomcp_client::Client::connect_tcp(addr).await.ok() }
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
    serve(turbomcp::Transport::websocket(&addr));

    let url = format!("ws://{addr}/ws");
    let client = connect_when_ready(|| {
        let config = WebSocketBidirectionalConfig {
            url: Some(url.clone()),
            ..Default::default()
        };
        async move {
            let transport = WebSocketBidirectionalTransport::new(config).await.ok()?;
            let client = turbomcp_client::Client::new(transport);
            client.initialize().await.ok()?;
            Some(client)
        }
    })
    .await;
    assert_session_works(&client).await;
}
