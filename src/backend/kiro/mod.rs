//! Kiro-CLI backend implementation for the AgentBackend trait.
//!
//! This module provides [`KiroBackend`] that communicates with kiro-cli
//! via the Agent Client Protocol (ACP) -- JSON-RPC 2.0 over stdin/stdout.
//!
//! # Architecture
//!
//! - [`KiroBackend`] manages a pool of [`KiroProcess`] instances, one per active session.
//! - Each process handles a single agent stage's session lifecycle.
//! - Streaming text, tool activity, and completion signals are translated from
//!   ACP notifications to [`AppMessage`] variants in [`events`].
//! - Permission requests use a per-process channel to route the user's decision
//!   back to the blocking JSON-RPC response.
//! - Diffs are produced via `git diff HEAD` (no ACP diff endpoint).
//! - Git commits are performed directly via `git` commands (no agent needed).

pub mod events;
pub mod process;
pub mod transport;
pub mod types;

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;

use tokio::sync::{mpsc, Mutex};

use crate::backend::AgentBackend;
use crate::messages::AppMessage;
use crate::opencode::events::SessionMap;
use crate::opencode::types::{
    DiffHunk, DiffLine, DiffLineKind, DiffStatus, FileDiff, ModelId, PermissionRequest,
};
use crate::tasks::models::TaskId;
use crate::workflow::agents::AgentKind;

use self::process::{check_kiro_available, resolve_binary, KiroProcess};

/// Kiro-CLI agent backend implementation.
///
/// Manages kiro-cli child processes via the Agent Client Protocol (ACP).
/// One process is spawned per agent stage; processes are keyed by `session_id`.
pub struct KiroBackend {
    /// Resolved path or name of the kiro-cli binary.
    binary: String,
    /// Absolute path to the project working directory, sent in `initialize`.
    cwd: String,
    /// Active KiroProcess instances keyed by ACP session ID.
    processes: Arc<Mutex<HashMap<String, KiroProcess>>>,
}

impl KiroBackend {
    /// Create a new `KiroBackend`.
    ///
    /// # Arguments
    /// * `binary` – optional kiro binary path; defaults to `"kiro"` (PATH lookup).
    /// * `cwd` – absolute path to the project working directory.
    pub fn new(binary: Option<String>, cwd: String) -> Self {
        Self {
            binary: resolve_binary(binary.as_deref()),
            cwd,
            processes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Check whether the kiro binary is reachable.
    fn binary_available(&self) -> bool {
        check_kiro_available(&self.binary)
    }
}

impl AgentBackend for KiroBackend {
    fn create_session(
        &self,
        task_id: TaskId,
        agent: AgentKind,
        prompt: String,
        _model: Option<ModelId>,
        session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let binary = self.binary.clone();
        let cwd = self.cwd.clone();
        let processes = self.processes.clone();
        tokio::spawn(async move {
            let agent_name = agent.kiro_agent_name();
            let process = match KiroProcess::spawn(
                &binary,
                agent_name,
                &cwd,
                task_id.clone(),
                async_tx.clone(),
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("failed to spawn kiro-cli for task {task_id}: {e}");
                    let _ = async_tx
                        .send(AppMessage::SessionError {
                            task_id,
                            session_id: String::new(),
                            error: e.to_string(),
                        })
                        .await;
                    return;
                }
            };

            let session_id = process.session_id().to_string();

            // Register session_id -> (task_id, agent) in the shared session map
            {
                let mut map = session_map.write().await;
                map.insert(session_id.clone(), (task_id.clone(), agent));
            }

            // Notify the workflow engine so it can track this session_id.
            // This is required for the SessionCompleted guard to reject stale
            // completions from old sessions when a new session takes over.
            let _ = async_tx
                .send(AppMessage::SessionCreated {
                    task_id: task_id.clone(),
                    session_id: session_id.clone(),
                })
                .await;

            // Send the initial prompt
            if let Err(e) = process.send_prompt(&prompt, async_tx.clone()).await {
                tracing::error!("failed to send prompt to kiro for task {task_id}: {e}");
                let _ = async_tx
                    .send(AppMessage::SessionError {
                        task_id,
                        session_id: session_id.clone(),
                        error: e.to_string(),
                    })
                    .await;
                process.shutdown().await;
                return;
            }

            let _ = async_tx
                .send(AppMessage::PromptSent {
                    task_id: task_id.clone(),
                    session_id: session_id.clone(),
                })
                .await;

            processes.lock().await.insert(session_id, process);
        });
    }

    fn create_idle_session(
        &self,
        task_id: TaskId,
        agent: AgentKind,
        session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let binary = self.binary.clone();
        let cwd = self.cwd.clone();
        let processes = self.processes.clone();
        tokio::spawn(async move {
            let agent_name = agent.kiro_agent_name();
            let process = match KiroProcess::spawn(
                &binary,
                agent_name,
                &cwd,
                task_id.clone(),
                async_tx.clone(),
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    tracing::error!("failed to spawn idle kiro-cli for task {task_id}: {e}");
                    let _ = async_tx
                        .send(AppMessage::SessionError {
                            task_id,
                            session_id: String::new(),
                            error: e.to_string(),
                        })
                        .await;
                    return;
                }
            };

            let session_id = process.session_id().to_string();
            {
                let mut map = session_map.write().await;
                map.insert(session_id.clone(), (task_id.clone(), agent));
            }

            let _ = async_tx
                .send(AppMessage::SessionCreated {
                    task_id: task_id.clone(),
                    session_id: session_id.clone(),
                })
                .await;

            processes.lock().await.insert(session_id, process);
        });
    }

