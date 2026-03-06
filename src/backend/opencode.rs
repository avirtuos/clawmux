//! OpenCode backend implementation.
//!
//! Wraps the [`OpenCodeClient`] HTTP API and implements [`AgentBackend`] by
//! moving the `tokio::spawn` blocks that previously lived in `App::handle_message`
//! into dedicated methods.  Each method internally spawns an async task and
//! routes results back to the event loop through `async_tx`.

use std::sync::Arc;

use tokio::sync::mpsc;

use super::AgentBackend;
use crate::messages::AppMessage;
use crate::opencode::events::SessionMap;
use crate::opencode::types::{ModelId, PermissionRequest};
use crate::opencode::OpenCodeClient;
use crate::tasks::models::TaskId;
use crate::workflow::agents::AgentKind;

/// Backend implementation backed by the OpenCode HTTP API.
///
/// Wraps an optional [`OpenCodeClient`]; when the client is `None` (server
/// unavailable) every operation that requires communication immediately sends
/// an appropriate error message via `async_tx`.
pub struct OpenCodeBackend {
    /// Shared HTTP client, absent when the OpenCode server is not running.
    client: Option<Arc<OpenCodeClient>>,
}

impl OpenCodeBackend {
    /// Creates a new `OpenCodeBackend` with the given client.
    ///
    /// Pass `None` when the OpenCode server is unavailable so the backend
    /// gracefully degrades rather than panicking.
    pub fn new(client: Option<Arc<OpenCodeClient>>) -> Self {
        Self { client }
    }
}

impl AgentBackend for OpenCodeBackend {
    fn create_session(
        &self,
        task_id: TaskId,
        agent: AgentKind,
        prompt: String,
        model: Option<ModelId>,
        session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let client = match self.client.clone() {
            Some(c) => c,
            None => {
                tokio::spawn(async move {
                    let _ = async_tx
                        .send(AppMessage::SessionError {
                            task_id,
                            session_id: String::new(),
                            error: "OpenCode client unavailable".to_string(),
                        })
                        .await;
                });
                return;
            }
        };
        tokio::spawn(async move {
            let session = match client.create_session().await {
                Ok(s) => s,
                Err(e) => {
                    let _ = async_tx
                        .send(AppMessage::SessionError {
                            task_id,
                            session_id: String::new(),
                            error: format!("Failed to create session: {e}"),
                        })
                        .await;
                    return;
                }
            };
            // Populate session map before sending SessionCreated to prevent TOCTOU
            // race with EventStreamConsumer.
            {
                let mut map = session_map.write().await;
                map.insert(session.id.clone(), (task_id.clone(), agent));
            }
            if let Err(e) = client
                .send_prompt_async(&session.id, Some(&agent), model.as_ref(), &prompt)
                .await
            {
                session_map.write().await.remove(&session.id);
                let _ = async_tx
                    .send(AppMessage::SessionError {
                        task_id,
                        session_id: session.id,
                        error: format!("Failed to send prompt: {e}"),
                    })
                    .await;
            } else {
                let _ = async_tx
                    .send(AppMessage::PromptSent {
                        task_id,
                        session_id: session.id,
                    })
                    .await;
            }
            // SessionCreated is sent by EventStreamConsumer when it sees the SSE
            // event; sending it again here would cause double-processing.
        });
    }

