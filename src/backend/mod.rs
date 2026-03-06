//! Backend abstraction layer for ClawMux.
//!
//! Defines the [`AgentBackend`] trait that decouples the application from any
//! specific AI coding assistant backend (OpenCode, kiro, etc.).  All session
//! lifecycle operations flow through this trait so that [`App`] remains
//! backend-agnostic.

pub mod kiro;
pub mod opencode;
pub use opencode::OpenCodeBackend;

use tokio::sync::mpsc;

use crate::messages::AppMessage;
use crate::opencode::events::SessionMap;
use crate::opencode::types::{ModelId, PermissionRequest};
use crate::tasks::models::TaskId;
use crate::workflow::agents::AgentKind;

/// Abstracts over AI coding assistant backends (OpenCode, kiro, etc.).
///
/// All methods are synchronous fire-and-forget: they spawn the necessary async
/// work internally and route results back through `async_tx`.  This keeps
/// [`App::handle_message`][crate::app::App::handle_message] fully synchronous
/// while still supporting async I/O.
pub trait AgentBackend: Send + Sync {
    /// Create a new agent session and send the initial prompt.
    ///
    /// On success, sends [`AppMessage::PromptSent`] via `async_tx`.
    /// On failure, sends [`AppMessage::SessionError`].
    ///
    /// # Arguments
    /// * `task_id` - The task this session belongs to.
    /// * `agent` - The pipeline agent to run.
    /// * `prompt` - The initial user prompt composed from task context.
    /// * `model` - Optional model override; backend uses its default if `None`.
    /// * `session_map` - Shared map for correlating session IDs to tasks.
    /// * `async_tx` - Channel for routing results back to the event loop.
    fn create_session(
        &self,
        task_id: TaskId,
        agent: AgentKind,
        prompt: String,
        model: Option<ModelId>,
        session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    );

    /// Create a new agent session without immediately sending a prompt.
    ///
    /// Used when the previous session's response could not be parsed and the
    /// user will steer the agent manually via a follow-up prompt (press `[p]`).
    /// On success, sends [`AppMessage::SessionCreated`] via `async_tx`.
    ///
    /// # Arguments
    /// * `task_id` - The task this session belongs to.
    /// * `agent` - The pipeline agent to create the session for.
    /// * `session_map` - Shared map for correlating session IDs to tasks.
    /// * `async_tx` - Channel for routing results back to the event loop.
    fn create_idle_session(
        &self,
        task_id: TaskId,
        agent: AgentKind,
        session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    );

    /// Send an additional prompt to an already-running session.
    ///
    /// On failure, sends [`AppMessage::SessionError`].
    ///
    /// # Arguments
    /// * `task_id` - The task owning this session.
    /// * `session_id` - The backend session identifier.
    /// * `agent` - The agent currently running in this session.
    /// * `prompt` - The follow-up prompt text.
    /// * `model` - Optional model override.
    /// * `async_tx` - Channel for routing results back to the event loop.
    fn send_prompt(
        &self,
        task_id: TaskId,
        session_id: String,
        agent: AgentKind,
        prompt: String,
        model: Option<ModelId>,
        async_tx: mpsc::Sender<AppMessage>,
    );

    /// Abort a running session.
    ///
    /// On failure, sends [`AppMessage::SessionError`].
    ///
    /// # Arguments
    /// * `task_id` - The task owning this session.
    /// * `session_id` - The backend session identifier to abort.
    /// * `async_tx` - Channel for routing results back to the event loop.
    fn abort_session(
        &self,
        task_id: TaskId,
        session_id: String,
        async_tx: mpsc::Sender<AppMessage>,
    );

    /// Resolve a pending permission request.
    ///
    /// After resolution, optionally sends a steering prompt to the agent when
    /// the permission was rejected with an explanation (`send_prompt_msg`).
    /// On API failure, re-emits [`AppMessage::PermissionAsked`] to retry.
    ///
    /// # Arguments
    /// * `task_id` - The task owning the permission request.
    /// * `request` - The original permission request from the agent.
    /// * `response` - One of `"once"`, `"always"`, or `"reject"`.
    /// * `send_prompt_msg` - Optional steering prompt to dispatch after resolution.
    /// * `async_tx` - Channel for routing results back to the event loop.
    fn resolve_permission(
        &self,
        task_id: TaskId,
        request: PermissionRequest,
        response: String,
        send_prompt_msg: Option<AppMessage>,
        async_tx: mpsc::Sender<AppMessage>,
    );

