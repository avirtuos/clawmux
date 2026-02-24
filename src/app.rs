//! Top-level application state and message dispatcher.
//!
//! The `App` struct holds all runtime state and coordinates between subsystems:
//! the TUI layer, workflow engine, task store, and opencode client.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::messages::AppMessage;
use crate::opencode::events::SessionMap;
use crate::opencode::OpenCodeClient;
use crate::tasks::models::SuggestedFix;
use crate::tasks::{Story, TaskId, TaskStore};
use crate::tui::tabs::agent_activity::Tab2State;
use crate::tui::tabs::code_review::Tab4State;
use crate::tui::tabs::task_details::Tab1State;
use crate::tui::tabs::team_status::Tab3State;
use crate::tui::task_list::TaskListState;
use crate::workflow::transitions::WorkflowEngine;

/// Top-level application state.
///
/// Coordinates the TUI, workflow engine, task store, and opencode client
/// via the [`AppMessage`] dispatch loop.
#[allow(dead_code)]
pub struct App {
    /// In-memory store for all loaded stories and tasks.
    pub task_store: TaskStore,
    /// Cached snapshot of stories from `task_store`, rebuilt by [`App::refresh_stories`].
    pub cached_stories: Vec<Story>,
    /// Index of the active tab in the right pane (0-based).
    pub active_tab: usize,
    /// When `true`, the event loop should exit and the TUI should shut down.
    pub should_quit: bool,
    /// When `true`, the quit confirmation dialog is displayed and input is intercepted.
    pub show_quit_confirm: bool,
    /// Navigation and expansion state for the left-pane task list widget.
    pub task_list_state: TaskListState,
    /// UI state for Tab 1 (Task Details): prompt input, answer inputs, focus flags.
    pub tab1_state: Tab1State,
    /// UI state for Tab 2 (Agent Activity): per-task activity lines and scroll.
    pub tab2_state: Tab2State,
    /// Pure state machine driving tasks through the 7-agent pipeline.
    pub workflow_engine: WorkflowEngine,
    /// UI state for Tab 3 (Team Status): work log scroll and current task.
    pub tab3_state: Tab3State,
    /// UI state for Tab 4 (Code Review): per-task diff storage.
    pub tab4_state: Tab4State,
    /// Shared HTTP client for async opencode session operations.
    pub opencode_client: Option<Arc<OpenCodeClient>>,
    /// Shared map from session ID to (TaskId, AgentKind), used by EventStreamConsumer.
    pub session_map: SessionMap,
    /// Sender used by tokio::spawn callbacks to post results back to the main loop.
    pub async_tx: mpsc::Sender<AppMessage>,
    /// Buffer of messages produced by spawned tasks, drained on each Tick.
    pub pending_messages: Vec<AppMessage>,
}

impl App {
    /// Creates a new `App` with the given task store and default UI state.
    ///
    /// # Arguments
    ///
    /// * `task_store` - The in-memory task store loaded from disk.
    /// * `opencode_client` - Optional shared HTTP client for opencode session operations.
    /// * `session_map` - Shared map correlating session IDs to tasks and agents.
    /// * `async_tx` - Channel sender for routing async task results back to the event loop.
    pub fn new(
        task_store: TaskStore,
        opencode_client: Option<Arc<OpenCodeClient>>,
        session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
    ) -> Self {
        let cached_stories = task_store.stories();
        let mut task_list_state = TaskListState::new();
        task_list_state.refresh(&cached_stories);
        App {
            task_store,
            cached_stories,
            active_tab: 0,
            should_quit: false,
            show_quit_confirm: false,
            task_list_state,
            tab1_state: Tab1State::new(),
            tab2_state: Tab2State::new(),
            workflow_engine: WorkflowEngine::new(),
            tab3_state: Tab3State::new(),
            tab4_state: Tab4State::new(),
            opencode_client,
            session_map,
            async_tx,
            pending_messages: Vec::new(),
        }
    }

    /// Refreshes [`cached_stories`](App::cached_stories) from the task store and rebuilds
    /// the task list widget state.
    ///
    /// Call this after any mutation to `task_store` to keep the TUI in sync.
    pub fn refresh_stories(&mut self) {
        self.cached_stories = self.task_store.stories();
        self.task_list_state.refresh(&self.cached_stories);
    }

    /// Dismisses the quit confirmation dialog without quitting.
    pub fn dismiss_quit_confirm(&mut self) {
        self.show_quit_confirm = false;
    }

