//! JSON-RPC 2.0 transport over stdin/stdout for ACP (Agent Client Protocol).
//!
//! This module provides [`Transport`], which wraps a kiro-cli child process's
//! stdin/stdout pipes with a framing layer. Messages are newline-delimited JSON.
//!
//! # Architecture
//!
//! - **Writer side**: serializes outgoing requests/notifications to JSON lines on stdin.
//! - **Reader task**: runs in a background tokio task, reading stdout line by line.
//!   Each line is parsed and routed:
//!   - Lines with `id` AND no `method` -> our request's response (via `oneshot`).
//!   - Lines with `id` AND `method` -> bidirectional request from agent.
//!   - Lines with no `id` -> notification from agent.
//!
//! Callers register pending request slots before writing to avoid race conditions.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::error::{ClawMuxError, Result};

use super::types::{IncomingMessage, RpcNotification, RpcRequest, RpcResponse};

/// Pending response slots: maps request id -> oneshot sender.
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

/// JSON-RPC 2.0 transport over a child process's stdin/stdout.
///
/// Clone-safe (backed by `Arc`). Dropping all clones shuts down the writer side;
/// the reader task exits naturally when the child's stdout closes.
#[derive(Clone)]
pub struct Transport {
    writer: Arc<Mutex<ChildStdin>>,
    pending: PendingMap,
    next_id: Arc<AtomicU64>,
}

impl Transport {
    /// Create a new transport and spawn the reader task.
    ///
    /// # Arguments
    /// * `stdin` – piped stdin of the kiro-cli child process.
    /// * `stdout` – piped stdout of the kiro-cli child process.
    /// * `notification_tx` – channel for agent notifications and bidirectional requests.
    ///
    /// Returns the transport handle and the reader task `JoinHandle` (for cleanup).
    pub fn new(
        stdin: ChildStdin,
        stdout: ChildStdout,
        notification_tx: mpsc::Sender<IncomingMessage>,
    ) -> (Self, JoinHandle<()>) {
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let transport = Self {
            writer: Arc::new(Mutex::new(stdin)),
            pending: pending.clone(),
            next_id: Arc::new(AtomicU64::new(1)),
        };
        let handle = tokio::spawn(reader_task(stdout, pending, notification_tx));
        (transport, handle)
    }

    /// Allocate a fresh request ID (monotonically increasing).
    pub(crate) fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Send a JSON-RPC request and await the correlated response.
    ///
    /// Returns `Ok(Value)` on success, `Err(ClawMuxError::Kiro(...))` if the
    /// remote returns an error object or if the channel closes unexpectedly.
    pub async fn request(&self, method: impl Into<String>, params: Option<Value>) -> Result<Value> {
        let id = self.next_id();
        let req = RpcRequest::new(id, method, params);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        self.write_line(&serde_json::to_value(&req)?).await?;

        rx.await.map_err(|_| {
            ClawMuxError::Kiro("transport channel closed before response arrived".to_string())
        })?
    }

    /// Send a JSON-RPC notification (no response expected).
    pub async fn notify(&self, method: impl Into<String>, params: Option<Value>) -> Result<()> {
        let notif = RpcNotification::new(method, params);
        self.write_line(&serde_json::to_value(&notif)?).await
    }

    /// Send a JSON-RPC response to a bidirectional agent request.
    ///
    /// `id` must match the `id` from the agent's request so it can correlate the reply.
    pub async fn respond(&self, id: &Value, result: Value) -> Result<()> {
        let resp = RpcResponse::ok(id.clone(), result);
        self.write_line(&serde_json::to_value(&resp)?).await
    }

    /// Send a JSON-RPC error response to a bidirectional agent request.
    pub async fn respond_error(
        &self,
        id: &Value,
        code: i64,
        message: impl Into<String>,
    ) -> Result<()> {
        use super::types::RpcError;
        let resp = RpcResponse::err(
            id.clone(),
            RpcError {
                code,
                message: message.into(),
                data: None,
            },
        );
        self.write_line(&serde_json::to_value(&resp)?).await
    }

    /// Write a JSON value as a newline-terminated line on stdin.
    async fn write_line(&self, value: &Value) -> Result<()> {
        let mut line = serde_json::to_string(value)?;
        line.push('\n');
        let mut writer = self.writer.lock().await;
        writer
            .write_all(line.as_bytes())
            .await
            .map_err(ClawMuxError::Io)
    }
}

