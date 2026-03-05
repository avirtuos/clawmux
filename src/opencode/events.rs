//! SSE event stream consumer.
//!
//! Connects to opencode's `GET /global/event` SSE stream, parses the 40+ event
//! types, and maps them to `AppMessage` values routed to the appropriate subsystem.
//! Runs as a long-lived tokio task.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest_eventsource::retry::ExponentialBackoff;
use reqwest_eventsource::{Event, EventSource};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::error::{ClawdMuxError, Result};
use crate::messages::AppMessage;
use crate::opencode::types::{MessagePart, OpenCodeEvent, PermissionRequest};
use crate::tasks::models::TaskId;
use crate::workflow::agents::AgentKind;

/// Maps session IDs to their associated task and agent.
///
/// Shared between the `EventStreamConsumer` and the workflow engine to correlate
/// opencode session events with ClawdMux tasks.
pub type SessionMap = Arc<RwLock<HashMap<String, (TaskId, AgentKind)>>>;

/// Consumes SSE events from the opencode server and routes them as `AppMessage` values.
///
/// Connects to `GET /global/event`, deserializes incoming events, and forwards
/// them through the provided `mpsc::Sender`. Runs as a long-lived tokio task with
/// automatic reconnection on stream termination.
#[allow(dead_code)]
pub struct EventStreamConsumer {
    tx: mpsc::Sender<AppMessage>,
    session_map: SessionMap,
    /// Accumulated assistant text per session ID, drained on session completion.
    accumulated_text: HashMap<String, String>,
    /// Last text seen per session that contains the `"action"` key.
    ///
    /// Unlike `accumulated_text` (which reflects the latest snapshot for streaming
    /// display and can be overwritten by empty user-message events), this is only
    /// updated when the text looks like an agent response (contains `"action"`).
    /// This prevents large user-message context prompts -- which arrive before the
    /// agent replies and can be far longer than the short JSON response -- from
    /// polluting the response captured at `SessionCompleted`.
    best_response_text: HashMap<String, String>,
    /// Per-(session, message) accumulated delta text for streaming display.
    ///
    /// Each delta is appended here so the full text can be emitted on every chunk.
    /// Drained when the session completes or errors.
    accumulated_deltas: HashMap<(String, String), String>,
    /// The ID of the most recently successfully-registered top-level session.
    ///
    /// Used as a fallback parent when OpenCode spawns a child session without
    /// including a parent reference in its `session.created` SSE event.
    last_registered_session: Option<String>,
}

#[allow(dead_code)]
impl EventStreamConsumer {
    /// Creates a new `EventStreamConsumer`.
    ///
    /// # Arguments
    ///
    /// * `tx` - Sender for routing `AppMessage` values to the main event loop.
    /// * `session_map` - Shared map from session ID to `(TaskId, AgentKind)`.
    pub fn new(tx: mpsc::Sender<AppMessage>, session_map: SessionMap) -> Self {
        Self {
            tx,
            session_map,
            accumulated_text: HashMap::new(),
            best_response_text: HashMap::new(),
            accumulated_deltas: HashMap::new(),
            last_registered_session: None,
        }
    }

    /// Connects to the opencode SSE stream and processes events indefinitely.
    ///
    /// The outer reconnection loop applies exponential backoff (1s initial, 2x factor,
    /// 30s cap) whenever the stream terminates. Backoff resets on a successful
    /// `Event::Open`. The library's built-in `ExponentialBackoff` policy handles
    /// transient per-request retries.
    ///
    /// # Arguments
    ///
    /// * `base_url` - Base URL of the opencode server (e.g. `"http://localhost:4242"`).
    ///
    /// # Errors
    ///
    /// This method is designed to run indefinitely and only returns if the
    /// underlying channel is closed (i.e. the main event loop has shut down).
    pub async fn run(&mut self, base_url: String) -> Result<()> {
        let url = format!("{}/global/event", base_url);
        let mut backoff_secs = 1u64;

        loop {
            let request = reqwest::Client::new().get(&url);
            let mut es = match EventSource::new(request) {
                Ok(es) => es,
                Err(e) => {
                    warn!("Failed to create EventSource: {}", e);
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(30);
                    continue;
                }
            };
            es.set_retry_policy(Box::new(ExponentialBackoff::new(
                Duration::from_secs(1),
                2.0,
                Some(Duration::from_secs(30)),
                None,
            )));

            while let Some(event) = es.next().await {
                match event {
                    Ok(Event::Open) => {
                        info!("SSE stream opened");
                        backoff_secs = 1;
                    }
                    Ok(Event::Message(msg)) => {
                        // opencode wraps all events in {"payload":{"type":"<name>","properties":{...}}}.
                        // The SSE event: field is always "message" and carries no type info.
                        tracing::trace!(
                            "SSE raw: event='{}', data_len={}",
                            msg.event,
                            msg.data.len()
                        );
                        let oc_event = parse_wire_event(&msg.data);
                        if let Err(e) = self.handle_event(oc_event).await {
                            warn!("Error handling SSE event: {e}");
                        }
                    }
                    Err(e) => {
                        warn!("SSE stream error: {}", e);
                    }
                }
            }

            info!("SSE stream terminated, reconnecting in {}s", backoff_secs);
            tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
            backoff_secs = (backoff_secs * 2).min(30);
        }
    }

