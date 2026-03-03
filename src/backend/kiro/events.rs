//! ACP notification -> AppMessage translation.
//!
//! The event loop in this module runs as a background task per kiro-cli session.
//! It receives [`IncomingMessage`] items from the transport's reader task,
//! translates them into [`AppMessage`] variants, and forwards them via `async_tx`.
//!
//! # Text accumulation
//!
//! ACP streams text as deltas (`agent_message_chunk`). This module accumulates
//! deltas into a full string and sends the accumulated text with every
//! [`AppMessage::StreamingUpdate`], matching the OpenCode backend behaviour.
//!
//! # Permission handling
//!
//! `session/request_permission` is a bidirectional JSON-RPC request: the agent
//! blocks until we reply. The event loop forwards a [`PermissionRequest`] to the
//! TUI via `async_tx`, then waits on a [`PermissionResponse`] channel.  Once the
//! user decides, the response is sent back via [`Transport::respond`].

use tokio::sync::mpsc;

use crate::messages::AppMessage;
use crate::opencode::types::{MessagePart, PermissionRequest};
use crate::tasks::models::TaskId;

use super::transport::Transport;
use super::types::{
    AcpPermissionKind, AgentMessageChunkParams, IncomingMessage, PermissionDecision,
    PermissionResult, RequestPermissionParams, SessionErrorParams, StopReason, ToolCallParams,
    ToolCallStatus, TurnEndParams,
};

/// A resolved permission decision from the TUI.
#[derive(Debug, Clone)]
pub struct PermissionResponse {
    /// The JSON-RPC request id of the `session/request_permission` we are answering.
    pub rpc_id: u64,
    /// The user's permission string: "once", "always", or "reject".
    pub decision: String,
}

impl PermissionResponse {
    /// Create a permission response from a raw decision string.
    pub fn new(rpc_id: u64, decision: impl Into<String>) -> Self {
        Self {
            rpc_id,
            decision: decision.into(),
        }
    }
}

/// Map a ClawdMux permission decision string to an ACP [`PermissionDecision`].
fn map_permission_decision(decision: &str) -> PermissionDecision {
    match decision {
        "always" => PermissionDecision::AllowAlways,
        "reject" => PermissionDecision::RejectOnce,
        _ => PermissionDecision::AllowOnce, // "once" and fallback
    }
}

/// Map ACP [`AcpPermissionKind`] to a ClawdMux permission type string for the TUI.
fn map_permission_kind(kind: &AcpPermissionKind) -> &'static str {
    match kind {
        AcpPermissionKind::FileRead => "bash",
        AcpPermissionKind::FileWrite => "bash",
        AcpPermissionKind::FileDelete => "bash",
        AcpPermissionKind::Execute => "bash",
        AcpPermissionKind::Network => "bash",
        AcpPermissionKind::Unknown => "bash",
    }
}

/// Run the ACP event loop for a single session.
///
/// Receives messages from the transport reader task via `incoming_rx`,
/// translates them to [`AppMessage`] variants, and forwards them on `async_tx`.
///
/// Permission requests are forwarded to the TUI and resolved via `permission_rx`.
///
/// # Arguments
/// * `task_id` – the task this session belongs to.
/// * `session_id` – the ACP session id (used to filter notifications).
/// * `transport` – transport handle for sending responses to bidirectional requests.
/// * `incoming_rx` – receives raw ACP messages from the reader task.
/// * `permission_rx` – receives resolved permission decisions from the TUI.
/// * `async_tx` – sends [`AppMessage`] variants to the application.
pub async fn run_event_loop(
    task_id: TaskId,
    session_id: String,
    transport: Transport,
    mut incoming_rx: mpsc::Receiver<IncomingMessage>,
    mut permission_rx: mpsc::Receiver<PermissionResponse>,
    async_tx: mpsc::Sender<AppMessage>,
) {
    let mut accumulated_text = String::new();

    loop {
        tokio::select! {
            msg = incoming_rx.recv() => {
                match msg {
                    None => {
                        tracing::debug!("kiro incoming channel closed for task {task_id}");
                        break;
                    }
                    Some(IncomingMessage::Notification(notif)) => {
                        let is_terminal = notif.method == "turn_end" || notif.method == "session/error";
                        handle_notification(
                            &notif.method,
                            notif.params.as_ref(),
                            &task_id,
                            &session_id,
                            &mut accumulated_text,
                            &async_tx,
                        ).await;
                        if is_terminal {
                            break;
                        }
                    }
                    Some(IncomingMessage::Request(req)) => {
                        // Bidirectional request from agent -- only permission requests expected
                        if req.method == "session/request_permission" {
                            handle_permission_request(
                                req.id,
                                req.params.as_ref(),
                                &task_id,
                                &session_id,
                                &transport,
                                &mut permission_rx,
                                &async_tx,
                            ).await;
                        } else {
                            tracing::warn!(
                                "unexpected bidirectional request from kiro: {}",
                                req.method
                            );
                            let _ = transport.respond_error(req.id, -32601, "Method not found").await;
                        }
                    }
                }
            }

            // Drain out-of-order permission resolutions to avoid blocking
            Some(perm) = permission_rx.recv() => {
                tracing::debug!(
                    "received out-of-order permission response for rpc_id={}, ignoring",
                    perm.rpc_id
                );
            }
        }
    }
}

