//! KiroProcess: lifecycle management for a single kiro-cli child process.
//!
//! Each [`KiroProcess`] wraps one kiro-cli process, owns its transport, and manages
//! the ACP session lifecycle from spawn through shutdown.
//!
//! # Lifecycle
//!
//! 1. **Spawn** -- `kiro acp --agent <name>` with piped stdin/stdout/stderr.
//! 2. **Initialize** -- Send `initialize` request; receive capabilities. Send `initialized` notification.
//! 3. **Session** -- Send `session/new`; receive `sessionId`.
//! 4. **Prompt** -- Send `session/prompt` notification with text content.
//! 5. **Event loop** -- [`events::run_event_loop`] translates ACP notifications to [`AppMessage`]s.
//! 6. **Cancel** -- Send `session/cancel` notification.
//! 7. **Shutdown** -- Close stdin, wait briefly, kill process.
//!
//! # One process per agent stage
//!
//! A fresh process is spawned for each agent stage in the pipeline. This avoids
//! kiro's automatic context compaction from discarding mid-pipeline context.

use std::process::Stdio;
use std::sync::{Arc, Mutex};

use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::error::{ClawdMuxError, Result};
use crate::messages::AppMessage;
use crate::tasks::models::TaskId;

use super::events::{run_event_loop, PermissionResponse};
use super::transport::Transport;
use super::types::{
    ClientCapabilities, ClientInfo, ContentPart, InitializeParams, InitializeResult,
    SessionNewParams, SessionNewResult, SessionPromptParams,
};

/// Protocol version string sent during ACP initialization.
const ACP_PROTOCOL_VERSION: &str = "1.0";

/// A running kiro-cli child process with an active ACP session.
///
/// Drop this to begin graceful shutdown (close stdin, kill after timeout).
pub struct KiroProcess {
    transport: Transport,
    task_id: TaskId,
    session_id: String,
    permission_tx: mpsc::Sender<PermissionResponse>,
    reader_handle: JoinHandle<()>,
    event_loop_handle: JoinHandle<()>,
    child: tokio::process::Child,
    /// Accumulated agent text from streaming chunks; shared with the event loop.
    accumulated_text: Arc<Mutex<String>>,
}