    /// Returns the [`TaskId`] of the currently selected task, or `None` if on a story.
    ///
    /// Derived from [`TaskListState::selected_task_id`].
    #[allow(dead_code)]
    pub fn selected_task(&self) -> Option<&TaskId> {
        self.task_list_state.selected_task_id()
    }

    /// Processes a single [`AppMessage`], mutating state and returning
    /// any follow-up messages to dispatch.
    pub fn handle_message(&mut self, msg: AppMessage) -> Vec<AppMessage> {
        match msg {
            // --- Application lifecycle ---
            AppMessage::Shutdown => {
                if self.show_quit_confirm {
                    self.should_quit = true;
                } else {
                    self.show_quit_confirm = true;
                }
                vec![]
            }
            AppMessage::TerminalEvent(event) => {
                if let Some(response) = crate::tui::handle_input(event, self) {
                    vec![response]
                } else {
                    vec![]
                }
            }
            AppMessage::Tick => std::mem::take(&mut self.pending_messages),

            // --- Streaming/tool activity ---
            AppMessage::StreamingUpdate {
                task_id,
                session_id: _,
                parts,
            } => {
                self.tab2_state.push_streaming(&task_id, &parts);
                vec![]
            }
            AppMessage::ToolActivity {
                task_id,
                session_id: _,
                tool,
                status,
            } => {
                self.tab2_state.push_tool(&task_id, tool, status);
                vec![]
            }

            // --- Malformed task fix ---
            AppMessage::RequestTaskFix { task_id } => {
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    if let Some(ref mut err_info) = task.parse_error {
                        err_info.fix_in_progress = true;
                        err_info.suggested_fix = None;
                    }
                }
                vec![]
            }
            AppMessage::TaskFixReady {
                task_id,
                corrected_content,
                explanation,
            } => {
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    if let Some(ref mut err_info) = task.parse_error {
                        err_info.fix_in_progress = false;
                        err_info.suggested_fix = Some(SuggestedFix {
                            corrected_content,
                            explanation,
                        });
                    }
                }
                vec![]
            }
            AppMessage::TaskFixFailed { task_id, error } => {
                tracing::warn!("AI fix request failed for task {}: {}", task_id, error);
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    if let Some(ref mut err_info) = task.parse_error {
                        err_info.fix_in_progress = false;
                    }
                }
                vec![]
            }
            AppMessage::ApplyTaskFix { task_id } => {
                // Extract the corrected content and file path from the task.
                let (corrected, file_path) = {
                    let task = match self.task_store.get(&task_id) {
                        Some(t) => t,
                        None => return vec![],
                    };
                    let corrected = match task
                        .parse_error
                        .as_ref()
                        .and_then(|e| e.suggested_fix.as_ref())
                        .map(|f| f.corrected_content.clone())
                    {
                        Some(c) => c,
                        None => return vec![],
                    };
                    (corrected, task.file_path.clone())
                };

                // Write corrected content to disk.
                if let Err(e) = std::fs::write(&file_path, &corrected) {
                    tracing::warn!("Failed to write fix for {}: {}", file_path.display(), e);
                    return vec![];
                }

                // Re-parse the file. On success, replace the task (clears parse_error).
                // On failure, insert a new malformed stub.
                let new_task = match crate::tasks::parser::parse_task(&corrected, file_path.clone())
                {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!(
                            "Fix did not resolve parse error for {}: {}",
                            file_path.display(),
                            e
                        );
                        crate::tasks::parser::create_malformed_task(
                            &corrected,
                            file_path,
                            e.to_string(),
                        )
                    }
                };
                self.task_store.insert(new_task);
                self.refresh_stories();
                vec![]
            }

            // --- Workflow messages: forward to engine ---
            AppMessage::StartTask { .. }
            | AppMessage::AgentCompleted { .. }
            | AppMessage::AgentKickedBack { .. }
            | AppMessage::AgentAskedQuestion { .. }
            | AppMessage::HumanAnswered { .. }
            | AppMessage::HumanApprovedReview { .. }
            | AppMessage::HumanRequestedRevisions { .. }
            | AppMessage::SessionCreated { .. }
            | AppMessage::SessionCompleted { .. }
            | AppMessage::SessionError { .. } => self.workflow_engine.process(msg),

            // --- Async session operations ---
            AppMessage::CreateSession {
                task_id,
                agent,
                prompt,
            } => {
                let client = match self.opencode_client.clone() {
                    Some(c) => c,
                    None => {
                        return vec![AppMessage::SessionError {
                            task_id,
                            session_id: String::new(),
                            error: "OpenCode client unavailable".to_string(),
                        }];
                    }
                };
                let async_tx = self.async_tx.clone();
                let session_map = self.session_map.clone();
                tokio::spawn(async move {
                    let session = match client.create_session().await {
                        Ok(s) => s,
                        Err(e) => {
                            let _ = async_tx
                                .send(AppMessage::SessionError {
                                    task_id,
                                    session_id: String::new(),
                                    error: format!("Failed to create session: {}", e),
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
                    if let Err(e) = client.send_prompt_async(&session.id, &agent, &prompt).await {
                        let _ = async_tx
                            .send(AppMessage::SessionError {
                                task_id,
                                session_id: session.id,
                                error: format!("Failed to send prompt: {}", e),
                            })
                            .await;
                        return;
                    }
                    let _ = async_tx
                        .send(AppMessage::SessionCreated {
                            task_id,
                            session_id: session.id,
                        })
                        .await;
                });
                vec![]
            }

            AppMessage::SendPrompt {
                task_id,
                session_id,
                prompt,
            } => {
                let client = match self.opencode_client.clone() {
                    Some(c) => c,
                    None => {
                        return vec![AppMessage::SessionError {
                            task_id,
                            session_id,
                            error: "OpenCode client unavailable".to_string(),
                        }];
                    }
                };
                let async_tx = self.async_tx.clone();
                let session_map = self.session_map.clone();
                tokio::spawn(async move {
                    let agent = {
                        let map = session_map.read().await;
                        map.get(&session_id).map(|(_, a)| *a)
                    };
                    let agent = match agent {
                        Some(a) => a,
                        None => {
                            let _ = async_tx
                                .send(AppMessage::SessionError {
                                    task_id,
                                    session_id,
                                    error: "Session not found in session map".to_string(),
                                })
                                .await;
                            return;
                        }
                    };
                    if let Err(e) = client.send_prompt_async(&session_id, &agent, &prompt).await {
                        let _ = async_tx
                            .send(AppMessage::SessionError {
                                task_id,
                                session_id,
                                error: format!("Failed to send prompt: {}", e),
                            })
                            .await;
                    }
                });
                vec![]
            }

            AppMessage::AbortSession {
                task_id,
                session_id,
            } => {
                let client = match self.opencode_client.clone() {
                    Some(c) => c,
                    None => {
                        return vec![AppMessage::SessionError {
                            task_id,
                            session_id,
                            error: "OpenCode client unavailable".to_string(),
                        }];
                    }
                };
                let async_tx = self.async_tx.clone();
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
                vec![]
            }

            // --- Task persistence ---
            AppMessage::TaskUpdated { task_id } => {
                if let Err(e) = self.task_store.persist(&task_id) {
                    tracing::warn!("Failed to persist task {}: {}", task_id, e);
                }
                self.refresh_stories();
                vec![]
            }
            AppMessage::TaskFileChanged { task_id } => {
                if let Err(e) = self.task_store.reload(&task_id) {
                    tracing::warn!("Failed to reload task {}: {}", task_id, e);
                }
                self.refresh_stories();
                vec![]
            }

            // --- Diff storage ---
            AppMessage::DiffReady { task_id, diffs } => {
                self.tab4_state.set_diffs(&task_id, diffs);
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use tokio::sync::RwLock;

    use super::*;

    fn test_app() -> App {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let session_map = Arc::new(RwLock::new(HashMap::new()));
        App::new(TaskStore::new(), None, session_map, tx)
    }

    #[test]
    fn test_app_new() {
        let app = test_app();
        assert!(app.selected_task().is_none());
        assert_eq!(app.active_tab, 0);
        assert!(!app.should_quit);
        assert!(!app.show_quit_confirm);
        assert_eq!(app.task_list_state.selected_index, 0);
        assert!(app.task_list_state.expanded_stories.is_empty());
    }

    #[test]
    fn test_handle_message_shutdown() {
        let mut app = test_app();
        let responses = app.handle_message(AppMessage::Shutdown);
        // First Shutdown shows the dialog, does not quit.
        assert!(!app.should_quit);
        assert!(app.show_quit_confirm);
        assert!(responses.is_empty());
    }

    #[test]
    fn test_handle_message_shutdown_confirm() {
        let mut app = test_app();
        // First Shutdown shows dialog.
        app.handle_message(AppMessage::Shutdown);
        assert!(app.show_quit_confirm);
        assert!(!app.should_quit);
        // Second Shutdown (with dialog visible) confirms quit.
        let responses = app.handle_message(AppMessage::Shutdown);
        assert!(app.should_quit);
        assert!(responses.is_empty());
    }

    #[test]
    fn test_dismiss_quit_confirm() {
        let mut app = test_app();
        app.show_quit_confirm = true;
        app.dismiss_quit_confirm();
        assert!(!app.show_quit_confirm);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_handle_message_tick_drains_pending() {
        let mut app = test_app();
        // Inject a pending message to verify Tick drains the buffer.
        app.pending_messages.push(AppMessage::TaskUpdated {
            task_id: TaskId::from_path("tasks/1.1.md"),
        });
        let responses = app.handle_message(AppMessage::Tick);
        // pending_messages is returned as follow-up work.
        assert_eq!(responses.len(), 1);
        assert!(app.pending_messages.is_empty());
    }

    /// Verifies two-phase quit: first Shutdown shows dialog, second sets should_quit.
    #[test]
    fn test_handle_shutdown() {
        let mut app = test_app();
        assert!(!app.show_quit_confirm);
        assert!(!app.should_quit);

        let msgs = app.handle_message(AppMessage::Shutdown);
        assert!(msgs.is_empty());
        assert!(app.show_quit_confirm);
        assert!(!app.should_quit);

        let msgs = app.handle_message(AppMessage::Shutdown);
        assert!(msgs.is_empty());
        assert!(app.should_quit);
    }

    /// Verifies that pressing q emits a Shutdown message via TerminalEvent.
    #[test]
    fn test_handle_terminal_event_q_key() {
        use crossterm::event::{
            Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers,
        };

        let mut app = test_app();
        let key_event = Event::Key(KeyEvent {
            code: KeyCode::Char('q'),
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        });
        let msgs = app.handle_message(AppMessage::TerminalEvent(key_event));
        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], AppMessage::Shutdown));
    }

    /// Verifies that StartTask causes the workflow engine to emit CreateSession for Intake.
    #[test]
    fn test_handle_start_task_emits_create_session() {
        let mut app = test_app();
        let task_id = TaskId::from_path("tasks/1.1.md");
        let msgs = app.handle_message(AppMessage::StartTask {
            task_id: task_id.clone(),
        });
        assert_eq!(msgs.len(), 1);
        assert!(
            matches!(
                &msgs[0],
                AppMessage::CreateSession { agent, .. }
                    if *agent == crate::workflow::agents::AgentKind::Intake
            ),
            "expected CreateSession for Intake, got: {:?}",
            msgs[0]
        );
    }

    /// Verifies that StreamingUpdate with text parts updates tab2_state.
    #[test]
    fn test_handle_streaming_update_updates_tab2() {
        use crate::opencode::types::MessagePart;

        let mut app = test_app();
        let task_id = TaskId::from_path("tasks/1.1.md");
        let msgs = app.handle_message(AppMessage::StreamingUpdate {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            parts: vec![MessagePart::Text {
                text: "hello from agent".to_string(),
            }],
        });
        assert!(msgs.is_empty());
        let lines = app.tab2_state.lines_for(&task_id);
        assert!(
            !lines.is_empty(),
            "tab2_state should have lines after StreamingUpdate"
        );
    }

    /// Verifies that TaskFileChanged triggers a reload from disk.
    #[test]
    fn test_handle_task_updated_reloads_store() {
        use crate::tasks::models::{Task, TaskStatus};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let tasks_dir = tmp.path().join("tasks");
        std::fs::create_dir(&tasks_dir).unwrap();
        let file_path = tasks_dir.join("1.1.md");

        let initial_md = "Story: 1. Story\nTask: 1.1\nStatus: OPEN\n\n## Description\n\ndesc\n";
        std::fs::write(&file_path, initial_md).unwrap();

        let task = Task {
            id: TaskId::from_path(file_path.clone()),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: "desc".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: file_path.clone(),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();

        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let session_map = Arc::new(RwLock::new(HashMap::new()));
        let mut app = App::new(TaskStore::new(), None, session_map, tx);
        app.task_store.insert(task);

        // Modify the file on disk to change the status.
        let updated_md =
            "Story: 1. Story\nTask: 1.1\nStatus: COMPLETED\n\n## Description\n\ndesc\n";
        std::fs::write(&file_path, updated_md).unwrap();

        let msgs = app.handle_message(AppMessage::TaskFileChanged {
            task_id: task_id.clone(),
        });
        assert!(msgs.is_empty());

        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.status,
            TaskStatus::Completed,
            "task status should reflect the updated file"
        );
    }

    fn make_malformed_task() -> crate::tasks::Task {
        use crate::tasks::models::{ParseErrorInfo, Task, TaskId, TaskStatus};
        Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: Some(ParseErrorInfo {
                error_message: "missing Status".to_string(),
                raw_content: "bad content".to_string(),
                suggested_fix: None,
                fix_in_progress: false,
            }),
        }
    }

    #[test]
    fn test_handle_request_fix_sets_in_progress() {
        use crate::tasks::TaskId;

        let mut app = test_app();
        app.task_store.insert(make_malformed_task());

        let id = TaskId::from_path("tasks/1.1.md");
        let responses = app.handle_message(AppMessage::RequestTaskFix {
            task_id: id.clone(),
        });
        assert!(responses.is_empty());
        let task = app.task_store.get(&id).unwrap();
        let err_info = task.parse_error.as_ref().unwrap();
        assert!(err_info.fix_in_progress);
        assert!(err_info.suggested_fix.is_none());
    }

    #[test]
    fn test_handle_fix_ready_stores_suggestion() {
        use crate::tasks::TaskId;

        let mut app = test_app();
        app.task_store.insert(make_malformed_task());

        let id = TaskId::from_path("tasks/1.1.md");
        // First set fix_in_progress to simulate a pending request.
        if let Some(t) = app.task_store.get_mut(&id) {
            if let Some(ref mut e) = t.parse_error {
                e.fix_in_progress = true;
            }
        }

        let responses = app.handle_message(AppMessage::TaskFixReady {
            task_id: id.clone(),
            corrected_content: "fixed md".to_string(),
            explanation: "Added Status".to_string(),
        });
        assert!(responses.is_empty());
        let task = app.task_store.get(&id).unwrap();
        let err_info = task.parse_error.as_ref().unwrap();
        assert!(!err_info.fix_in_progress);
        let fix = err_info.suggested_fix.as_ref().expect("fix should be set");
        assert_eq!(fix.corrected_content, "fixed md");
        assert_eq!(fix.explanation, "Added Status");
    }

    #[test]
    fn test_handle_apply_fix_success() {
        use crate::tasks::models::{ParseErrorInfo, SuggestedFix, Task, TaskId, TaskStatus};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("1.1.md");
        std::fs::write(&file_path, "bad content").unwrap();

        let corrected = "Story: 1. Story\nTask: 1.1\nStatus: OPEN\n\n## Description\n\ndesc\n";

        let task = Task {
            id: TaskId::from_path(file_path.clone()),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: file_path.clone(),
            extra_sections: Vec::new(),
            parse_error: Some(ParseErrorInfo {
                error_message: "missing Status".to_string(),
                raw_content: "bad content".to_string(),
                suggested_fix: Some(SuggestedFix {
                    corrected_content: corrected.to_string(),
                    explanation: "Added Status line".to_string(),
                }),
                fix_in_progress: false,
            }),
        };
        let id = task.id.clone();
        let mut app = test_app();
        app.task_store.insert(task);

        let responses = app.handle_message(AppMessage::ApplyTaskFix {
            task_id: id.clone(),
        });
        assert!(responses.is_empty());
        // Task should now be valid (no parse_error).
        let updated = app.task_store.get(&id).unwrap();
        assert!(
            !updated.is_malformed(),
            "task should be valid after applying fix"
        );
    }

    #[test]
    fn test_handle_apply_fix_still_broken() {
        use crate::tasks::models::{ParseErrorInfo, SuggestedFix, Task, TaskId, TaskStatus};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("1.1.md");
        std::fs::write(&file_path, "bad content").unwrap();

        // The "fix" is still broken.
        let still_bad = "still not valid markdown for a task";

        let task = Task {
            id: TaskId::from_path(file_path.clone()),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: file_path.clone(),
            extra_sections: Vec::new(),
            parse_error: Some(ParseErrorInfo {
                error_message: "original error".to_string(),
                raw_content: "bad content".to_string(),
                suggested_fix: Some(SuggestedFix {
                    corrected_content: still_bad.to_string(),
                    explanation: "Attempted fix".to_string(),
                }),
                fix_in_progress: false,
            }),
        };
        let id = task.id.clone();
        let mut app = test_app();
        app.task_store.insert(task);

        app.handle_message(AppMessage::ApplyTaskFix {
            task_id: id.clone(),
        });
        // Task should still be malformed since the fix didn't parse cleanly.
        let updated = app.task_store.get(&id).unwrap();
        assert!(
            updated.is_malformed(),
            "task should still be malformed when fix is also broken"
        );
    }
}