/// Background reader task: reads stdout line-by-line, routes to pending or notification channel.
pub(crate) async fn reader_task(
    stdout: ChildStdout,
    pending: PendingMap,
    notification_tx: mpsc::Sender<IncomingMessage>,
) {
    let reader = BufReader::new(stdout);
    let mut lines = reader.lines();

    loop {
        let line = match lines.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => {
                // stdout EOF -- process exited
                tracing::debug!("kiro-cli stdout closed (EOF)");
                break;
            }
            Err(e) => {
                tracing::warn!("kiro-cli stdout read error: {e}");
                break;
            }
        };

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let parsed: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("kiro-cli sent invalid JSON: {e} -- line: {line}");
                continue;
            }
        };

        route_message(parsed, &pending, &notification_tx).await;
    }

    // Drain pending waiters with an error so callers don't hang forever.
    let mut map = pending.lock().await;
    for (id, tx) in map.drain() {
        let _ = tx.send(Err(ClawMuxError::Kiro(format!(
            "process exited before responding to request {id}"
        ))));
    }
}

/// Route a parsed JSON value to the appropriate channel.
pub(crate) async fn route_message(
    value: Value,
    pending: &PendingMap,
    notification_tx: &mpsc::Sender<IncomingMessage>,
) {
    let has_id = value.get("id").is_some();
    let has_method = value.get("method").is_some();

    match (has_id, has_method) {
        // Response to one of our requests (has id, no method)
        (true, false) => {
            let id = match value["id"].as_u64() {
                Some(n) => n,
                None => {
                    tracing::warn!("kiro-cli response has non-numeric id: {value}");
                    return;
                }
            };
            let tx = {
                let mut map = pending.lock().await;
                map.remove(&id)
            };
            let Some(tx) = tx else {
                tracing::warn!("kiro-cli response for unknown request id {id}");
                return;
            };

            let result = if value.get("error").is_some() {
                let msg = value["error"]["message"]
                    .as_str()
                    .unwrap_or("unknown error")
                    .to_string();
                let code = value["error"]["code"].as_i64().unwrap_or(-1);
                Err(ClawMuxError::Kiro(format!("RPC error {code}: {msg}")))
            } else {
                Ok(value.get("result").cloned().unwrap_or(Value::Null))
            };

            let _ = tx.send(result);
        }

        // Bidirectional request from agent (has id AND method)
        (true, true) => match serde_json::from_value::<RpcRequest>(value) {
            Ok(req) => {
                let _ = notification_tx.send(IncomingMessage::Request(req)).await;
            }
            Err(e) => {
                tracing::warn!("failed to parse agent bidirectional request: {e}");
            }
        },

        // Notification from agent (no id, has method)
        (false, true) => match serde_json::from_value::<RpcNotification>(value) {
            Ok(notif) => {
                let _ = notification_tx
                    .send(IncomingMessage::Notification(notif))
                    .await;
            }
            Err(e) => {
                tracing::warn!("failed to parse agent notification: {e}");
            }
        },

        // Unknown message format: no id, no method -- log and ignore.
        (false, false) => {
            tracing::debug!("kiro-cli sent unrecognized message (no id, no method): {value}");
        }
    }
}