impl KiroProcess {
    /// Spawn a kiro-cli process, run ACP initialization, and create a session.
    ///
    /// # Arguments
    /// * `binary` – path or name of the kiro-cli binary (looked up in PATH if not absolute).
    /// * `agent_name` – kiro agent name, e.g. `"clawdmux-intake"`.
    /// * `cwd` – absolute path to the project working directory, sent in `initialize`.
    /// * `task_id` – task this process belongs to.
    /// * `async_tx` – channel for forwarding [`AppMessage`] variants to the application.
    ///
    /// Returns an initialized `KiroProcess` ready to receive prompts.
    pub async fn spawn(
        binary: &str,
        agent_name: &str,
        cwd: &str,
        task_id: TaskId,
        async_tx: mpsc::Sender<AppMessage>,
    ) -> Result<Self> {
        tracing::info!("spawning kiro-cli: binary={binary} agent={agent_name} task={task_id}");

        let mut child = Command::new(binary)
            .args(["acp", "--agent", agent_name])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                ClawdMuxError::Kiro(format!("failed to spawn kiro-cli ({binary}): {e}"))
            })?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| ClawdMuxError::Kiro("kiro-cli stdin not available".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| ClawdMuxError::Kiro("kiro-cli stdout not available".to_string()))?;

        // Drain stderr to avoid blocking and surface kiro-cli errors in the log.
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let reader = tokio::io::BufReader::new(stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if !line.is_empty() {
                        tracing::warn!("kiro-cli stderr: {line}");
                    }
                }
            });
        }

        // Notification channel: reader task -> event loop
        let (notification_tx, notification_rx) = mpsc::channel::<super::types::IncomingMessage>(64);
        let (permission_tx, permission_rx) = mpsc::channel::<PermissionResponse>(8);

        let (transport, reader_handle) = Transport::new(stdin, stdout, notification_tx);

        // ACP handshake: initialize + initialized
        let session_id = Self::handshake(&transport, agent_name, cwd).await?;

        tracing::info!(
            "kiro-cli session created: agent={agent_name} session_id={session_id} task={task_id}"
        );

        // Shared accumulated text: event loop writes chunks; send_prompt reads final value.
        let accumulated_text: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));

        // Spawn the event loop to translate ACP notifications -> AppMessage
        let event_transport = transport.clone();
        let event_task_id = task_id.clone();
        let event_session_id = session_id.clone();
        let event_accumulated = accumulated_text.clone();
        let event_loop_handle = tokio::spawn(run_event_loop(
            event_task_id,
            event_session_id,
            event_transport,
            notification_rx,
            permission_rx,
            async_tx,
            event_accumulated,
        ));

        Ok(Self {
            transport,
            task_id,
            session_id,
            permission_tx,
            reader_handle,
            event_loop_handle,
            accumulated_text,
            child,
        })
    }

    /// Perform the ACP initialization handshake: `initialize` -> `initialized` -> `session/new`.
    ///
    /// Returns the new session ID on success.
    async fn handshake(transport: &Transport, agent_name: &str, cwd: &str) -> Result<String> {
        // Send initialize request
        let params = InitializeParams {
            protocol_version: ACP_PROTOCOL_VERSION.to_string(),
            client_info: ClientInfo {
                name: "clawdmux".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
            },
            capabilities: ClientCapabilities {
                fs: false,
                terminal: false,
            },
        };
        let result = transport
            .request("initialize", Some(serde_json::to_value(&params)?))
            .await?;

        let init_result: InitializeResult = serde_json::from_value(result).map_err(|e| {
            ClawdMuxError::Kiro(format!(
                "invalid initialize response from {agent_name}: {e}"
            ))
        })?;

        tracing::debug!(
            "kiro-cli initialized: protocol={} agent={}",
            init_result.protocol_version,
            init_result.agent_info.name
        );

        // Send initialized notification (no response expected)
        transport.notify("initialized", None).await?;

        // Create a new session
        let session_params = SessionNewParams {
            cwd: cwd.to_string(),
            mcp_servers: vec![],
            metadata: None,
        };
        let session_result = transport
            .request("session/new", Some(serde_json::to_value(&session_params)?))
            .await?;

        let session: SessionNewResult = serde_json::from_value(session_result).map_err(|e| {
            ClawdMuxError::Kiro(format!(
                "invalid session/new response from {agent_name}: {e}"
            ))
        })?;

        Ok(session.session_id)
    }

    /// Send a text prompt to the active session.
    ///
    /// `session/prompt` is a JSON-RPC **request** (not a notification): kiro
    /// responds when the turn completes with `{ stopReason: "end_turn" | ... }`.
    /// A background task awaits that response and forwards any error to `async_tx`.
    /// Streaming updates arrive separately via `session/update` notifications handled
    /// by the event loop.
    pub async fn send_prompt(
        &self,
        prompt: &str,
        async_tx: mpsc::Sender<AppMessage>,
    ) -> Result<()> {
        let params = SessionPromptParams {
            session_id: self.session_id.clone(),
            prompt: vec![ContentPart::text(prompt)],
        };
        let params_value = serde_json::to_value(&params)?;
        let transport = self.transport.clone();
        let task_id = self.task_id.clone();
        let session_id = self.session_id.clone();
        let accumulated_text = self.accumulated_text.clone();

        tokio::spawn(async move {
            match transport
                .request("session/prompt", Some(params_value))
                .await
            {
                Ok(result) => {
                    let stop_reason = result
                        .get("stopReason")
                        .and_then(|v| v.as_str())
                        .unwrap_or("end_turn");
                    tracing::info!(
                        "session/prompt response: task={task_id} stop_reason={stop_reason}"
                    );
                    if stop_reason == "error" || stop_reason == "cancelled" {
                        let _ = async_tx
                            .send(AppMessage::SessionError {
                                task_id,
                                session_id,
                                error: format!(
                                    "session/prompt returned stop reason: {stop_reason}"
                                ),
                            })
                            .await;
                    } else {
                        // Read the text accumulated by the event loop during this turn.
                        let response_text = accumulated_text
                            .lock()
                            .map(|g| g.clone())
                            .unwrap_or_default();
                        let _ = async_tx
                            .send(AppMessage::SessionCompleted {
                                task_id,
                                session_id,
                                response_text,
                            })
                            .await;
                    }
                }
                Err(e) => {
                    tracing::error!("session/prompt failed for task={task_id}: {e}");
                    let _ = async_tx
                        .send(AppMessage::SessionError {
                            task_id,
                            session_id,
                            error: e.to_string(),
                        })
                        .await;
                }
            }
        });

        Ok(())
    }

    /// Send a `session/cancel` notification to abort the current turn.
    pub async fn cancel(&self) -> Result<()> {
        use super::types::SessionCancelParams;
        let params = SessionCancelParams {
            session_id: self.session_id.clone(),
        };
        self.transport
            .notify("session/cancel", Some(serde_json::to_value(&params)?))
            .await
    }

    /// Resolve a pending permission request.
    ///
    /// Routes the user's decision to the event loop via the permission channel.
    pub async fn resolve_permission(&self, rpc_id: u64, decision: &str) -> Result<()> {
        self.permission_tx
            .send(PermissionResponse::new(rpc_id, decision))
            .await
            .map_err(|_| {
                ClawdMuxError::Kiro(
                    "permission channel closed (process already exited)".to_string(),
                )
            })
    }

    /// The ACP session ID for this process.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Gracefully shut down the kiro-cli process.
    ///
    /// Closes the transport writer (drops stdin), aborts the event loop,
    /// and kills the child process if it has not already exited.
    pub async fn shutdown(mut self) {
        tracing::debug!("shutting down kiro-cli for task={}", self.task_id);
        self.reader_handle.abort();
        self.event_loop_handle.abort();

        // Kill the child process; ignore errors if already exited.
        let _ = self.child.kill().await;
        let _ = self.child.wait().await;
    }
}