    fn create_idle_session(
        &self,
        task_id: TaskId,
        agent: AgentKind,
        session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let client = match self.client.clone() {
            Some(c) => c,
            None => return, // No client: silently skip.
        };
        tokio::spawn(async move {
            match client.create_session().await {
                Ok(new_session) => {
                    {
                        let mut map = session_map.write().await;
                        map.insert(new_session.id.clone(), (task_id.clone(), agent));
                    }
                    let _ = async_tx
                        .send(AppMessage::SessionCreated {
                            task_id,
                            session_id: new_session.id,
                        })
                        .await;
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to create replacement session after parse failure: {}",
                        e
                    );
                }
            }
        });
    }

    fn send_prompt(
        &self,
        task_id: TaskId,
        session_id: String,
        agent: AgentKind,
        prompt: String,
        model: Option<ModelId>,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let client = match self.client.clone() {
            Some(c) => c,
            None => {
                tokio::spawn(async move {
                    let _ = async_tx
                        .send(AppMessage::SessionError {
                            task_id,
                            session_id,
                            error: "OpenCode client unavailable".to_string(),
                        })
                        .await;
                });
                return;
            }
        };
        tokio::spawn(async move {
            if let Err(e) = client
                .send_prompt_async(&session_id, Some(&agent), model.as_ref(), &prompt)
                .await
            {
                let _ = async_tx
                    .send(AppMessage::SessionError {
                        task_id,
                        session_id,
                        error: format!("Failed to send prompt: {}", e),
                    })
                    .await;
            }
        });
    }

    fn abort_session(
        &self,
        task_id: TaskId,
        session_id: String,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let client = match self.client.clone() {
            Some(c) => c,
            None => {
                tokio::spawn(async move {
                    let _ = async_tx
                        .send(AppMessage::SessionError {
                            task_id,
                            session_id,
                            error: "OpenCode client unavailable".to_string(),
                        })
                        .await;
                });
                return;
            }
        };
        tokio::spawn(async move {
            if let Err(e) = client.abort_session(&session_id).await {
                let _ = async_tx
                    .send(AppMessage::SessionError {
                        task_id,
                        session_id,
                        error: format!("Failed to abort session: {}", e),
                    })
                    .await;
            }
        });
    }

    fn resolve_permission(
        &self,
        task_id: TaskId,
        request: PermissionRequest,
        response: String,
        send_prompt_msg: Option<AppMessage>,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let client = match self.client.clone() {
            Some(c) => c,
            None => {
                // No client (test/dev mode): dispatch steering prompt synchronously
                // by spawning it so the async_tx send does not block.
                if let Some(msg) = send_prompt_msg {
                    tokio::spawn(async move {
                        let _ = async_tx.send(msg).await;
                    });
                } else {
                    tracing::warn!("Cannot resolve permission: OpenCode client unavailable");
                }
                return;
            }
        };
        let session_id = request.session_id.clone();
        let permission_id = request.id.clone();
        tokio::spawn(async move {
            if let Err(e) = client
                .resolve_permission(&session_id, &permission_id, &response)
                .await
            {
                tracing::warn!("Failed to resolve permission {}: {}", permission_id, e);
                let _ = async_tx
                    .send(AppMessage::PermissionAsked {
                        task_id,
                        request: PermissionRequest {
                            id: permission_id,
                            session_id,
                            permission: "unknown".to_string(),
                            patterns: vec![],
                            always: vec![],
                        },
                    })
                    .await;
                return;
            }
            // Send the steering prompt after successful permission resolution.
            if let Some(msg) = send_prompt_msg {
                let _ = async_tx.send(msg).await;
            }
        });
    }

    fn get_diffs(&self, task_id: TaskId, session_id: String, async_tx: mpsc::Sender<AppMessage>) {
        let client = match self.client.clone() {
            Some(c) => c,
            None => return, // No diffs available without a client.
        };
        tokio::spawn(async move {
            match client.get_session_diffs(&session_id).await {
                Ok(diffs) => {
                    let _ = async_tx
                        .send(AppMessage::DiffReady { task_id, diffs })
                        .await;
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch diffs for session {}: {}", session_id, e);
                }
            }
        });
    }

    fn reply_question(
        &self,
        task_id: TaskId,
        request_id: String,
        answer: String,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let client = match self.client.clone() {
            Some(c) => c,
            None => return, // No client: silently skip.
        };
        tokio::spawn(async move {
            if let Err(e) = client.reply_question(&request_id, &answer).await {
                tracing::warn!("Failed to reply to OpenCode question {}: {}", request_id, e);
                let _ = async_tx
                    .send(AppMessage::SessionError {
                        task_id,
                        session_id: String::new(),
                        error: format!("Question reply failed: {}", e),
                    })
                    .await;
            }
        });
    }

    fn check_session_statuses(
        &self,
        sessions: Vec<(TaskId, String)>,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let client = match self.client.clone() {
            Some(c) => c,
            None => return, // No client: skip liveness polling.
        };
        tokio::spawn(async move {
            let statuses = match client.get_session_statuses().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!("Session status poll failed: {}", e);
                    return;
                }
            };
            for (task_id, session_id) in sessions {
                let is_idle = matches!(
                    statuses.get(&session_id),
                    Some(crate::opencode::types::SessionStatus::Idle) | None
                );
                if !is_idle {
                    continue;
                }
                // Session idle: fetch messages for error details.
                let error = match client.get_session_messages(&session_id).await {
                    Ok(messages) => messages
                        .iter()
                        .rev()
                        .find_map(|entry| {
                            if entry.info.role == crate::opencode::types::MessageRole::Assistant {
                                if let Some(ref err) = entry.info.error {
                                    return err.message.clone();
                                }
                                if entry.info.finish.as_deref() == Some("error") {
                                    return Some("Session finished with error status".to_string());
                                }
                            }
                            None
                        })
                        .unwrap_or_else(|| {
                            "Session was idle after prompt -- OpenCode may have crashed silently"
                                .to_string()
                        }),
                    Err(e) => {
                        format!(
                            "Session was idle after prompt (message fetch failed: {})",
                            e
                        )
                    }
                };
                let _ = async_tx
                    .send(AppMessage::VerifySessionIdle {
                        task_id,
                        session_id,
                        error,
                    })
                    .await;
            }
        });
    }

    fn commit_changes(
        &self,
        task_id: TaskId,
        commit_message: String,
        _file_paths: Vec<String>,
        _model: Option<ModelId>,
        _session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        tokio::spawn(async move {
            match super::run_git_commit(&commit_message).await {
                Ok(()) => {
                    let _ = async_tx.send(AppMessage::CommitCompleted { task_id }).await;
                }
                Err(e) => {
                    tracing::error!("opencode commit failed for task {task_id}: {e}");
                    let _ = async_tx
                        .send(AppMessage::CommitFailed {
                            task_id,
                            error: e.to_string(),
                        })
                        .await;
                }
            }
        });
    }

    fn is_available(&self) -> bool {
        self.client.is_some()
    }

    fn name(&self) -> &str {
        "opencode"
    }
}