    fn send_prompt(
        &self,
        task_id: TaskId,
        session_id: String,
        _agent: AgentKind,
        prompt: String,
        _model: Option<ModelId>,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        let processes = self.processes.clone();
        tokio::spawn(async move {
            let result = {
                let map = processes.lock().await;
                if let Some(process) = map.get(&session_id) {
                    process.send_prompt(&prompt, async_tx.clone()).await
                } else {
                    Err(crate::error::ClawMuxError::Kiro(format!(
                        "no active kiro session for session_id={session_id}"
                    )))
                }
            };

            if let Err(e) = result {
                tracing::error!("failed to send prompt to kiro session {session_id}: {e}");
                let _ = async_tx
                    .send(AppMessage::SessionError {
                        task_id,
                        session_id,
                        error: e.to_string(),
                    })
                    .await;
            } else {
                let _ = async_tx
                    .send(AppMessage::PromptSent {
                        task_id,
                        session_id,
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
        let processes = self.processes.clone();
        tokio::spawn(async move {
            let result = {
                let map = processes.lock().await;
                if let Some(process) = map.get(&session_id) {
                    process.cancel().await
                } else {
                    // Session already gone -- not an error
                    tracing::debug!(
                        "abort_session: no active kiro session for {session_id}, already gone"
                    );
                    return;
                }
            };
            if let Err(e) = result {
                tracing::warn!("failed to cancel kiro session {session_id}: {e}");
                let _ = async_tx
                    .send(AppMessage::SessionError {
                        task_id,
                        session_id,
                        error: e.to_string(),
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
        let processes = self.processes.clone();
        let session_id = request.session_id.clone();
        let rpc_id_str = request.id.clone();

        tokio::spawn(async move {
            let result = {
                let map = processes.lock().await;
                if let Some(process) = map.get(&session_id) {
                    process.resolve_permission(&rpc_id_str, &response).await
                } else {
                    tracing::warn!("resolve_permission: no active kiro session for {session_id}");
                    Ok(())
                }
            };

            if let Err(e) = result {
                tracing::warn!("failed to resolve permission for session {session_id}: {e}");
                // Re-emit the permission request so the user can retry
                let _ = async_tx
                    .send(AppMessage::PermissionAsked { task_id, request })
                    .await;
                return;
            }

            // For rejections with an explanation, send the steering prompt
            if response == "reject" {
                if let Some(msg) = send_prompt_msg {
                    let _ = async_tx.send(msg).await;
                }
            }
        });
    }

    fn get_diffs(&self, task_id: TaskId, _session_id: String, async_tx: mpsc::Sender<AppMessage>) {
        tokio::spawn(async move {
            match run_git_diff().await {
                Ok(diffs) => {
                    let _ = async_tx
                        .send(AppMessage::DiffReady { task_id, diffs })
                        .await;
                }
                Err(e) => {
                    tracing::warn!("kiro get_diffs failed: {e}");
                    // Send empty diffs rather than an error -- diffs are best-effort
                    let _ = async_tx
                        .send(AppMessage::DiffReady {
                            task_id,
                            diffs: vec![],
                        })
                        .await;
                }
            }
        });
    }

    fn reply_question(
        &self,
        task_id: TaskId,
        _request_id: String,
        answer: String,
        async_tx: mpsc::Sender<AppMessage>,
    ) {
        // Kiro doesn't have a separate question/answer API -- send answer as a follow-up prompt.
        // We don't have the session_id here, so we log a warning and no-op.
        // The TUI's Send Prompt path (via send_prompt) is the correct channel.
        tracing::debug!(
            "reply_question for kiro task {task_id}: answer will be sent as a follow-up prompt"
        );
        let _ = (answer, async_tx); // explicitly consumed
    }

    fn check_session_statuses(
        &self,
        _sessions: Vec<(TaskId, String)>,
        _async_tx: mpsc::Sender<AppMessage>,
    ) {
        // Kiro sessions are tracked via the event loop (run_event_loop sends
        // SessionCompleted/SessionError directly). No HTTP polling needed.
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
            match run_git_commit(&commit_message).await {
                Ok(()) => {
                    let _ = async_tx.send(AppMessage::CommitCompleted { task_id }).await;
                }
                Err(e) => {
                    tracing::error!("kiro commit failed for task {task_id}: {e}");
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
        self.binary_available()
    }

    fn name(&self) -> &str {
        "kiro"
    }
}

// ---------------------------------------------------------------------------
// Git helpers
// ---------------------------------------------------------------------------

/// Run `git diff HEAD` and parse the output into [`FileDiff`] structs.
async fn run_git_diff() -> crate::error::Result<Vec<FileDiff>> {
    let output = tokio::process::Command::new("git")
        .args(["diff", "HEAD", "--unified=3"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .map_err(crate::error::ClawMuxError::Io)?;

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(parse_unified_diff(&text))
}

/// Parse a unified diff string into a list of [`FileDiff`] structs.
pub(crate) fn parse_unified_diff(diff: &str) -> Vec<FileDiff> {
    let mut result: Vec<FileDiff> = Vec::new();
    let mut current_file: Option<FileDiff> = None;
    let mut current_hunk: Option<DiffHunk> = None;
    // Track file status separately because "new file mode" / "deleted file mode"
    // appear before the "+++ b/..." line that creates current_file.
    let mut pending_status = DiffStatus::Modified;

    for line in diff.lines() {
        if line.starts_with("diff --git") {
            // Flush current hunk and file
            if let Some(hunk) = current_hunk.take() {
                if let Some(ref mut file) = current_file {
                    file.hunks.push(hunk);
                }
            }
            if let Some(file) = current_file.take() {
                result.push(file);
            }
            pending_status = DiffStatus::Modified;
        } else if line.starts_with("--- ") {
            // Ignore: file header handled by +++ line
        } else if line.starts_with("+++ ") {
            let path = line
                .strip_prefix("+++ b/")
                .or_else(|| line.strip_prefix("+++ "))
                .unwrap_or_else(|| line.trim_start_matches('+').trim_start_matches(' '))
                .to_string();
            current_file = Some(FileDiff {
                path,
                status: pending_status.clone(),
                hunks: Vec::new(),
            });
        } else if line.starts_with("new file mode") {
            pending_status = DiffStatus::Added;
        } else if line.starts_with("deleted file mode") {
            pending_status = DiffStatus::Deleted;
        } else if line.starts_with("@@ ") {
            // Flush previous hunk
            if let Some(hunk) = current_hunk.take() {
                if let Some(ref mut file) = current_file {
                    file.hunks.push(hunk);
                }
            }
            let (old_start, new_start) = parse_hunk_header(line);
            current_hunk = Some(DiffHunk {
                old_start,
                new_start,
                lines: Vec::new(),
            });
        } else if let Some(ref mut hunk) = current_hunk {
            let (kind, content) = if let Some(c) = line.strip_prefix('+') {
                (DiffLineKind::Added, c.to_string())
            } else if let Some(c) = line.strip_prefix('-') {
                (DiffLineKind::Removed, c.to_string())
            } else if let Some(c) = line.strip_prefix(' ') {
                (DiffLineKind::Context, c.to_string())
            } else {
                // Hunk end marker "\ No newline at end of file" etc.
                continue;
            };
            hunk.lines.push(DiffLine { kind, content });
        }
    }

    // Flush remaining hunk and file
    if let Some(hunk) = current_hunk.take() {
        if let Some(ref mut file) = current_file {
            file.hunks.push(hunk);
        }
    }
    if let Some(file) = current_file.take() {
        result.push(file);
    }

    result
}

/// Parse a hunk header line like `@@ -10,5 +10,6 @@` into `(old_start, new_start)`.
fn parse_hunk_header(line: &str) -> (u32, u32) {
    // Format: @@ -<old_start>[,<old_count>] +<new_start>[,<new_count>] @@
    let parts: Vec<&str> = line.splitn(5, ' ').collect();
    // parts[1] = "-10,5", parts[2] = "+10,6"
    let old_start = parts
        .get(1)
        .and_then(|s| s.trim_start_matches('-').split(',').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    let new_start = parts
        .get(2)
        .and_then(|s| s.trim_start_matches('+').split(',').next())
        .and_then(|n| n.parse().ok())
        .unwrap_or(1);
    (old_start, new_start)
}

/// Perform a git add + commit directly (no agent required).
///
/// Stages all working-tree changes via `git add -A`. We assume only one task
/// is worked on at a time in this workspace, so all pending changes belong to
/// the current task.
async fn run_git_commit(commit_message: &str) -> crate::error::Result<()> {
    // Stage all working-tree changes.
    let status = tokio::process::Command::new("git")
        .args(["add", "-A"])
        .status()
        .await
        .map_err(crate::error::ClawMuxError::Io)?;
    if !status.success() {
        return Err(crate::error::ClawMuxError::Kiro(
            "git add -A failed".to_string(),
        ));
    }

    // Commit
    let status = tokio::process::Command::new("git")
        .args(["commit", "-m", commit_message])
        .status()
        .await
        .map_err(crate::error::ClawMuxError::Io)?;

    if status.success() {
        Ok(())
    } else {
        Err(crate::error::ClawMuxError::Kiro(
            "git commit failed".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kiro_backend_name() {
        let b = KiroBackend::new(None, "/tmp".to_string());
        assert_eq!(b.name(), "kiro");
    }

    #[test]
    fn test_kiro_backend_binary_default() {
        let b = KiroBackend::new(None, "/tmp".to_string());
        assert_eq!(b.binary, "kiro-cli");
    }

    #[test]
    fn test_kiro_backend_binary_custom() {
        let b = KiroBackend::new(Some("/opt/kiro/bin/kiro".to_string()), "/tmp".to_string());
        assert_eq!(b.binary, "/opt/kiro/bin/kiro");
    }

    #[test]
    fn test_kiro_backend_is_available_nonexistent() {
        // Unless kiro is actually installed, should return false
        let b = KiroBackend::new(
            Some("this-binary-does-not-exist-99999".to_string()),
            "/tmp".to_string(),
        );
        assert!(!b.is_available());
    }

    #[test]
    fn test_parse_unified_diff_empty() {
        let diffs = parse_unified_diff("");
        assert!(diffs.is_empty());
    }

    #[test]
    fn test_parse_unified_diff_single_file_modified() {
        let diff = "\
diff --git a/src/foo.rs b/src/foo.rs
--- a/src/foo.rs
+++ b/src/foo.rs
@@ -1,3 +1,4 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"hello world\");
+    // new comment
 }
";
        let diffs = parse_unified_diff(diff);
        assert_eq!(diffs.len(), 1);
        let file = &diffs[0];
        assert_eq!(file.path, "src/foo.rs");
        assert!(matches!(file.status, DiffStatus::Modified));
        assert_eq!(file.hunks.len(), 1);
        let hunk = &file.hunks[0];
        assert_eq!(hunk.old_start, 1);
        assert_eq!(hunk.new_start, 1);
        // context, removed, added, added, context
        assert_eq!(hunk.lines.len(), 5);
        assert!(matches!(hunk.lines[0].kind, DiffLineKind::Context));
        assert!(matches!(hunk.lines[1].kind, DiffLineKind::Removed));
        assert!(matches!(hunk.lines[2].kind, DiffLineKind::Added));
        assert!(matches!(hunk.lines[3].kind, DiffLineKind::Added));
        assert!(matches!(hunk.lines[4].kind, DiffLineKind::Context));
    }

    #[test]
    fn test_parse_unified_diff_new_file() {
        let diff = "\
diff --git a/new_file.rs b/new_file.rs
new file mode 100644
--- /dev/null
+++ b/new_file.rs
@@ -0,0 +1,2 @@
+fn hello() {}
+fn world() {}
";
        let diffs = parse_unified_diff(diff);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0].status, DiffStatus::Added));
        assert_eq!(diffs[0].hunks[0].lines.len(), 2);
        assert!(diffs[0].hunks[0]
            .lines
            .iter()
            .all(|l| matches!(l.kind, DiffLineKind::Added)));
    }

    #[test]
    fn test_parse_unified_diff_deleted_file() {
        let diff = "\
diff --git a/old.rs b/old.rs
deleted file mode 100644
--- a/old.rs
+++ /dev/null
@@ -1,2 +0,0 @@
-fn old() {}
-fn code() {}
";
        let diffs = parse_unified_diff(diff);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(diffs[0].status, DiffStatus::Deleted));
        assert!(diffs[0].hunks[0]
            .lines
            .iter()
            .all(|l| matches!(l.kind, DiffLineKind::Removed)));
    }

    #[test]
    fn test_parse_unified_diff_multiple_files() {
        let diff = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1,1 +1,1 @@
-old
+new
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1,1 +1,1 @@
-foo
+bar
";
        let diffs = parse_unified_diff(diff);
        assert_eq!(diffs.len(), 2);
        assert_eq!(diffs[0].path, "a.rs");
        assert_eq!(diffs[1].path, "b.rs");
    }

    #[test]
    fn test_parse_hunk_header() {
        assert_eq!(parse_hunk_header("@@ -10,5 +10,6 @@"), (10, 10));
        assert_eq!(parse_hunk_header("@@ -1,3 +1,4 @@"), (1, 1));
        assert_eq!(parse_hunk_header("@@ -0,0 +1,2 @@"), (0, 1));
        assert_eq!(parse_hunk_header("@@ -100 +100 @@"), (100, 100));
    }

    #[test]
    fn test_parse_unified_diff_multiple_hunks() {
        let diff = "\
diff --git a/file.rs b/file.rs
--- a/file.rs
+++ b/file.rs
@@ -1,3 +1,3 @@
 line1
-old line2
+new line2
 line3
@@ -10,3 +10,4 @@
 line10
+extra line
 line11
 line12
";
        let diffs = parse_unified_diff(diff);
        assert_eq!(diffs.len(), 1);
        assert_eq!(diffs[0].hunks.len(), 2);
        assert_eq!(diffs[0].hunks[0].old_start, 1);
        assert_eq!(diffs[0].hunks[1].old_start, 10);
    }
}