    /// Fetch current file diffs for a session.
    ///
    /// On success, sends [`AppMessage::DiffReady`] via `async_tx`.
    ///
    /// # Arguments
    /// * `task_id` - The task owning this session.
    /// * `session_id` - The backend session identifier.
    /// * `async_tx` - Channel for routing results back to the event loop.
    fn get_diffs(&self, task_id: TaskId, session_id: String, async_tx: mpsc::Sender<AppMessage>);

    /// Reply to a backend-generated question with the human's answer.
    ///
    /// This is a no-op for backends that do not support bidirectional
    /// question/answer flows (e.g., kiro delegates these to [`AppMessage`]).
    ///
    /// # Arguments
    /// * `task_id` - The task owning the session with the open question.
    /// * `request_id` - The backend-assigned question request identifier.
    /// * `answer` - The human's answer text.
    /// * `async_tx` - Channel for routing results back to the event loop.
    fn reply_question(
        &self,
        task_id: TaskId,
        request_id: String,
        answer: String,
        async_tx: mpsc::Sender<AppMessage>,
    );

    /// Poll the status of active sessions for liveness checking.
    ///
    /// For each session found to be idle when it should still be running,
    /// sends [`AppMessage::VerifySessionIdle`] via `async_tx`.
    ///
    /// # Arguments
    /// * `sessions` - List of `(task_id, session_id)` pairs to check.
    /// * `async_tx` - Channel for routing results back to the event loop.
    fn check_session_statuses(
        &self,
        sessions: Vec<(TaskId, String)>,
        async_tx: mpsc::Sender<AppMessage>,
    );

    /// Stage changed files and commit them via an agent-mediated git operation.
    ///
    /// Creates a new session (registered in `session_map`) and routes
    /// completion through [`AppMessage::CommitCompleted`] / [`AppMessage::CommitFailed`].
    ///
    /// # Arguments
    /// * `task_id` - The task to commit changes for.
    /// * `commit_message` - The human-approved commit message.
    /// * `file_paths` - Changed file paths for targeted `git add` (empty = `git add -A`).
    /// * `model` - Optional model override for the commit session.
    /// * `session_map` - Shared map for registering the commit session.
    /// * `async_tx` - Channel for routing results back to the event loop.
    fn commit_changes(
        &self,
        task_id: TaskId,
        commit_message: String,
        file_paths: Vec<String>,
        model: Option<ModelId>,
        session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    );

    /// Returns `true` if the backend is connected and ready to accept requests.
    fn is_available(&self) -> bool;

    /// Returns a human-readable name for this backend (e.g. `"opencode"`).
    fn name(&self) -> &str;
}

/// Perform a git add + commit directly, capturing stdout/stderr to avoid
/// corrupting the TUI.
///
/// Stages all working-tree changes via `git add -A`. We assume only one task
/// is worked on at a time in this workspace, so all pending changes belong to
/// the current task.
pub(crate) async fn run_git_commit(commit_message: &str) -> crate::error::Result<()> {
    use std::process::Stdio;

    let output = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(crate::error::ClawMuxError::Io)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(crate::error::ClawMuxError::Internal(format!(
            "git add -A failed: {stderr}"
        )));
    }

    let output = tokio::process::Command::new("git")
        .args(["commit", "-m", commit_message])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(crate::error::ClawMuxError::Io)?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(crate::error::ClawMuxError::Internal(format!(
            "git commit failed: {stderr}"
        )))
    }
}

/// A no-op backend used in tests and when no backend is configured.
///
/// All session operations that require communication immediately send an error
/// message via `async_tx`.  Read-only operations (diffs, question replies, etc.)
/// are silently skipped.
#[allow(dead_code)]
pub struct NullBackend;

