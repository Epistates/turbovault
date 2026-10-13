//! A client session for in-process tests: records the notifications the
//! server sends it, and answers the requests it makes (elicitation) with a
//! reply the test chose.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use turbomcp_core::session::{McpSession, SessionFuture};
use turbomcp_types::{ClientCapabilities, ElicitationCapabilities};

#[derive(Debug, Default)]
pub struct RecordingSession {
    /// `(method, params)` of every notification, in order.
    pub notifications: Mutex<Vec<(String, Value)>>,
    /// `(method, params)` of every request the server made.
    pub requests: Mutex<Vec<(String, Value)>>,
    /// What the client says it can do.
    pub capabilities: Option<ClientCapabilities>,
    /// The answer to an `elicitation/create` request.
    pub elicitation_reply: Mutex<Option<Value>>,
}

impl RecordingSession {
    /// A client that declares no capabilities.
    pub fn plain() -> Arc<Self> {
        Arc::new(Self {
            capabilities: Some(ClientCapabilities::default()),
            ..Self::default()
        })
    }

    /// A client that can answer form elicitations, with `reply`.
    pub fn eliciting(reply: Value) -> Arc<Self> {
        Arc::new(Self {
            capabilities: Some(ClientCapabilities {
                elicitation: Some(ElicitationCapabilities::default()),
                ..ClientCapabilities::default()
            }),
            elicitation_reply: Mutex::new(Some(reply)),
            ..Self::default()
        })
    }

    /// The notifications with `method` so far.
    pub fn sent(&self, method: &str) -> Vec<Value> {
        self.notifications
            .lock()
            .unwrap()
            .iter()
            .filter(|(sent, _)| sent == method)
            .map(|(_, params)| params.clone())
            .collect()
    }

    /// Wait up to `timeout` for a notification with `method` whose params
    /// satisfy `matches`.
    pub async fn wait_for(
        &self,
        method: &str,
        timeout: Duration,
        matches: impl Fn(&Value) -> bool,
    ) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if self.sent(method).iter().any(&matches) {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

impl McpSession for RecordingSession {
    fn client_capabilities<'a>(&'a self) -> SessionFuture<'a, Option<ClientCapabilities>> {
        Box::pin(async move { Ok(self.capabilities.clone()) })
    }

    fn call<'a>(&'a self, method: &'a str, params: Value) -> SessionFuture<'a, Value> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push((method.to_string(), params));
            match method {
                "elicitation/create" => Ok(self
                    .elicitation_reply
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| serde_json::json!({ "action": "cancel" }))),
                _ => Err(turbomcp::McpError::capability_not_supported(method)),
            }
        })
    }

    fn notify<'a>(&'a self, method: &'a str, params: Value) -> SessionFuture<'a, ()> {
        Box::pin(async move {
            self.notifications
                .lock()
                .unwrap()
                .push((method.to_string(), params));
            Ok(())
        })
    }
}