    /// Maps an `OpenCodeEvent` to an `AppMessage` and forwards it through the channel.
    ///
    /// Looks up the session in the shared session map to resolve `task_id`.
    /// Events with unknown session IDs are silently ignored with a debug log.
    /// `MessageCreated` and `Unknown` variants are always ignored.
    pub(crate) async fn handle_event(&mut self, event: OpenCodeEvent) -> Result<()> {
        match event {
            OpenCodeEvent::SessionCreated {
                session_id,
                parent_id,
            } => {
                // Retry up to 3 times with a 50ms sleep to handle the TOCTOU
                // race where the SSE event arrives before the session map is
                // populated by the caller that initiated the CreateSession.
                const MAX_ATTEMPTS: usize = 3;
                let mut task_id = None;
                for attempt in 0..MAX_ATTEMPTS {
                    {
                        let map = self.session_map.read().await;
                        task_id = map.get(&session_id).map(|(id, _)| id.clone());
                    }
                    if task_id.is_some() {
                        break;
                    }
                    if attempt + 1 < MAX_ATTEMPTS {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
                // Remember this as the most recent top-level session so that
                // child sessions without a parent reference can inherit from it.
                if task_id.is_some() {
                    self.last_registered_session = Some(session_id.clone());
                }
                // If the session is not directly registered, check whether this is a
                // child session spawned by OpenCode from a known parent (e.g. for
                // parallel agents or sub-tasks). Inherit the parent's task mapping and
                // register the child so all its events reach the UI.
                if task_id.is_none() {
                    if let Some(ref pid) = parent_id {
                        let entry = {
                            let map = self.session_map.read().await;
                            map.get(pid.as_str()).cloned()
                        };
                        if let Some(entry) = entry {
                            debug!(
                                "SSE session.created: child session {} inherits from parent {}",
                                session_id, pid
                            );
                            task_id = Some(entry.0.clone());
                            let mut map = self.session_map.write().await;
                            map.insert(session_id.clone(), entry);
                        }
                    }
                }
                // Last resort: OpenCode sometimes spawns child sessions without any
                // parent reference in the event. Fall back to the most recently
                // registered top-level session so the child's events reach the UI.
                if task_id.is_none() {
                    if let Some(ref last) = self.last_registered_session.clone() {
                        let entry = {
                            let map = self.session_map.read().await;
                            map.get(last.as_str()).cloned()
                        };
                        if let Some(entry) = entry {
                            debug!(
                                "SSE session.created: adopting unknown session {} under last session {}",
                                session_id, last
                            );
                            task_id = Some(entry.0.clone());
                            let mut map = self.session_map.write().await;
                            map.insert(session_id.clone(), entry);
                        }
                    }
                }
                if let Some(task_id) = task_id {
                    self.send(AppMessage::SessionCreated {
                        task_id,
                        session_id,
                    })
                    .await?;
                } else {
                    warn!(
                        "SessionCreated for unknown session_id after retries: {} (parent={:?})",
                        session_id, parent_id
                    );
                }
            }
            OpenCodeEvent::MessageUpdated {
                session_id,
                message_id,
                parts,
            } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    // Accumulate the latest Text part for this session.
                    if let Some(text) = parts.iter().rev().find_map(|p| match p {
                        MessagePart::Text { text } => Some(text.clone()),
                        _ => None,
                    }) {
                        self.accumulated_text
                            .insert(session_id.clone(), text.clone());
                        // Only track in best_response_text when the text looks like an
                        // agent response. User-message context prompts can be much longer
                        // than the agent's short JSON reply, so a length-based heuristic
                        // would pick the wrong text. Checking for "action" ensures we
                        // only store genuine agent responses here.
                        if text.contains("\"action\"") {
                            self.best_response_text.insert(session_id.clone(), text);
                        }
                    }
                    self.send(AppMessage::StreamingUpdate {
                        task_id,
                        session_id,
                        message_id,
                        parts,
                    })
                    .await?;
                } else {
                    debug!("MessageUpdated for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::MessagePartDelta {
                session_id,
                message_id,
                field,
                delta,
            } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    if field == "text" {
                        let full_text = {
                            let entry = self
                                .accumulated_deltas
                                .entry((session_id.clone(), message_id.clone()))
                                .or_default();
                            entry.push_str(&delta);
                            entry.clone()
                        };
                        self.accumulated_text
                            .insert(session_id.clone(), full_text.clone());
                        if full_text.contains("\"action\"") {
                            self.best_response_text
                                .insert(session_id.clone(), full_text.clone());
                        }
                        self.send(AppMessage::StreamingUpdate {
                            task_id,
                            session_id,
                            message_id,
                            parts: vec![MessagePart::Text { text: full_text }],
                        })
                        .await?;
                    }
                } else {
                    debug!("MessagePartDelta for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::ToolExecuting {
                session_id,
                tool,
                detail,
            } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::ToolActivity {
                        task_id,
                        session_id,
                        tool,
                        status: "executing".to_string(),
                        detail,
                    })
                    .await?;
                } else {
                    debug!("ToolExecuting for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::ToolCompleted {
                session_id,
                tool,
                detail,
                ..
            } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::ToolActivity {
                        task_id,
                        session_id,
                        tool,
                        status: "completed".to_string(),
                        detail,
                    })
                    .await?;
                } else {
                    debug!("ToolCompleted for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::ToolPending {
                session_id,
                tool,
                detail,
            } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::ToolActivity {
                        task_id,
                        session_id,
                        tool,
                        status: "pending".to_string(),
                        detail,
                    })
                    .await?;
                } else {
                    debug!("ToolPending for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::SessionCompleted { session_id } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    let response_text = self
                        .best_response_text
                        .remove(&session_id)
                        .or_else(|| self.accumulated_text.remove(&session_id))
                        .unwrap_or_default();
                    // Also clean up accumulated_text (may still have a stale entry).
                    self.accumulated_text.remove(&session_id);
                    self.accumulated_deltas
                        .retain(|(sid, _), _| *sid != session_id);
                    // Clone before move into message so we can remove from the map after send.
                    let sid_for_cleanup = session_id.clone();
                    self.send(AppMessage::SessionCompleted {
                        task_id,
                        session_id,
                        response_text,
                    })
                    .await?;
                    // Remove the session so child-session completions that arrive later
                    // are not mistaken for the primary session.
                    self.session_map.write().await.remove(&sid_for_cleanup);
                } else {
                    debug!("SessionCompleted for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::SessionError { session_id, error } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                // Clean up accumulated text and deltas to prevent memory leaks.
                self.best_response_text.remove(&session_id);
                self.accumulated_text.remove(&session_id);
                self.accumulated_deltas
                    .retain(|(sid, _), _| *sid != session_id);
                if let Some(task_id) = task_id {
                    // Clone before move into message so we can remove from the map after send.
                    let sid_for_cleanup = session_id.clone();
                    self.send(AppMessage::SessionError {
                        task_id,
                        session_id,
                        error,
                    })
                    .await?;
                    // Remove the session so stale events after the error are dropped.
                    self.session_map.write().await.remove(&sid_for_cleanup);
                } else {
                    debug!("SessionError for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::PermissionAsked {
                session_id,
                request,
            } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::PermissionAsked { task_id, request })
                        .await?;
                } else {
                    debug!("PermissionAsked for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::QuestionAsked {
                session_id,
                request_id,
                question,
            } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::OpenCodeQuestionAsked {
                        task_id,
                        request_id,
                        question,
                    })
                    .await?;
                } else {
                    debug!("QuestionAsked for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::SessionDiff { session_id } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::SessionDiffChanged {
                        task_id,
                        session_id,
                    })
                    .await?;
                } else {
                    debug!("SessionDiff for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::TokensUpdated {
                session_id,
                input_tokens,
                output_tokens,
                is_cumulative,
                step_id,
            } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::TokensUpdated {
                        task_id,
                        input_tokens,
                        output_tokens,
                        is_cumulative,
                        step_id,
                    })
                    .await?;
                } else {
                    debug!("TokensUpdated for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::MessageCreated { .. } => {
                // Ignored -- redundant with MessageUpdated
            }
            OpenCodeEvent::Unknown => {
                // Logging is handled inside parse_wire_event at the appropriate level.
            }
        }
        Ok(())
    }

    /// Sends an `AppMessage` through the channel.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Sse`] if the receiver has been dropped.
    async fn send(&self, msg: AppMessage) -> Result<()> {
        self.tx
            .send(msg)
            .await
            .map_err(|e| ClawdMuxError::Sse(e.to_string()))
    }
}

/// Extracts a concise display string from a tool's `input` JSON object.
/// Truncates `s` to at most `max_chars` Unicode scalar values, appending `"..."`
/// when truncation occurs. Avoids panics on multi-byte UTF-8 characters.
fn truncate_str(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}...", &s[..idx]),
        None => s.to_string(),
    }
}

///
/// Returns the single most relevant value (file path, command, pattern, URL, etc.)
/// for the given `tool` name, or `None` if no useful detail can be found.
/// Long values are truncated to 80 characters.
fn extract_tool_detail(tool: &str, input: &serde_json::Value) -> Option<String> {
    let obj = input.as_object()?;
    let raw: Option<&str> = match tool.to_lowercase().as_str() {
        "read" => obj
            .get("file_path")
            .or_else(|| obj.get("path"))
            .and_then(|v| v.as_str()),
        "glob" => obj.get("pattern").and_then(|v| v.as_str()),
        "write" | "edit" | "multiedit" | "notebookedit" => obj
            .get("file_path")
            .or_else(|| obj.get("path"))
            .and_then(|v| v.as_str()),
        "bash" | "execute" => obj
            .get("command")
            .or_else(|| obj.get("cmd"))
            .and_then(|v| v.as_str()),
        "webfetch" | "web_fetch" => obj.get("url").and_then(|v| v.as_str()),
        "task" | "agent" => obj.get("description").and_then(|v| v.as_str()),
        _ => obj.values().find_map(|v| v.as_str()),
    };
    raw.filter(|s| !s.is_empty()).map(|s| truncate_str(s, 80))
}