impl AgentBackend for NullBackend {
    fn create_session(
        &self,
        task_id: TaskId,
        _agent: AgentKind,
        _prompt: String,
        _model: Option<ModelId>,
        _session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = async_tx
                    .send(AppMessage::SessionError {
                        task_id,
                        session_id: String::new(),
                        error: "No backend configured".to_string(),
                    })
                    .await;
            });
        }
    }

    fn create_idle_session(
        &self,
        _task_id: TaskId,
        _agent: AgentKind,
        _session_map: SessionMap,
        _async_tx: mpsc::Sender<AppMessage>,
    ) {
        // No-op: without a backend there is no session to create.
    }

    fn send_prompt(
        &self,
        task_id: TaskId,
        session_id: String,
        _agent: AgentKind,
        _prompt: String,
        _model: Option<ModelId>,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = async_tx
                    .send(AppMessage::SessionError {
                        task_id,
                        session_id,
                        error: "No backend configured".to_string(),
                    })
                    .await;
            });
        }
    }

    fn abort_session(
        &self,
        _task_id: TaskId,
        _session_id: String,
        _async_tx: mpsc::Sender<AppMessage>,
    ) {
        // No-op.
    }

    fn resolve_permission(
        &self,
        _task_id: TaskId,
        _request: PermissionRequest,
        _response: String,
        send_prompt_msg: Option<AppMessage>,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        // No real permission to resolve; dispatch the optional steering prompt if present.
        if let Some(msg) = send_prompt_msg {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = async_tx.send(msg).await;
                });
            }
        }
    }

    fn get_diffs(
        &self,
        _task_id: TaskId,
        _session_id: String,
        _async_tx: mpsc::Sender<AppMessage>,
    ) {
        // No-op.
    }

    fn reply_question(
        &self,
        _task_id: TaskId,
        _request_id: String,
        _answer: String,
        _async_tx: mpsc::Sender<AppMessage>,
    ) {
        // No-op.
    }

    fn check_session_statuses(
        &self,
        _sessions: Vec<(TaskId, String)>,
        _async_tx: mpsc::Sender<AppMessage>,
    ) {
        // No-op.
    }

    fn commit_changes(
        &self,
        task_id: TaskId,
        _commit_message: String,
        _file_paths: Vec<String>,
        _model: Option<ModelId>,
        _session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = async_tx
                    .send(AppMessage::CommitFailed {
                        task_id,
                        error: "No backend configured".to_string(),
                    })
                    .await;
            });
        }
    }

    fn is_available(&self) -> bool {
        false
    }

    fn name(&self) -> &str {
        "null"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that NullBackend reports not available and has the expected name.
    #[test]
    fn test_null_backend_is_available_false() {
        let b = NullBackend;
        assert!(!b.is_available());
        assert_eq!(b.name(), "null");
    }

    /// Verifies that NullBackend::create_session sends a SessionError.
    #[tokio::test]
    async fn test_null_backend_create_session_sends_error() {
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let (tx, mut rx) = mpsc::channel(8);
        let session_map = Arc::new(RwLock::new(HashMap::new()));
        let b = NullBackend;
        b.create_session(
            TaskId::from_path("tasks/1.1.md"),
            AgentKind::Intake,
            "test prompt".to_string(),
            None,
            session_map,
            tx,
        );
        let msg = rx.recv().await.expect("expected a message");
        assert!(
            matches!(msg, AppMessage::SessionError { .. }),
            "expected SessionError, got: {:?}",
            msg
        );
    }

    /// Verifies that NullBackend::commit_changes sends a CommitFailed.
    #[tokio::test]
    async fn test_null_backend_commit_changes_sends_error() {
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let (tx, mut rx) = mpsc::channel(8);
        let session_map = Arc::new(RwLock::new(HashMap::new()));
        let b = NullBackend;
        b.commit_changes(
            TaskId::from_path("tasks/1.1.md"),
            "feat: test".to_string(),
            vec![],
            None,
            session_map,
            tx,
        );
        let msg = rx.recv().await.expect("expected a message");
        assert!(
            matches!(msg, AppMessage::CommitFailed { .. }),
            "expected CommitFailed, got: {:?}",
            msg
        );
    }
}