/// Handle a single ACP notification, translating it to an [`AppMessage`].
pub(crate) async fn handle_notification(
    method: &str,
    params: Option<&serde_json::Value>,
    task_id: &TaskId,
    session_id: &str,
    accumulated_text: &mut String,
    async_tx: &mpsc::Sender<AppMessage>,
) {
    match method {
        "agent_message_chunk" => {
            let Some(params) = params else { return };
            let chunk: AgentMessageChunkParams = match serde_json::from_value(params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("failed to parse agent_message_chunk: {e}");
                    return;
                }
            };
            if chunk.session_id != session_id {
                return;
            }
            accumulated_text.push_str(&chunk.delta);
            let msg = AppMessage::StreamingUpdate {
                task_id: task_id.clone(),
                session_id: session_id.to_string(),
                message_id: session_id.to_string(),
                parts: vec![MessagePart::Text {
                    text: accumulated_text.clone(),
                }],
            };
            let _ = async_tx.send(msg).await;
        }

        "tool_call" | "tool_call_update" => {
            let Some(params) = params else { return };
            let tool: ToolCallParams = match serde_json::from_value(params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("failed to parse tool_call notification: {e}");
                    return;
                }
            };
            if tool.session_id != session_id {
                return;
            }
            let status_str = match tool.status {
                ToolCallStatus::Pending => "pending",
                ToolCallStatus::InProgress => "executing",
                ToolCallStatus::Completed => "completed",
                ToolCallStatus::Failed => "failed",
            };
            let detail = tool.input.as_ref().and_then(|v| {
                // Try to extract a useful summary from the tool input
                v.as_str()
                    .map(|s| s.to_string())
                    .or_else(|| {
                        v.get("path")
                            .and_then(|p| p.as_str())
                            .map(|s| s.to_string())
                    })
                    .or_else(|| {
                        v.get("command")
                            .and_then(|c| c.as_str())
                            .map(|s| s.to_string())
                    })
            });
            let msg = AppMessage::ToolActivity {
                task_id: task_id.clone(),
                session_id: session_id.to_string(),
                tool: tool.name.clone(),
                status: status_str.to_string(),
                detail,
            };
            let _ = async_tx.send(msg).await;
        }

        "turn_end" => {
            let Some(params) = params else {
                send_session_completed(task_id, session_id, accumulated_text.clone(), async_tx)
                    .await;
                return;
            };
            let turn: TurnEndParams = match serde_json::from_value(params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!("failed to parse turn_end: {e}");
                    send_session_completed(task_id, session_id, accumulated_text.clone(), async_tx)
                        .await;
                    return;
                }
            };
            if turn.session_id != session_id {
                return;
            }
            if turn.stop_reason == StopReason::Error || turn.stop_reason == StopReason::Cancelled {
                let msg = AppMessage::SessionError {
                    task_id: task_id.clone(),
                    session_id: session_id.to_string(),
                    error: format!("session ended with stop reason: {:?}", turn.stop_reason),
                };
                let _ = async_tx.send(msg).await;
            } else {
                send_session_completed(task_id, session_id, accumulated_text.clone(), async_tx)
                    .await;
            }
        }

        "session/error" => {
            let error_msg = if let Some(params) = params {
                match serde_json::from_value::<SessionErrorParams>(params.clone()) {
                    Ok(p) if p.session_id == session_id => p.error,
                    Ok(_) => return,
                    Err(_) => "unknown session error".to_string(),
                }
            } else {
                "unknown session error".to_string()
            };
            let msg = AppMessage::SessionError {
                task_id: task_id.clone(),
                session_id: session_id.to_string(),
                error: error_msg,
            };
            let _ = async_tx.send(msg).await;
        }

        _ => {
            tracing::debug!("kiro: unhandled notification method: {method}");
        }
    }
}