/// Parses an opencode SSE JSON body into an [`OpenCodeEvent`].
///
/// The opencode wire format wraps all events in:
/// `{"payload": {"type": "<event.name>", "properties": {...}}, ...}`
///
/// The SSE protocol `event:` field is always `"message"` and carries no type information.
/// The actual event type is always found at `payload.type`.
///
/// Logging policy:
/// - Known-but-unneeded events (heartbeats, metadata updates): `debug!`
/// - Recognized events with missing required fields: `warn!`
/// - Unrecognized event types: `info!`
/// - JSON parse failures: `warn!`
fn parse_wire_event(json_data: &str) -> OpenCodeEvent {
    let v: serde_json::Value = match serde_json::from_str(json_data) {
        Ok(v) => v,
        Err(e) => {
            warn!("Failed to parse SSE JSON: {}; data: {}", e, json_data);
            return OpenCodeEvent::Unknown;
        }
    };

    let payload = match v.get("payload") {
        Some(p) => p,
        None => {
            warn!("SSE event missing 'payload' field: {}", json_data);
            return OpenCodeEvent::Unknown;
        }
    };

    let event_type = match payload.get("type").and_then(|t| t.as_str()) {
        Some(t) => t,
        None => {
            debug!("SSE payload missing 'type' field: {}", json_data);
            return OpenCodeEvent::Unknown;
        }
    };

    let props = payload
        .get("properties")
        .unwrap_or(&serde_json::Value::Null);

    match event_type {
        "session.created" => {
            if let Some(id) = props["info"]["id"].as_str() {
                let parent_id = props["info"]["parent"]
                    .as_str()
                    .or_else(|| props["info"]["parentId"].as_str())
                    .map(|s| s.to_string());
                // Log full info object so parent field name can be identified if
                // OpenCode uses a different key than "parent"/"parentId".
                debug!(
                    "SSE session.created: session_id={}, parent={:?}, info={}",
                    id, parent_id, props["info"]
                );
                OpenCodeEvent::SessionCreated {
                    session_id: id.to_string(),
                    parent_id,
                }
            } else {
                warn!("session.created missing info.id: {}", json_data);
                OpenCodeEvent::Unknown
            }
        }
        "session.error" => {
            let session_id = props["info"]["id"]
                .as_str()
                .or_else(|| props["sessionID"].as_str())
                .or_else(|| props["sessionId"].as_str());
            let error = props["error"]
                .as_str()
                .or_else(|| props["error"]["data"]["message"].as_str())
                .or_else(|| props["error"]["message"].as_str())
                .unwrap_or("unknown error");
            match session_id {
                Some(sid) => OpenCodeEvent::SessionError {
                    session_id: sid.to_string(),
                    error: error.to_string(),
                },
                None => {
                    warn!("session.error missing session id: {}", json_data);
                    OpenCodeEvent::Unknown
                }
            }
        }
        "session.completed" => {
            let session_id = props["info"]["id"]
                .as_str()
                .or_else(|| props["sessionID"].as_str())
                .or_else(|| props["sessionId"].as_str());
            match session_id {
                Some(sid) => OpenCodeEvent::SessionCompleted {
                    session_id: sid.to_string(),
                },
                None => {
                    warn!("session.completed missing session id: {}", json_data);
                    OpenCodeEvent::Unknown
                }
            }
        }
        // message.part.delta carries an incremental text delta (OpenCode >= 1.2).
        "message.part.delta" => {
            let session_id = props["sessionID"].as_str();
            let message_id = props["messageID"].as_str();
            let field = props["field"].as_str();
            let delta = props["delta"].as_str();
            match (session_id, message_id, field, delta) {
                (Some(sid), Some(mid), Some(fld), Some(dlt)) => OpenCodeEvent::MessagePartDelta {
                    session_id: sid.to_string(),
                    message_id: mid.to_string(),
                    field: fld.to_string(),
                    delta: dlt.to_string(),
                },
                _ => {
                    warn!(
                        "message.part.delta: missing required fields; props: {}",
                        props
                    );
                    OpenCodeEvent::Unknown
                }
            }
        }
        // session.idle is the new completion signal (replaces session.completed in OpenCode >= 1.2).
        "session.idle" => {
            let session_id = props["sessionID"]
                .as_str()
                .or_else(|| props["sessionId"].as_str());
            match session_id {
                Some(sid) => OpenCodeEvent::SessionCompleted {
                    session_id: sid.to_string(),
                },
                None => {
                    warn!("session.idle missing session id: {}", json_data);
                    OpenCodeEvent::Unknown
                }
            }
        }
        "permission.asked" => {
            let id = props["id"].as_str();
            let session_id = props["sessionID"]
                .as_str()
                .or_else(|| props["sessionId"].as_str());
            let permission = props["permission"].as_str().unwrap_or("unknown");
            let patterns: Vec<String> = props["patterns"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            let always: Vec<String> = props["always"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect()
                })
                .unwrap_or_default();
            match (id, session_id) {
                (Some(id), Some(sid)) => OpenCodeEvent::PermissionAsked {
                    session_id: sid.to_string(),
                    request: PermissionRequest {
                        id: id.to_string(),
                        session_id: sid.to_string(),
                        permission: permission.to_string(),
                        patterns,
                        always,
                    },
                },
                _ => {
                    warn!("permission.asked missing required fields; props: {}", props);
                    OpenCodeEvent::Unknown
                }
            }
        }
        "question.asked" => {
            let request_id = props["id"]
                .as_str()
                .or_else(|| props["requestID"].as_str())
                .or_else(|| props["requestId"].as_str());
            let session_id = props["sessionID"]
                .as_str()
                .or_else(|| props["sessionId"].as_str());
            // Question text may be in properties.question, properties.text, or properties.message.
            let question = props["question"]
                .as_str()
                .or_else(|| props["text"].as_str())
                .or_else(|| props["message"].as_str());
            match (request_id, session_id, question) {
                (Some(rid), Some(sid), Some(q)) => OpenCodeEvent::QuestionAsked {
                    session_id: sid.to_string(),
                    request_id: rid.to_string(),
                    question: q.to_string(),
                },
                _ => {
                    warn!("question.asked missing required fields; props: {}", props);
                    OpenCodeEvent::Unknown
                }
            }
        }
        "session.diff" => {
            let session_id = props["sessionID"]
                .as_str()
                .or_else(|| props["sessionId"].as_str());
            match session_id {
                Some(sid) => {
                    debug!("SSE session.diff: session_id={}", sid);
                    OpenCodeEvent::SessionDiff {
                        session_id: sid.to_string(),
                    }
                }
                None => {
                    warn!("session.diff missing session id: {}", json_data);
                    OpenCodeEvent::Unknown
                }
            }
        }
        // message.part.updated carries tool state (pending/running/completed) and
        // text snapshots. Text parts carry the agent's accumulated response and must
        // be forwarded so accumulated_text is populated for SessionCompleted.
        // message.part.delta is the preferred streaming path but OpenCode also sends
        // full-text snapshots via part.updated; both paths must be handled.
        "message.part.updated" => {
            if let Some(part) = props.get("part") {
                if part.get("type").and_then(|t| t.as_str()) == Some("tool") {
                    let session_id = part["sessionID"]
                        .as_str()
                        .or_else(|| part["sessionId"].as_str());
                    let tool = part["tool"].as_str().unwrap_or("unknown");
                    let status = part["state"]["status"].as_str().unwrap_or("unknown");
                    let detail = extract_tool_detail(tool, &part["input"]);
                    if let Some(sid) = session_id {
                        return match status {
                            "running" => OpenCodeEvent::ToolExecuting {
                                session_id: sid.to_string(),
                                tool: tool.to_string(),
                                detail,
                            },
                            "completed" => OpenCodeEvent::ToolCompleted {
                                session_id: sid.to_string(),
                                tool: tool.to_string(),
                                result: String::new(),
                                detail,
                            },
                            _ => OpenCodeEvent::ToolPending {
                                session_id: sid.to_string(),
                                tool: tool.to_string(),
                                detail,
                            },
                        };
                    }
                    warn!(
                        "message.part.updated tool part missing session id: {}",
                        json_data
                    );
                    return OpenCodeEvent::Unknown;
                }
                if part.get("type").and_then(|t| t.as_str()) == Some("step-finish") {
                    let session_id = part["sessionID"]
                        .as_str()
                        .or_else(|| part["sessionId"].as_str());
                    let input = part["tokens"]["input"].as_u64();
                    let output = part["tokens"]["output"].as_u64();
                    if let (Some(sid), Some(inp), Some(out)) = (session_id, input, output) {
                        if inp > 0 || out > 0 {
                            let step_id = part["id"].as_str().map(str::to_string);
                            return OpenCodeEvent::TokensUpdated {
                                session_id: sid.to_string(),
                                input_tokens: inp,
                                output_tokens: out,
                                is_cumulative: false,
                                step_id,
                            };
                        }
                    }
                }
                // text parts carry the agent's full response snapshot and must be
                // accumulated for SessionCompleted. message.part.delta is the preferred
                // streaming path, but OpenCode also delivers text via part.updated.
                if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                    let session_id = part["sessionID"]
                        .as_str()
                        .or_else(|| part["sessionId"].as_str());
                    let text = part["text"].as_str();
                    if let (Some(sid), Some(txt)) = (session_id, text) {
                        let message_id = part["messageID"]
                            .as_str()
                            .or_else(|| part["messageId"].as_str())
                            .unwrap_or("")
                            .to_string();
                        return OpenCodeEvent::MessageUpdated {
                            session_id: sid.to_string(),
                            message_id,
                            parts: vec![MessagePart::Text {
                                text: txt.to_string(),
                            }],
                        };
                    }
                }
            }
            debug!("SSE event '{}': ignoring (props: {})", event_type, props);
            OpenCodeEvent::Unknown
        }
        // message.updated: extract text from the message body when present, then extract
        // token usage. In some OpenCode versions (observed in production) the final
        // assistant response text arrives exclusively via message.updated (props["parts"])
        // rather than via message.part.updated or message.part.delta. Text extraction is
        // prioritised: when a text part is found we return MessageUpdated immediately so
        // accumulated_text is populated for SessionCompleted. Token extraction is the
        // fallback when no text parts are present.
        //
        // OpenCode sends two token path layouts depending on message role:
        //   - assistant: info.tokens.{input,output}
        //   - user:      info.summary.tokens.{input,output}  (fallback, not always present)
        //
        // Initial creation events carry all-zero counts and are skipped to avoid
        // showing "in:0 out:0" before the model has actually processed anything.
        "message.updated" => {
            let session_id = props["sessionId"]
                .as_str()
                .or_else(|| props["sessionID"].as_str());
            // Check for text content in the message parts first.
            if let Some(sid) = session_id {
                if let Some(parts) = props["parts"].as_array() {
                    if let Some(text) = parts.iter().find_map(|p| {
                        if p["type"].as_str() == Some("text") {
                            p["text"].as_str().map(|t| t.to_string())
                        } else {
                            None
                        }
                    }) {
                        let message_id = props["messageId"]
                            .as_str()
                            .or_else(|| props["messageID"].as_str())
                            .unwrap_or("")
                            .to_string();
                        return OpenCodeEvent::MessageUpdated {
                            session_id: sid.to_string(),
                            message_id,
                            parts: vec![MessagePart::Text { text }],
                        };
                    }
                }
            }
            let input = props["info"]["tokens"]["input"]
                .as_u64()
                .or_else(|| props["info"]["summary"]["tokens"]["input"].as_u64());
            let output = props["info"]["tokens"]["output"]
                .as_u64()
                .or_else(|| props["info"]["summary"]["tokens"]["output"].as_u64());
            if let (Some(sid), Some(inp), Some(out)) = (session_id, input, output) {
                if inp > 0 || out > 0 {
                    return OpenCodeEvent::TokensUpdated {
                        session_id: sid.to_string(),
                        input_tokens: inp,
                        output_tokens: out,
                        is_cumulative: true,
                        step_id: None,
                    };
                }
            }
            debug!("SSE event 'message.updated': ignoring (no token data in props)");
            OpenCodeEvent::Unknown
        }
        // session.status with type=idle is the primary session-completion signal in
        // OpenCode >= 1.2.11. Other status values (e.g. "busy") are ignored.
        "session.status" => {
            let session_id = props["sessionID"]
                .as_str()
                .or_else(|| props["sessionId"].as_str());
            if props["status"]["type"].as_str() == Some("idle") {
                match session_id {
                    Some(sid) => OpenCodeEvent::SessionCompleted {
                        session_id: sid.to_string(),
                    },
                    None => {
                        warn!("session.status idle missing session id: {}", json_data);
                        OpenCodeEvent::Unknown
                    }
                }
            } else {
                debug!(
                    "SSE event 'session.status': non-idle status, ignoring (props: {})",
                    props
                );
                OpenCodeEvent::Unknown
            }
        }
        // Known events we intentionally do not act on.
        "session.updated" | "server.heartbeat" | "server.connected" | "project.updated"
        | "message.created" | "permission.replied" => {
            debug!("SSE event '{}': ignoring (props: {})", event_type, props);
            OpenCodeEvent::Unknown
        }
        _ => {
            warn!("Unhandled SSE event type '{}': {}", event_type, props);
            OpenCodeEvent::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opencode::types::{MessageRole, OpenCodeMessage};
    use tokio::sync::mpsc;

    fn make_consumer() -> (EventStreamConsumer, mpsc::Receiver<AppMessage>, SessionMap) {
        let (tx, rx) = mpsc::channel(32);
        let session_map: SessionMap = Arc::new(RwLock::new(HashMap::new()));
        let consumer = EventStreamConsumer::new(tx, Arc::clone(&session_map));
        (consumer, rx, session_map)
    }

    fn make_text_parts(text: &str) -> Vec<crate::opencode::types::MessagePart> {
        vec![crate::opencode::types::MessagePart::Text {
            text: text.to_string(),
        }]
    }

    #[tokio::test]
    async fn test_event_routing_known_session() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-abc".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        // MessageUpdated -- accumulates response_text for SessionCompleted.
        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-abc".to_string(),
                message_id: "msg-0".to_string(),
                parts: make_text_parts("agent response text"),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("StreamingUpdate message");

        // ToolExecuting
        consumer
            .handle_event(OpenCodeEvent::ToolExecuting {
                session_id: "sess-abc".to_string(),
                tool: "bash".to_string(),
                detail: None,
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("ToolActivity executing message");
        assert!(
            matches!(msg, AppMessage::ToolActivity { ref status, .. } if status == "executing")
        );

        // ToolCompleted
        consumer
            .handle_event(OpenCodeEvent::ToolCompleted {
                session_id: "sess-abc".to_string(),
                tool: "bash".to_string(),
                result: "ok".to_string(),
                detail: None,
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("ToolActivity completed message");
        assert!(
            matches!(msg, AppMessage::ToolActivity { ref status, .. } if status == "completed")
        );

        // SessionCreated -- must come before SessionCompleted because SessionCompleted
        // removes the session from the map and subsequent events would not route.
        consumer
            .handle_event(OpenCodeEvent::SessionCreated {
                session_id: "sess-abc".to_string(),
                parent_id: None,
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("SessionCreated message");
        assert!(
            matches!(msg, AppMessage::SessionCreated { ref session_id, .. } if session_id == "sess-abc")
        );

        // SessionCompleted -- last event; removes the session from the map.
        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-abc".to_string(),
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("SessionCompleted message");
        assert!(
            matches!(&msg, AppMessage::SessionCompleted { ref session_id, ref response_text, .. }
                if session_id == "sess-abc" && response_text == "agent response text")
        );
    }

    #[tokio::test]
    async fn test_event_routing_unknown_session() {
        let (mut consumer, mut rx, _session_map) = make_consumer();
        // No sessions in the map -- events for unknown sessions are silently ignored.

        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "unknown-sess".to_string(),
            })
            .await
            .expect("handle_event");
        assert!(
            rx.try_recv().is_err(),
            "no message expected for unknown session"
        );

        consumer
            .handle_event(OpenCodeEvent::ToolExecuting {
                session_id: "unknown-sess".to_string(),
                tool: "bash".to_string(),
                detail: None,
            })
            .await
            .expect("handle_event");
        assert!(
            rx.try_recv().is_err(),
            "no message expected for unknown session"
        );

        // MessageCreated is always ignored regardless of session presence.
        consumer
            .handle_event(OpenCodeEvent::MessageCreated {
                session_id: "unknown-sess".to_string(),
                message: OpenCodeMessage {
                    id: "m1".to_string(),
                    role: MessageRole::User,
                    parts: vec![],
                },
            })
            .await
            .expect("handle_event");
        assert!(
            rx.try_recv().is_err(),
            "MessageCreated should always be ignored"
        );

        // Unknown is always ignored.
        consumer
            .handle_event(OpenCodeEvent::Unknown)
            .await
            .expect("handle_event");
        assert!(rx.try_recv().is_err(), "Unknown should always be ignored");
    }

    /// Verifies that `SessionCompleted` removes the session from the map so that
    /// subsequent events for child sessions are not routed.
    #[tokio::test]
    async fn test_session_map_cleanup_on_completed() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-done".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-done".to_string(),
            })
            .await
            .expect("handle_event");
        // Drain the SessionCompleted message.
        let _ = rx.try_recv().expect("SessionCompleted message");

        // Session must be removed from the map.
        let map = session_map.read().await;
        assert!(
            !map.contains_key("sess-done"),
            "session should be removed from session_map after SessionCompleted"
        );
    }

    /// Verifies that `SessionError` removes the session from the map so that
    /// subsequent events for child sessions are not routed.
    #[tokio::test]
    async fn test_session_map_cleanup_on_error() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-err".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        consumer
            .handle_event(OpenCodeEvent::SessionError {
                session_id: "sess-err".to_string(),
                error: "something went wrong".to_string(),
            })
            .await
            .expect("handle_event");
        // Drain the SessionError message.
        let _ = rx.try_recv().expect("SessionError message");

        // Session must be removed from the map.
        let map = session_map.read().await;
        assert!(
            !map.contains_key("sess-err"),
            "session should be removed from session_map after SessionError"
        );
    }

    /// Verifies that `SessionCreated` succeeds when the session map is populated
    /// after a short delay (simulating the TOCTOU race condition).
    #[tokio::test]
    async fn test_session_created_retry_succeeds() {
        // Pause the tokio clock so timers resolve in deterministic order
        // regardless of real wall-clock timing under CI load.
        tokio::time::pause();

        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/6.1.md");
        let session_id = "sess-race".to_string();

        // Spawn a task that inserts into the session map after 25ms,
        // simulating the caller populating the map slightly after the SSE event arrives.
        // With the clock paused, the 25ms sleep resolves before the 50ms retry
        // sleep in handle_event, guaranteeing the map is populated on attempt 2.
        let map_clone = Arc::clone(&session_map);
        let tid_clone = task_id.clone();
        let sid_clone = session_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(25)).await;
            let mut map = map_clone.write().await;
            map.insert(sid_clone, (tid_clone, AgentKind::Intake));
        });

        consumer
            .handle_event(OpenCodeEvent::SessionCreated {
                session_id: session_id.clone(),
                parent_id: None,
            })
            .await
            .expect("handle_event");

        let msg = rx
            .try_recv()
            .expect("SessionCreated message should be received after retry");
        assert!(
            matches!(msg, AppMessage::SessionCreated { ref session_id, .. } if session_id == "sess-race")
        );
    }

    /// Verifies that a child session with a known parent inherits the parent's task mapping.
    #[tokio::test]
    async fn test_session_created_child_inherits_parent_mapping() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-parent".to_string(),
                (task_id.clone(), AgentKind::Intake),
            );
        }

        // A child session with parent_id pointing to a registered parent should
        // inherit the parent's task mapping and emit SessionCreated.
        consumer
            .handle_event(OpenCodeEvent::SessionCreated {
                session_id: "sess-child".to_string(),
                parent_id: Some("sess-parent".to_string()),
            })
            .await
            .expect("handle_event");

        // The child session should now be registered in the session map.
        {
            let map = session_map.read().await;
            assert!(
                map.contains_key("sess-child"),
                "child session should be registered in session_map"
            );
        }

        // A SessionCreated message should be emitted for the child session.
        let msg = rx
            .try_recv()
            .expect("SessionCreated message should be emitted for child session");
        assert!(
            matches!(msg, AppMessage::SessionCreated { ref session_id, .. } if session_id == "sess-child"),
            "unexpected: {msg:?}"
        );
    }

    /// Verifies that a child session with an unknown parent is still dropped.
    #[tokio::test]
    async fn test_session_created_child_unknown_parent_dropped() {
        let (mut consumer, mut rx, _session_map) = make_consumer();

        consumer
            .handle_event(OpenCodeEvent::SessionCreated {
                session_id: "sess-child".to_string(),
                parent_id: Some("sess-unknown-parent".to_string()),
            })
            .await
            .expect("handle_event");

        assert!(
            rx.try_recv().is_err(),
            "child of unknown parent should not emit a message"
        );
    }

    /// Verifies that when OpenCode spawns a child session with no parent reference,
    /// it is adopted under the most recently registered top-level session.
    #[tokio::test]
    async fn test_session_created_no_parent_adopts_last_registered() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-toplevel".to_string(),
                (task_id.clone(), AgentKind::Intake),
            );
        }

        // Register the top-level session so last_registered_session is set.
        consumer
            .handle_event(OpenCodeEvent::SessionCreated {
                session_id: "sess-toplevel".to_string(),
                parent_id: None,
            })
            .await
            .expect("handle_event");
        // Drain the SessionCreated message for the top-level session.
        rx.try_recv().expect("top-level SessionCreated");

        // Now a child session fires with no parent reference at all.
        consumer
            .handle_event(OpenCodeEvent::SessionCreated {
                session_id: "sess-orphan".to_string(),
                parent_id: None,
            })
            .await
            .expect("handle_event");

        // The orphan should have been adopted under the top-level session's task.
        {
            let map = session_map.read().await;
            assert!(
                map.contains_key("sess-orphan"),
                "orphan session should be adopted into session_map"
            );
        }
        let msg = rx
            .try_recv()
            .expect("SessionCreated message should be emitted for adopted orphan");
        assert!(
            matches!(msg, AppMessage::SessionCreated { ref session_id, .. } if session_id == "sess-orphan"),
            "unexpected: {msg:?}"
        );
    }

    /// Verifies that a no-parent session is still dropped when there is no
    /// previously registered session to adopt it.
    #[tokio::test]
    async fn test_session_created_no_parent_no_fallback_dropped() {
        let (mut consumer, mut rx, _session_map) = make_consumer();

        consumer
            .handle_event(OpenCodeEvent::SessionCreated {
                session_id: "sess-orphan".to_string(),
                parent_id: None,
            })
            .await
            .expect("handle_event");

        assert!(
            rx.try_recv().is_err(),
            "orphan with no fallback should not emit a message"
        );
    }

    #[tokio::test]
    async fn test_session_map_insert_and_remove() {
        let session_map: SessionMap = Arc::new(RwLock::new(HashMap::new()));
        let task_id_1 = TaskId::from_path("tasks/1.1.md");
        let task_id_2 = TaskId::from_path("tasks/1.2.md");

        // Insert two sessions.
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-1".to_string(),
                (task_id_1.clone(), AgentKind::Implementation),
            );
            map.insert(
                "sess-2".to_string(),
                (task_id_2.clone(), AgentKind::CodeReview),
            );
        }

        // Verify both are present.
        {
            let map = session_map.read().await;
            assert_eq!(map.len(), 2);
            assert!(map.contains_key("sess-1"));
            assert!(map.contains_key("sess-2"));
        }

        // Remove one session.
        {
            let mut map = session_map.write().await;
            map.remove("sess-1");
        }

        // Verify only the second session remains.
        {
            let map = session_map.read().await;
            assert_eq!(map.len(), 1);
            assert!(!map.contains_key("sess-1"));
            assert!(map.contains_key("sess-2"));
        }

        // Verify concurrent read from a cloned Arc sees the same map.
        let session_map_clone = Arc::clone(&session_map);
        let handle = tokio::spawn(async move {
            let map = session_map_clone.read().await;
            map.contains_key("sess-2")
        });
        assert!(
            handle.await.expect("spawn"),
            "cloned Arc sees same map state"
        );
    }

    /// Verifies that accumulated text is drained and included in SessionCompleted.
    #[tokio::test]
    async fn test_accumulated_text_cleared_on_complete() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert("sess-x".to_string(), (task_id.clone(), AgentKind::Intake));
        }

        // Emit two MessageUpdated events; only the latest text should be kept.
        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-x".to_string(),
                message_id: "msg-1".to_string(),
                parts: make_text_parts("first"),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-x".to_string(),
                message_id: "msg-2".to_string(),
                parts: make_text_parts("second"),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        // SessionCompleted should carry the last accumulated text.
        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-x".to_string(),
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("SessionCompleted message");
        assert!(
            matches!(&msg, AppMessage::SessionCompleted { response_text, .. } if response_text == "second"),
            "expected last accumulated text 'second'"
        );

        // Both maps should now be empty for this session.
        assert!(
            consumer.accumulated_text.is_empty(),
            "accumulated_text should be cleared after SessionCompleted"
        );
        assert!(
            consumer.best_response_text.is_empty(),
            "best_response_text should be cleared after SessionCompleted"
        );
    }

    /// Verifies that accumulated text is cleaned up when a session errors.
    #[tokio::test]
    async fn test_accumulated_text_cleaned_on_error() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert("sess-y".to_string(), (task_id.clone(), AgentKind::Design));
        }

        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-y".to_string(),
                message_id: "msg-1".to_string(),
                parts: make_text_parts("partial text"),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        consumer
            .handle_event(OpenCodeEvent::SessionError {
                session_id: "sess-y".to_string(),
                error: "crash".to_string(),
            })
            .await
            .expect("handle_event");
        // SessionError should still be routed.
        let _ = rx.try_recv().expect("SessionError message");

        assert!(
            consumer.accumulated_text.is_empty(),
            "accumulated_text should be cleared on SessionError"
        );
        assert!(
            consumer.best_response_text.is_empty(),
            "best_response_text should be cleared on SessionError"
        );
    }

    /// Verifies that an empty `MessageUpdated` (user-message event) does not overwrite a
    /// previously captured non-empty response text in `best_response_text`.
    #[tokio::test]
    async fn test_empty_message_updated_does_not_wipe_response_text() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert("sess-z".to_string(), (task_id.clone(), AgentKind::Intake));
        }

        // Emit a MessageUpdated with the real JSON response.
        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-z".to_string(),
                message_id: "msg-1".to_string(),
                parts: make_text_parts(r#"{"action":"complete"}"#),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        // Emit an empty MessageUpdated (simulates OpenCode user-message event).
        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-z".to_string(),
                message_id: "msg-2".to_string(),
                parts: make_text_parts(""),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        // SessionCompleted should carry the original non-empty response text.
        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-z".to_string(),
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("SessionCompleted message");
        assert!(
            matches!(&msg, AppMessage::SessionCompleted { response_text, .. } if response_text == r#"{"action":"complete"}"#),
            "expected original JSON response, got: {msg:?}"
        );

        assert!(consumer.accumulated_text.is_empty());
        assert!(consumer.best_response_text.is_empty());
    }

    /// Verifies that a large user-message context prompt does not overwrite the agent's
    /// short JSON response in `best_response_text`.
    ///
    /// The user-message text is longer than the agent response but does not contain
    /// `"action"`, so it must not be preferred over the agent's JSON.
    #[tokio::test]
    async fn test_large_user_context_does_not_pollute_response_text() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-ctx".to_string(),
                (task_id.clone(), AgentKind::CodeReview),
            );
        }

        // Simulate a large user-message context arriving first (no "action").
        let user_context = "## Task Context\n- Story: 6\n- Task: 6.2\n".repeat(200);
        assert!(user_context.len() > 5000, "user context should be large");
        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-ctx".to_string(),
                message_id: "msg-user".to_string(),
                parts: make_text_parts(&user_context),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        // Agent then responds with a short JSON (contains "action").
        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-ctx".to_string(),
                message_id: "msg-agent".to_string(),
                parts: make_text_parts(r#"{"action":"complete","summary":"done"}"#),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        // SessionCompleted must carry the agent JSON, not the user context.
        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-ctx".to_string(),
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("SessionCompleted message");
        assert!(
            matches!(&msg, AppMessage::SessionCompleted { response_text, .. }
                if response_text == r#"{"action":"complete","summary":"done"}"#),
            "expected agent JSON response, got: {msg:?}"
        );
    }

    /// Verifies that parse_wire_event correctly extracts the session ID from the nested
    /// info object in a session.created payload.
    #[test]
    fn test_parse_wire_event_session_created() {
        let json = r#"{"payload":{"type":"session.created","properties":{"info":{"id":"ses_abc","slug":"eager-rocket"}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::SessionCreated { ref session_id, .. } if session_id == "ses_abc"),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that session.created extracts the parent field when present.
    #[test]
    fn test_parse_wire_event_session_created_with_parent() {
        let json = r#"{"payload":{"type":"session.created","properties":{"info":{"id":"ses_child","slug":"eager-rocket","parent":"ses_parent"}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::SessionCreated { ref session_id, ref parent_id }
                if session_id == "ses_child" && parent_id.as_deref() == Some("ses_parent")),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that session.created without a parent field sets parent_id to None.
    #[test]
    fn test_parse_wire_event_session_created_no_parent() {
        let json = r#"{"payload":{"type":"session.created","properties":{"info":{"id":"ses_abc","slug":"eager-rocket"}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::SessionCreated { ref parent_id, .. }
                if parent_id.is_none()),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that session.created without info.id returns Unknown (with a warning).
    #[test]
    fn test_parse_wire_event_session_created_missing_id() {
        let json = r#"{"payload":{"type":"session.created","properties":{}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "expected Unknown, got: {event:?}"
        );
    }

    /// Verifies that session.error is mapped correctly with the real wire format.
    #[test]
    fn test_parse_wire_event_session_error() {
        let json = r#"{"payload":{"type":"session.error","properties":{"sessionID":"ses_abc","error":{"name":"APIError","data":{"message":"rate limit"}}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::SessionError { ref session_id, ref error }
                if session_id == "ses_abc" && error == "rate limit"),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that session.error handles the legacy plain-string error format.
    #[test]
    fn test_parse_wire_event_session_error_legacy_string() {
        let json = r#"{"payload":{"type":"session.error","properties":{"sessionID":"ses_abc","error":"rate limit"}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::SessionError { ref session_id, ref error }
                if session_id == "ses_abc" && error == "rate limit"),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that session.completed is mapped correctly.
    #[test]
    fn test_parse_wire_event_session_completed() {
        let json =
            r#"{"payload":{"type":"session.completed","properties":{"info":{"id":"ses_abc"}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::SessionCompleted { ref session_id } if session_id == "ses_abc"),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that message.updated with text parts produces MessageUpdated.
    ///
    /// In some OpenCode versions the final response text arrives only via
    /// message.updated (props["parts"]); we must extract it so accumulated_text
    /// is populated when SessionCompleted fires.
    #[test]
    fn test_parse_wire_event_message_updated_with_text_produces_message_updated() {
        let json = r#"{"payload":{"type":"message.updated","properties":{"sessionId":"ses_abc","messageId":"msg_1","parts":[{"type":"text","text":"hello"}]}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                event,
                OpenCodeEvent::MessageUpdated {
                    ref session_id,
                    ref message_id,
                    ref parts,
                } if session_id == "ses_abc"
                    && message_id == "msg_1"
                    && matches!(parts.first(), Some(MessagePart::Text { text }) if text == "hello")
            ),
            "message.updated with text parts should produce MessageUpdated, got: {event:?}"
        );
    }

    /// Verifies that message.updated with zero tokens is ignored (startup creation event).
    #[test]
    fn test_parse_wire_event_message_updated_zero_tokens_ignored() {
        let json = r#"{"payload":{"type":"message.updated","properties":{"sessionId":"ses_abc","info":{"tokens":{"input":0,"output":0,"reasoning":0}}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "message.updated with all-zero tokens should return Unknown, got: {event:?}"
        );
    }

    /// Verifies that message.updated with non-zero info.tokens emits TokensUpdated.
    ///
    /// This is the assistant message layout: tokens are at info.tokens.{input,output}.
    #[test]
    fn test_parse_wire_event_message_updated_with_info_tokens() {
        let json = r#"{"payload":{"type":"message.updated","properties":{"sessionId":"ses_abc","info":{"tokens":{"input":1234,"output":567,"reasoning":0}}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                event,
                OpenCodeEvent::TokensUpdated {
                    ref session_id,
                    input_tokens: 1234,
                    output_tokens: 567,
                    is_cumulative: true,
                    step_id: None,
                } if session_id == "ses_abc"
            ),
            "expected TokensUpdated from info.tokens path, got: {event:?}"
        );
    }

    /// Verifies that message.updated with info.summary.tokens also emits TokensUpdated (fallback path).
    #[test]
    fn test_parse_wire_event_message_updated_with_summary_tokens() {
        let json = r#"{"payload":{"type":"message.updated","properties":{"sessionId":"ses_abc","info":{"summary":{"tokens":{"input":2000,"output":800}}}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                event,
                OpenCodeEvent::TokensUpdated {
                    ref session_id,
                    input_tokens: 2000,
                    output_tokens: 800,
                    is_cumulative: true,
                    step_id: None,
                } if session_id == "ses_abc"
            ),
            "expected TokensUpdated from info.summary.tokens path, got: {event:?}"
        );
    }

    /// Verifies that a message.part.updated with no `part` object (legacy `parts` array)
    /// still returns Unknown (non-tool payload is ignored).
    #[test]
    fn test_parse_wire_event_message_part_updated_no_part_object_ignored() {
        let json = r#"{"payload":{"type":"message.part.updated","properties":{"sessionId":"ses_abc","messageId":"msg_1","parts":[{"type":"text","text":"hello"}]}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "message.part.updated with no part object should return Unknown, got: {event:?}"
        );
    }

    /// Verifies that a message.part.updated with a tool part in pending state produces ToolPending.
    #[test]
    fn test_parse_tool_pending_from_message_part_updated() {
        let json = r#"{"payload":{"type":"message.part.updated","properties":{"part":{"type":"tool","sessionID":"ses_abc","tool":"write","state":{"status":"pending"}}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                event,
                OpenCodeEvent::ToolPending { ref session_id, ref tool, .. }
                    if session_id == "ses_abc" && tool == "write"
            ),
            "expected ToolPending, got: {event:?}"
        );
    }

    /// Verifies that a message.part.updated with a tool part in running state produces ToolExecuting.
    #[test]
    fn test_parse_tool_running_from_message_part_updated() {
        let json = r#"{"payload":{"type":"message.part.updated","properties":{"part":{"type":"tool","sessionID":"ses_abc","tool":"bash","state":{"status":"running"}}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                event,
                OpenCodeEvent::ToolExecuting { ref session_id, ref tool, .. }
                    if session_id == "ses_abc" && tool == "bash"
            ),
            "expected ToolExecuting, got: {event:?}"
        );
    }

    /// Verifies that a message.part.updated with a tool part in completed state produces ToolCompleted.
    #[test]
    fn test_parse_tool_completed_from_message_part_updated() {
        let json = r#"{"payload":{"type":"message.part.updated","properties":{"part":{"type":"tool","sessionID":"ses_abc","tool":"read","state":{"status":"completed"}}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                event,
                OpenCodeEvent::ToolCompleted { ref session_id, ref tool, .. }
                    if session_id == "ses_abc" && tool == "read"
            ),
            "expected ToolCompleted, got: {event:?}"
        );
    }

    /// Verifies that a message.part.updated with a text part produces MessageUpdated
    /// (not Unknown), so that accumulated_text is populated for SessionCompleted.
    #[test]
    fn test_message_part_updated_text_produces_message_updated() {
        let json = r#"{"payload":{"type":"message.part.updated","properties":{"part":{"type":"text","sessionID":"ses_abc","messageID":"msg_1","text":"hello"}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                event,
                OpenCodeEvent::MessageUpdated {
                    ref session_id,
                    ref message_id,
                    ref parts,
                } if session_id == "ses_abc"
                    && message_id == "msg_1"
                    && matches!(parts.first(), Some(MessagePart::Text { text }) if text == "hello")
            ),
            "text part.updated should produce MessageUpdated, got: {event:?}"
        );
    }

    /// Verifies that text arriving via message.part.updated is accumulated and
    /// included in the SessionCompleted AppMessage's response_text field.
    #[tokio::test]
    async fn test_message_part_updated_text_accumulates_for_session_completed() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-text".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        // Emit a MessageUpdated produced from a text part.updated event.
        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-text".to_string(),
                message_id: "msg-1".to_string(),
                parts: make_text_parts("the full response"),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        // Now fire SessionCompleted and check response_text.
        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-text".to_string(),
            })
            .await
            .expect("handle_event");

        let msg = rx.try_recv().expect("SessionCompleted AppMessage");
        match msg {
            AppMessage::SessionCompleted {
                response_text,
                task_id: tid,
                ..
            } => {
                assert_eq!(tid, task_id);
                assert_eq!(
                    response_text, "the full response",
                    "response_text should carry text accumulated from part.updated"
                );
            }
            other => panic!("expected SessionCompleted, got: {other:?}"),
        }
    }

    /// Verifies that text arriving via message.updated (props["parts"]) is accumulated
    /// and included in the SessionCompleted AppMessage's response_text field.
    /// This covers the case where OpenCode delivers final text through message.updated
    /// rather than message.part.updated or message.part.delta.
    #[tokio::test]
    async fn test_message_updated_text_accumulates_for_session_completed() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert("sess-mu".to_string(), (task_id.clone(), AgentKind::Design));
        }

        // Simulate a message.updated event that carries the full response text.
        let json = r#"{"payload":{"type":"message.updated","properties":{"sessionId":"sess-mu","messageId":"msg-final","parts":[{"type":"text","text":"design output"}]}}}"#;
        let event = parse_wire_event(json);
        consumer.handle_event(event).await.expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-mu".to_string(),
            })
            .await
            .expect("handle_event");

        let msg = rx.try_recv().expect("SessionCompleted AppMessage");
        match msg {
            AppMessage::SessionCompleted {
                response_text,
                task_id: tid,
                ..
            } => {
                assert_eq!(tid, task_id);
                assert_eq!(
                    response_text, "design output",
                    "response_text should carry text accumulated from message.updated parts"
                );
            }
            other => panic!("expected SessionCompleted, got: {other:?}"),
        }
    }

    /// Verifies that known-but-ignored events (heartbeats etc.) return Unknown.
    #[test]
    fn test_parse_wire_event_heartbeat_ignored() {
        let json = r#"{"payload":{"type":"server.heartbeat","properties":{}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "heartbeat should return Unknown"
        );
    }

    /// Verifies that message.part.delta parses all required fields correctly.
    #[test]
    fn test_parse_wire_event_message_part_delta() {
        let json = r#"{"payload":{"type":"message.part.delta","properties":{"sessionID":"ses_abc","messageID":"msg_1","field":"text","delta":"hello "}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                event,
                OpenCodeEvent::MessagePartDelta {
                    ref session_id,
                    ref message_id,
                    ref field,
                    ref delta
                } if session_id == "ses_abc"
                    && message_id == "msg_1"
                    && field == "text"
                    && delta == "hello "
            ),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that message.part.delta with missing fields returns Unknown.
    #[test]
    fn test_parse_wire_event_message_part_delta_missing_fields() {
        // Missing "delta" field.
        let json = r#"{"payload":{"type":"message.part.delta","properties":{"sessionID":"ses_abc","messageID":"msg_1","field":"text"}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "expected Unknown when delta is missing, got: {event:?}"
        );
    }

    /// Verifies that session.idle maps to SessionCompleted.
    #[test]
    fn test_parse_wire_event_session_idle() {
        let json = r#"{"payload":{"type":"session.idle","properties":{"sessionID":"ses_abc"}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::SessionCompleted { ref session_id } if session_id == "ses_abc"),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that session.idle without a session ID returns Unknown.
    #[test]
    fn test_parse_wire_event_session_idle_missing_id() {
        let json = r#"{"payload":{"type":"session.idle","properties":{}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "expected Unknown when session id is missing, got: {event:?}"
        );
    }

    /// Verifies that session.status with type=idle maps to SessionCompleted.
    #[test]
    fn test_session_status_idle_maps_to_completed() {
        let json = r#"{"payload":{"type":"session.status","properties":{"sessionID":"ses_abc","status":{"type":"idle"}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(&event, OpenCodeEvent::SessionCompleted { session_id } if session_id == "ses_abc"),
            "session.status idle should return SessionCompleted, got: {event:?}"
        );
    }

    /// Verifies that session.status with type=busy returns Unknown.
    #[test]
    fn test_session_status_busy_ignored() {
        let json = r#"{"payload":{"type":"session.status","properties":{"sessionID":"ses_abc","status":{"type":"busy"}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "session.status busy should return Unknown, got: {event:?}"
        );
    }

    /// Verifies that session.diff now produces a SessionDiff event.
    #[test]
    fn test_parse_wire_event_session_diff_routes() {
        let json = r#"{"payload":{"type":"session.diff","properties":{"sessionID":"ses_abc"}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::SessionDiff { ref session_id } if session_id == "ses_abc"),
            "session.diff should produce SessionDiff, got: {event:?}"
        );
    }

    /// Verifies that permission.asked is parsed to PermissionAsked.
    #[test]
    fn test_parse_permission_asked() {
        let json = r#"{"payload":{"type":"permission.asked","properties":{"id":"perm-1","sessionID":"ses_abc","permission":"bash","patterns":["cargo build 2>&1"],"always":[]}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                &event,
                OpenCodeEvent::PermissionAsked { ref session_id, ref request }
                    if session_id == "ses_abc"
                    && request.id == "perm-1"
                    && request.permission == "bash"
                    && request.patterns == vec!["cargo build 2>&1"]
            ),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that permission.asked with missing fields returns Unknown.
    #[test]
    fn test_parse_permission_asked_missing_fields() {
        let json =
            r#"{"payload":{"type":"permission.asked","properties":{"sessionID":"ses_abc"}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "expected Unknown when id missing, got: {event:?}"
        );
    }

    /// Verifies that question.asked is parsed to QuestionAsked.
    #[test]
    fn test_parse_question_asked() {
        let json = r#"{"payload":{"type":"question.asked","properties":{"id":"req-1","sessionID":"ses_abc","question":"What is the target environment?"}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                &event,
                OpenCodeEvent::QuestionAsked { ref session_id, ref request_id, ref question }
                    if session_id == "ses_abc"
                    && request_id == "req-1"
                    && question == "What is the target environment?"
            ),
            "unexpected: {event:?}"
        );
    }

    /// Verifies that question.asked with missing fields returns Unknown.
    #[test]
    fn test_parse_question_asked_missing_fields() {
        let json = r#"{"payload":{"type":"question.asked","properties":{"sessionID":"ses_abc"}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "expected Unknown when question missing, got: {event:?}"
        );
    }

    /// Verifies that handle_event routes PermissionAsked to AppMessage::PermissionAsked.
    #[tokio::test]
    async fn test_handle_permission_asked_routes_to_app() {
        use crate::opencode::types::PermissionRequest;

        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-abc".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        let request = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-abc".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["cargo build".to_string()],
            always: vec![],
        };

        consumer
            .handle_event(OpenCodeEvent::PermissionAsked {
                session_id: "sess-abc".to_string(),
                request: request.clone(),
            })
            .await
            .expect("handle_event");

        let msg = rx.try_recv().expect("PermissionAsked message");
        assert!(
            matches!(&msg, AppMessage::PermissionAsked { task_id: ref tid, request: ref r }
                if *tid == task_id && r.id == "perm-1"),
            "unexpected: {msg:?}"
        );
    }

    /// Verifies that handle_event routes QuestionAsked to AppMessage::OpenCodeQuestionAsked.
    #[tokio::test]
    async fn test_handle_question_asked_routes_to_app() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-abc".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        consumer
            .handle_event(OpenCodeEvent::QuestionAsked {
                session_id: "sess-abc".to_string(),
                request_id: "req-1".to_string(),
                question: "What is the target environment?".to_string(),
            })
            .await
            .expect("handle_event");

        let msg = rx.try_recv().expect("OpenCodeQuestionAsked message");
        assert!(
            matches!(&msg, AppMessage::OpenCodeQuestionAsked { ref request_id, ref question, .. }
                if request_id == "req-1" && question == "What is the target environment?"),
            "unexpected: {msg:?}"
        );
    }

    /// Verifies that SessionDiff routes to AppMessage::SessionDiffChanged.
    #[tokio::test]
    async fn test_handle_session_diff_routes_to_app() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-abc".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        consumer
            .handle_event(OpenCodeEvent::SessionDiff {
                session_id: "sess-abc".to_string(),
            })
            .await
            .expect("handle_event");

        let msg = rx.try_recv().expect("SessionDiffChanged message");
        assert!(
            matches!(&msg, AppMessage::SessionDiffChanged { ref session_id, .. }
                if session_id == "sess-abc"),
            "unexpected: {msg:?}"
        );
    }

    /// Verifies that completely unknown event types return Unknown.
    #[test]
    fn test_parse_wire_event_unknown_type() {
        let json = r#"{"payload":{"type":"some.future.event","properties":{"foo":"bar"}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "unrecognized type should return Unknown"
        );
    }

    /// Verifies that invalid JSON returns Unknown.
    #[test]
    fn test_parse_wire_event_invalid_json() {
        let event = parse_wire_event("not json at all");
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "invalid JSON should return Unknown"
        );
    }

    /// Verifies that three MessagePartDelta events produce three StreamingUpdates
    /// with progressively accumulated text.
    #[tokio::test]
    async fn test_handle_event_message_part_delta_accumulates() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-d".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        for (i, chunk) in ["Hello", " world", "!"].iter().enumerate() {
            consumer
                .handle_event(OpenCodeEvent::MessagePartDelta {
                    session_id: "sess-d".to_string(),
                    message_id: "msg-1".to_string(),
                    field: "text".to_string(),
                    delta: chunk.to_string(),
                })
                .await
                .expect("handle_event");

            let msg = rx.try_recv().expect("StreamingUpdate expected");
            let expected = ["Hello", "Hello world", "Hello world!"][i];
            assert!(
                matches!(&msg, AppMessage::StreamingUpdate { parts, .. }
                    if matches!(parts.first(), Some(crate::opencode::types::MessagePart::Text { text }) if text == expected)),
                "chunk {i}: expected accumulated '{expected}', got: {msg:?}"
            );
        }
    }

    /// Verifies that accumulated delta text is included in SessionCompleted response_text.
    #[tokio::test]
    async fn test_handle_event_delta_updates_accumulated_text_for_session_completed() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-e".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        for chunk in &["full ", "response"] {
            consumer
                .handle_event(OpenCodeEvent::MessagePartDelta {
                    session_id: "sess-e".to_string(),
                    message_id: "msg-1".to_string(),
                    field: "text".to_string(),
                    delta: chunk.to_string(),
                })
                .await
                .expect("handle_event");
            let _ = rx.try_recv().expect("drain StreamingUpdate");
        }

        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-e".to_string(),
            })
            .await
            .expect("handle_event");

        let msg = rx.try_recv().expect("SessionCompleted expected");
        assert!(
            matches!(&msg, AppMessage::SessionCompleted { response_text, .. } if response_text == "full response"),
            "expected 'full response' in SessionCompleted, got: {msg:?}"
        );
    }

    /// Verifies that accumulated_deltas is drained after SessionCompleted.
    #[tokio::test]
    async fn test_handle_event_delta_cleanup_on_completed() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-f".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        consumer
            .handle_event(OpenCodeEvent::MessagePartDelta {
                session_id: "sess-f".to_string(),
                message_id: "msg-1".to_string(),
                field: "text".to_string(),
                delta: "some text".to_string(),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        assert!(
            !consumer.accumulated_deltas.is_empty(),
            "accumulated_deltas should be populated before completion"
        );

        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-f".to_string(),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain SessionCompleted");

        assert!(
            consumer.accumulated_deltas.is_empty(),
            "accumulated_deltas should be empty after SessionCompleted"
        );
    }

    /// Verifies that accumulated_deltas is drained after SessionError.
    #[tokio::test]
    async fn test_handle_event_delta_cleanup_on_error() {
        let (mut consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-g".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        consumer
            .handle_event(OpenCodeEvent::MessagePartDelta {
                session_id: "sess-g".to_string(),
                message_id: "msg-1".to_string(),
                field: "text".to_string(),
                delta: "partial".to_string(),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain StreamingUpdate");

        consumer
            .handle_event(OpenCodeEvent::SessionError {
                session_id: "sess-g".to_string(),
                error: "crash".to_string(),
            })
            .await
            .expect("handle_event");
        let _ = rx.try_recv().expect("drain SessionError");

        assert!(
            consumer.accumulated_deltas.is_empty(),
            "accumulated_deltas should be empty after SessionError"
        );
    }

    /// Verifies that MessagePartDelta for an unknown session emits no message.
    #[tokio::test]
    async fn test_handle_event_delta_unknown_session_ignored() {
        let (mut consumer, mut rx, _session_map) = make_consumer();

        consumer
            .handle_event(OpenCodeEvent::MessagePartDelta {
                session_id: "no-such-session".to_string(),
                message_id: "msg-1".to_string(),
                field: "text".to_string(),
                delta: "hi".to_string(),
            })
            .await
            .expect("handle_event");

        assert!(
            rx.try_recv().is_err(),
            "no message expected for delta from unknown session"
        );
    }

    /// Verifies that a message.part.updated with a step-finish part containing non-zero
    /// tokens emits TokensUpdated with the correct counts.
    #[test]
    fn test_parse_step_finish_tokens_from_message_part_updated() {
        // Matches the actual SSE payload observed in production logs (research.md line 29).
        let json = r#"{"payload":{"type":"message.part.updated","properties":{"part":{"cost":0.044665,"id":"prt_abc","messageID":"msg_1","reason":"tool-calls","sessionID":"ses_xyz","tokens":{"cache":{"read":0,"write":0},"input":7873,"output":212,"reasoning":0,"total":8085},"type":"step-finish"}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(
                event,
                OpenCodeEvent::TokensUpdated {
                    ref session_id,
                    input_tokens,
                    output_tokens,
                    is_cumulative: false,
                    ref step_id,
                } if session_id == "ses_xyz" && input_tokens == 7873 && output_tokens == 212 && step_id.as_deref() == Some("prt_abc")
            ),
            "expected TokensUpdated from step-finish part, got: {event:?}"
        );
    }

    /// Verifies that `permission.replied` is treated as a known-ignored event and returns
    /// `Unknown` without hitting the catch-all warn branch.
    #[test]
    fn test_parse_permission_replied_is_ignored() {
        let json = r#"{"payload":{"type":"permission.replied","properties":{}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "permission.replied should parse as Unknown (known-ignored), got: {event:?}"
        );
    }

    /// Verifies that a step-finish part with all-zero token counts returns Unknown
    /// (avoids displaying "in:0 out:0" before any real inference has happened).
    #[test]
    fn test_parse_step_finish_zero_tokens_ignored() {
        let json = r#"{"payload":{"type":"message.part.updated","properties":{"part":{"sessionID":"ses_xyz","tokens":{"input":0,"output":0},"type":"step-finish"}}}}"#;
        let event = parse_wire_event(json);
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "step-finish with zero tokens should return Unknown, got: {event:?}"
        );
    }
}