/// Helper: resolve the kiro binary path.
///
/// Uses `binary` if provided, otherwise tries `"kiro"` from PATH.
pub fn resolve_binary(binary: Option<&str>) -> String {
    binary.unwrap_or("kiro").to_string()
}

/// Check whether the kiro binary is available on this system.
///
/// Performs a synchronous PATH lookup via the `which` crate pattern
/// (uses `std::process::Command` with `--version` as a probe).
pub fn check_kiro_available(binary: &str) -> bool {
    std::process::Command::new(binary)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_binary_default() {
        assert_eq!(resolve_binary(None), "kiro");
    }

    #[test]
    fn test_resolve_binary_custom() {
        assert_eq!(
            resolve_binary(Some("/usr/local/bin/kiro")),
            "/usr/local/bin/kiro"
        );
    }

    #[test]
    fn test_check_kiro_available_nonexistent() {
        // A binary that definitely doesn't exist should return false.
        assert!(!check_kiro_available("this-binary-does-not-exist-12345"));
    }

    #[test]
    fn test_check_kiro_available_echo() {
        // "echo" exists on all Unix systems and exits 0 even with --version.
        // This tests that the availability check correctly identifies an existing binary.
        #[cfg(unix)]
        assert!(check_kiro_available("echo"));
    }

    #[test]
    fn test_acp_protocol_version() {
        assert!(!ACP_PROTOCOL_VERSION.is_empty());
    }
}