/// Handle a `session/request_permission` bidirectional request.
///
/// Forwards the permission request to the TUI, then blocks until the user resolves.
async fn handle_permission_request(
    rpc_id: u64,
    params: Option<&serde_json::Value>,
    task_id: &TaskId,
    session_id: &str,
    transport: &Transport,
    permission_rx: &mut mpsc::Receiver<PermissionResponse>,
    async_tx: &mpsc::Sender<AppMessage>,
) {
    let Some(params) = params else {
        let _ = transport
            .respond_error(rpc_id, -32602, "Missing params")
            .await;
        return;
    };

    let perm: RequestPermissionParams = match serde_json::from_value(params.clone()) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("failed to parse session/request_permission params: {e}");
            let _ = transport
                .respond_error(rpc_id, -32602, "Invalid params")
                .await;
            return;
        }
    };

    if perm.session_id != session_id {
        return;
    }

    let permission_kind = map_permission_kind(&perm.permission);
    let request = PermissionRequest {
        id: rpc_id.to_string(),
        session_id: session_id.to_string(),
        permission: permission_kind.to_string(),
        patterns: if perm.patterns.is_empty() && !perm.description.is_empty() {
            vec![perm.description.clone()]
        } else {
            perm.patterns.clone()
        },
        always: vec![],
    };

    let msg = AppMessage::PermissionAsked {
        task_id: task_id.clone(),
        request: request.clone(),
    };
    let _ = async_tx.send(msg).await;

    // Block until the user resolves the permission.
    // Drain other permission responses until we see one for our rpc_id.
    loop {
        match permission_rx.recv().await {
            None => {
                // Channel closed -- reject to unblock the agent
                tracing::warn!("permission channel closed while waiting for rpc_id={rpc_id}");
                let result = serde_json::to_value(PermissionResult {
                    decision: PermissionDecision::RejectOnce,
                })
                .unwrap_or(serde_json::json!({"decision": "reject_once"}));
                let _ = transport.respond(rpc_id, result).await;
                break;
            }
            Some(response) if response.rpc_id == rpc_id => {
                let acp_decision = map_permission_decision(&response.decision);
                let result = serde_json::to_value(PermissionResult {
                    decision: acp_decision,
                })
                .unwrap_or(serde_json::json!({"decision": "allow_once"}));
                let _ = transport.respond(rpc_id, result).await;
                break;
            }
            Some(other) => {
                // Response for a different rpc_id (shouldn't happen in practice)
                tracing::debug!(
                    "ignoring permission response for rpc_id={} while waiting for {}",
                    other.rpc_id,
                    rpc_id
                );
            }
        }
    }
}

