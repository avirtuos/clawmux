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
use crate::opencode::types::OpenCodeEvent;
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
        Self { tx, session_map }
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
    pub async fn run(&self, base_url: String) -> Result<()> {
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
                        match serde_json::from_str::<OpenCodeEvent>(&msg.data) {
                            Ok(oc_event) => {
                                if let Err(e) = self.handle_event(oc_event).await {
                                    warn!("Error handling SSE event: {}", e);
                                }
                            }
                            Err(e) => {
                                warn!("Failed to deserialize SSE event: {}", e);
                            }
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
    pub(crate) async fn handle_event(&self, event: OpenCodeEvent) -> Result<()> {
        match event {
            OpenCodeEvent::SessionCreated { session_id } => {
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
                if let Some(task_id) = task_id {
                    self.send(AppMessage::SessionCreated {
                        task_id,
                        session_id,
                    })
                    .await?;
                } else {
                    warn!(
                        "SessionCreated for unknown session_id after retries: {}",
                        session_id
                    );
                }
            }
            OpenCodeEvent::MessageUpdated {
                session_id, parts, ..
            } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::StreamingUpdate {
                        task_id,
                        session_id,
                        parts,
                    })
                    .await?;
                } else {
                    debug!("MessageUpdated for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::ToolExecuting { session_id, tool } => {
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
                    })
                    .await?;
                } else {
                    debug!("ToolExecuting for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::ToolCompleted {
                session_id, tool, ..
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
                    })
                    .await?;
                } else {
                    debug!("ToolCompleted for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::SessionCompleted { session_id } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::SessionCompleted {
                        task_id,
                        session_id,
                    })
                    .await?;
                } else {
                    debug!("SessionCompleted for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::SessionError { session_id, error } => {
                let task_id = {
                    let map = self.session_map.read().await;
                    map.get(&session_id).map(|(task_id, _)| task_id.clone())
                };
                if let Some(task_id) = task_id {
                    self.send(AppMessage::SessionError {
                        task_id,
                        session_id,
                        error,
                    })
                    .await?;
                } else {
                    debug!("SessionError for unknown session_id: {}", session_id);
                }
            }
            OpenCodeEvent::MessageCreated { .. } => {
                // Ignored -- redundant with MessageUpdated
            }
            OpenCodeEvent::Unknown => {
                debug!("Received unknown SSE event type, ignoring");
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

    #[tokio::test]
    async fn test_event_routing_known_session() {
        let (consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/1.1.md");
        {
            let mut map = session_map.write().await;
            map.insert(
                "sess-abc".to_string(),
                (task_id.clone(), AgentKind::Implementation),
            );
        }

        // SessionCompleted
        consumer
            .handle_event(OpenCodeEvent::SessionCompleted {
                session_id: "sess-abc".to_string(),
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("SessionCompleted message");
        assert!(
            matches!(msg, AppMessage::SessionCompleted { ref session_id, .. } if session_id == "sess-abc")
        );

        // MessageUpdated
        consumer
            .handle_event(OpenCodeEvent::MessageUpdated {
                session_id: "sess-abc".to_string(),
                message_id: "msg-1".to_string(),
                parts: vec![],
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("StreamingUpdate message");
        assert!(
            matches!(msg, AppMessage::StreamingUpdate { ref session_id, .. } if session_id == "sess-abc")
        );

        // ToolExecuting
        consumer
            .handle_event(OpenCodeEvent::ToolExecuting {
                session_id: "sess-abc".to_string(),
                tool: "bash".to_string(),
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
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("ToolActivity completed message");
        assert!(
            matches!(msg, AppMessage::ToolActivity { ref status, .. } if status == "completed")
        );

        // SessionError
        consumer
            .handle_event(OpenCodeEvent::SessionError {
                session_id: "sess-abc".to_string(),
                error: "oops".to_string(),
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("SessionError message");
        assert!(
            matches!(msg, AppMessage::SessionError { ref session_id, .. } if session_id == "sess-abc")
        );

        // SessionCreated
        consumer
            .handle_event(OpenCodeEvent::SessionCreated {
                session_id: "sess-abc".to_string(),
            })
            .await
            .expect("handle_event");
        let msg = rx.try_recv().expect("SessionCreated message");
        assert!(
            matches!(msg, AppMessage::SessionCreated { ref session_id, .. } if session_id == "sess-abc")
        );
    }

    #[tokio::test]
    async fn test_event_routing_unknown_session() {
        let (consumer, mut rx, _session_map) = make_consumer();
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

    /// Verifies that `SessionCreated` succeeds when the session map is populated
    /// after a short delay (simulating the TOCTOU race condition).
    #[tokio::test]
    async fn test_session_created_retry_succeeds() {
        let (consumer, mut rx, session_map) = make_consumer();
        let task_id = TaskId::from_path("tasks/6.1.md");
        let session_id = "sess-race".to_string();

        // Spawn a task that inserts into the session map after 25ms,
        // simulating the caller populating the map slightly after the SSE event arrives.
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
}