#[cfg(test)]
impl Transport {
    /// Create a transport backed by a `cat` child process for use in tests.
    ///
    /// Returns the transport and a `JoinHandle` for the reader task.
    /// The caller should drop both when the test is done.
    pub fn new_test() -> (Self, JoinHandle<()>) {
        use tokio::process::Command;
        let mut child = Command::new("cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("failed to spawn cat for test transport");
        let stdin = child.stdin.take().expect("cat stdin");
        let stdout = child.stdout.take().expect("cat stdout");
        let (notification_tx, _notification_rx) = mpsc::channel(64);
        // Leak the child handle; it will be cleaned up when the process exits.
        tokio::spawn(async move {
            let _ = child.wait().await;
        });
        Transport::new(stdin, stdout, notification_tx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    // These tests exercise the core routing logic (route_message) which is the
    // heart of the transport layer. End-to-end Transport tests (which require
    // ChildStdin/ChildStdout from a real process) are covered by KiroProcess
    // integration tests in process.rs.

    #[tokio::test]
    async fn test_route_response_to_pending_waiter() {
        let (notification_tx, mut notification_rx) = mpsc::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(5, tx);

        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 5,
            "result": {"sessionId": "abc-123"}
        });

        route_message(response, &pending, &notification_tx).await;

        let result = rx.await.unwrap().unwrap();
        assert_eq!(result["sessionId"], "abc-123");
        assert!(notification_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_route_error_response_to_pending_waiter() {
        let (notification_tx, _notification_rx) = mpsc::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(3, tx);

        let error_response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 3,
            "error": {"code": -32600, "message": "Invalid Request"}
        });

        route_message(error_response, &pending, &notification_tx).await;

        let result = rx.await.unwrap();
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ClawMuxError::Kiro(_)));
        assert!(err.to_string().contains("Invalid Request"));
        assert!(err.to_string().contains("-32600"));
    }

    #[tokio::test]
    async fn test_route_notification_to_channel() {
        let (notification_tx, mut notification_rx) = mpsc::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "agent_message_chunk",
            "params": {"sessionId": "sess-1", "delta": "Hello"}
        });

        route_message(notification, &pending, &notification_tx).await;

        let msg = notification_rx.recv().await.unwrap();
        assert!(matches!(msg, IncomingMessage::Notification(_)));
        if let IncomingMessage::Notification(n) = msg {
            assert_eq!(n.method, "agent_message_chunk");
            let params = n.params.unwrap();
            assert_eq!(params["delta"], "Hello");
        }
    }

    #[tokio::test]
    async fn test_route_bidirectional_request_to_channel() {
        let (notification_tx, mut notification_rx) = mpsc::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let bidi_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "session/request_permission",
            "params": {
                "sessionId": "sess-1",
                "permission": "file_write",
                "patterns": ["src/**"]
            }
        });

        route_message(bidi_request, &pending, &notification_tx).await;

        let msg = notification_rx.recv().await.unwrap();
        assert!(matches!(msg, IncomingMessage::Request(_)));
        if let IncomingMessage::Request(req) = msg {
            assert_eq!(req.method, "session/request_permission");
            assert_eq!(req.id, serde_json::json!(10));
        }
    }

    /// Regression test: kiro-cli sends UUID strings as bidirectional request IDs.
    ///
    /// Previously `RpcRequest.id` was `u64`, causing deserialization to fail and
    /// permission requests to be silently dropped.
    #[tokio::test]
    async fn test_route_bidirectional_request_uuid_string_id() {
        let (notification_tx, mut notification_rx) = mpsc::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let bidi_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "8c599dcf-5387-4823-8335-102f884f9048",
            "method": "session/request_permission",
            "params": {
                "sessionId": "sess-1",
                "permission": "file_write",
                "patterns": ["docs/**"]
            }
        });

        route_message(bidi_request, &pending, &notification_tx).await;

        let msg = notification_rx.recv().await.unwrap();
        assert!(matches!(msg, IncomingMessage::Request(_)));
        if let IncomingMessage::Request(req) = msg {
            assert_eq!(req.method, "session/request_permission");
            assert_eq!(
                req.id,
                serde_json::json!("8c599dcf-5387-4823-8335-102f884f9048")
            );
        }
    }

    #[tokio::test]
    async fn test_route_unknown_response_id_ignored() {
        let (notification_tx, mut notification_rx) = mpsc::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        // No pending waiter registered for id=99
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 99,
            "result": {}
        });

        route_message(response, &pending, &notification_tx).await;
        assert!(notification_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_route_non_numeric_id_ignored() {
        let (notification_tx, mut notification_rx) = mpsc::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": "string-id",
            "result": {}
        });

        route_message(response, &pending, &notification_tx).await;
        assert!(notification_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_pending_drained_on_close() {
        // Verify that pending oneshot senders receive errors when drained.
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let (tx1, rx1) = oneshot::channel::<Result<Value>>();
        let (tx2, rx2) = oneshot::channel::<Result<Value>>();
        pending.lock().await.insert(1, tx1);
        pending.lock().await.insert(2, tx2);

        // Drain manually (simulating what reader_task does on EOF)
        let mut map = pending.lock().await;
        for (id, tx) in map.drain() {
            let _ = tx.send(Err(ClawMuxError::Kiro(format!(
                "process exited before responding to request {id}"
            ))));
        }
        drop(map);

        let err1 = rx1.await.unwrap().unwrap_err();
        let err2 = rx2.await.unwrap().unwrap_err();
        assert!(matches!(err1, ClawMuxError::Kiro(_)));
        assert!(matches!(err2, ClawMuxError::Kiro(_)));
        assert!(err1.to_string().contains("process exited"));
        assert!(err2.to_string().contains("process exited"));
    }

    #[tokio::test]
    async fn test_route_result_null_when_no_result_field() {
        let (notification_tx, _) = mpsc::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        let (tx, rx) = oneshot::channel();
        pending.lock().await.insert(7, tx);

        // Response with id but no result field (just null result)
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 7
        });

        route_message(response, &pending, &notification_tx).await;

        let result = rx.await.unwrap().unwrap();
        assert_eq!(result, Value::Null);
    }

    #[tokio::test]
    async fn test_route_multiple_notifications_in_order() {
        let (notification_tx, mut notification_rx) = mpsc::channel(64);
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));

        for i in 0..5u32 {
            let notif = serde_json::json!({
                "jsonrpc": "2.0",
                "method": "agent_message_chunk",
                "params": {"sessionId": "sess-1", "delta": format!("chunk-{i}")}
            });
            route_message(notif, &pending, &notification_tx).await;
        }

        for i in 0..5u32 {
            let msg = notification_rx.recv().await.unwrap();
            if let IncomingMessage::Notification(n) = msg {
                let delta = n.params.unwrap()["delta"].as_str().unwrap().to_string();
                assert_eq!(delta, format!("chunk-{i}"));
            } else {
                panic!("expected Notification");
            }
        }
    }
}