/// Send a [`AppMessage::SessionCompleted`] with the accumulated response text.
async fn send_session_completed(
    task_id: &TaskId,
    session_id: &str,
    response_text: String,
    async_tx: &mpsc::Sender<AppMessage>,
) {
    let msg = AppMessage::SessionCompleted {
        task_id: task_id.clone(),
        session_id: session_id.to_string(),
        response_text,
    };
    let _ = async_tx.send(msg).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::AppMessage;
    use crate::tasks::models::TaskId;

    fn task_id() -> TaskId {
        TaskId::from_path("tasks/1.1.md")
    }

    #[tokio::test]
    async fn test_handle_notification_text_chunk_accumulates() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "agent_message_chunk",
            Some(&serde_json::json!({
                "sessionId": "sess-1",
                "delta": "Hello "
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        handle_notification(
            "agent_message_chunk",
            Some(&serde_json::json!({
                "sessionId": "sess-1",
                "delta": "world"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        assert_eq!(accumulated, "Hello world");

        let msg1 = rx.recv().await.unwrap();
        if let AppMessage::StreamingUpdate { parts, .. } = msg1 {
            if let MessagePart::Text { text } = &parts[0] {
                assert_eq!(text, "Hello ");
            } else {
                panic!("expected Text part");
            }
        } else {
            panic!("expected StreamingUpdate");
        }

        let msg2 = rx.recv().await.unwrap();
        if let AppMessage::StreamingUpdate { parts, .. } = msg2 {
            if let MessagePart::Text { text } = &parts[0] {
                assert_eq!(text, "Hello world");
            } else {
                panic!("expected Text part");
            }
        } else {
            panic!("expected StreamingUpdate");
        }
    }

    #[tokio::test]
    async fn test_handle_notification_text_chunk_wrong_session_ignored() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "agent_message_chunk",
            Some(&serde_json::json!({
                "sessionId": "other-session",
                "delta": "ignored"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        assert!(rx.try_recv().is_err());
        assert!(accumulated.is_empty());
    }

    #[tokio::test]
    async fn test_handle_notification_tool_call_in_progress() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "tool_call",
            Some(&serde_json::json!({
                "sessionId": "sess-1",
                "toolCallId": "tc-1",
                "name": "read_file",
                "status": "in_progress"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        let msg = rx.recv().await.unwrap();
        if let AppMessage::ToolActivity { tool, status, .. } = msg {
            assert_eq!(tool, "read_file");
            assert_eq!(status, "executing");
        } else {
            panic!("expected ToolActivity");
        }
    }

    #[tokio::test]
    async fn test_handle_notification_tool_call_completed() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "tool_call_update",
            Some(&serde_json::json!({
                "sessionId": "sess-1",
                "toolCallId": "tc-2",
                "name": "write_file",
                "status": "completed"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        let msg = rx.recv().await.unwrap();
        if let AppMessage::ToolActivity { status, .. } = msg {
            assert_eq!(status, "completed");
        } else {
            panic!("expected ToolActivity");
        }
    }

    #[tokio::test]
    async fn test_handle_notification_tool_call_with_path_detail() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "tool_call",
            Some(&serde_json::json!({
                "sessionId": "sess-1",
                "toolCallId": "tc-3",
                "name": "read_file",
                "status": "in_progress",
                "input": {"path": "src/main.rs"}
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        let msg = rx.recv().await.unwrap();
        if let AppMessage::ToolActivity { detail, .. } = msg {
            assert_eq!(detail, Some("src/main.rs".to_string()));
        } else {
            panic!("expected ToolActivity");
        }
    }

    #[tokio::test]
    async fn test_handle_notification_turn_end_sends_completed() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = "full response text".to_string();

        handle_notification(
            "turn_end",
            Some(&serde_json::json!({
                "sessionId": "sess-1",
                "stopReason": "end_turn"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        let msg = rx.recv().await.unwrap();
        if let AppMessage::SessionCompleted {
            response_text,
            session_id,
            task_id,
        } = msg
        {
            assert_eq!(response_text, "full response text");
            assert_eq!(session_id, "sess-1");
            assert_eq!(task_id, TaskId::from_path("tasks/1.1.md"));
        } else {
            panic!("expected SessionCompleted, got: {:?}", msg);
        }
    }

    #[tokio::test]
    async fn test_handle_notification_turn_end_error_sends_session_error() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "turn_end",
            Some(&serde_json::json!({
                "sessionId": "sess-1",
                "stopReason": "error"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, AppMessage::SessionError { .. }));
    }

    #[tokio::test]
    async fn test_handle_notification_turn_end_cancelled_sends_session_error() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "turn_end",
            Some(&serde_json::json!({
                "sessionId": "sess-1",
                "stopReason": "cancelled"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, AppMessage::SessionError { .. }));
    }

    #[tokio::test]
    async fn test_handle_notification_session_error() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "session/error",
            Some(&serde_json::json!({
                "sessionId": "sess-1",
                "error": "agent crashed"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        let msg = rx.recv().await.unwrap();
        if let AppMessage::SessionError { error, .. } = msg {
            assert_eq!(error, "agent crashed");
        } else {
            panic!("expected SessionError");
        }
    }

    #[tokio::test]
    async fn test_handle_notification_session_error_wrong_session_ignored() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "session/error",
            Some(&serde_json::json!({
                "sessionId": "other-sess",
                "error": "not for us"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_handle_notification_unknown_method_ignored() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = String::new();

        handle_notification(
            "some_unknown_notification",
            None,
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_map_permission_decision() {
        assert!(matches!(
            map_permission_decision("always"),
            PermissionDecision::AllowAlways
        ));
        assert!(matches!(
            map_permission_decision("reject"),
            PermissionDecision::RejectOnce
        ));
        assert!(matches!(
            map_permission_decision("once"),
            PermissionDecision::AllowOnce
        ));
        assert!(matches!(
            map_permission_decision("unknown"),
            PermissionDecision::AllowOnce
        ));
    }

    #[tokio::test]
    async fn test_turn_end_no_params_sends_completed() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = "some text".to_string();

        handle_notification(
            "turn_end",
            None,
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        let msg = rx.recv().await.unwrap();
        assert!(matches!(msg, AppMessage::SessionCompleted { .. }));
    }

    #[tokio::test]
    async fn test_turn_end_wrong_session_ignored() {
        let (tx, mut rx) = mpsc::channel(64);
        let mut accumulated = "text".to_string();

        handle_notification(
            "turn_end",
            Some(&serde_json::json!({
                "sessionId": "other-sess",
                "stopReason": "end_turn"
            })),
            &task_id(),
            "sess-1",
            &mut accumulated,
            &tx,
        )
        .await;

        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_permission_response_new() {
        let resp = PermissionResponse::new(42, "always");
        assert_eq!(resp.rpc_id, 42);
        assert_eq!(resp.decision, "always");
    }
}
