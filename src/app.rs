//! Top-level application state and message dispatcher.
//!
//! The `App` struct holds all runtime state and coordinates between subsystems:
//! the TUI layer, workflow engine, task store, and opencode client.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::messages::AppMessage;
use crate::opencode::events::SessionMap;
use crate::opencode::OpenCodeClient;
use crate::tasks::models::{status_to_index, Question, SuggestedFix, TaskStatus, WorkLogEntry};
use crate::tasks::{Story, TaskId, TaskStore};
use crate::tui::tabs::agent_activity::Tab2State;
use crate::tui::tabs::code_review::Tab4State;
use crate::tui::tabs::questions::QuestionsTabState;
use crate::tui::tabs::task_details::Tab1State;
use crate::tui::tabs::team_status::Tab3State;
use crate::tui::task_list::TaskListState;
use crate::workflow::agents::AgentKind;
use crate::workflow::prompt_composer::compose_user_message;
use crate::workflow::response_parser::{parse_response, AgentResponse};
use crate::workflow::transitions::{WorkflowEngine, WorkflowPhase};

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
    /// When `Some(idx)`, the status picker dialog is visible with `idx` highlighted.
    pub show_status_picker: Option<usize>,
    /// Navigation and expansion state for the left-pane task list widget.
    pub task_list_state: TaskListState,
    /// UI state for Tab 0 (Task Details): prompt input and focus flags.
    pub tab1_state: Tab1State,
    /// UI state for Tab 1 (Questions): question navigation and answer inputs.
    pub questions_state: QuestionsTabState,
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
    /// Timestamp of the last `GET /session/status` poll to throttle requests.
    pub last_status_poll: Instant,
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
            show_status_picker: None,
            task_list_state,
            tab1_state: Tab1State::new(),
            questions_state: QuestionsTabState::new(),
            tab2_state: Tab2State::new(),
            workflow_engine: WorkflowEngine::new(),
            tab3_state: Tab3State::new(),
            tab4_state: Tab4State::new(),
            opencode_client,
            session_map,
            async_tx,
            pending_messages: Vec::new(),
            last_status_poll: Instant::now(),
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

    /// Opens the status picker dialog pre-selecting the current task's status.
    ///
    /// If no task is currently selected, this is a no-op.
    pub fn open_status_picker(&mut self) {
        if let Some(task_id) = self.selected_task().cloned() {
            if let Some(task) = self.task_store.get(&task_id) {
                self.show_status_picker = Some(status_to_index(&task.status));
            }
        }
    }

    /// Dismisses the status picker dialog without changing the task status.
    pub fn dismiss_status_picker(&mut self) {
        self.show_status_picker = None;
    }

    /// Returns the [`TaskId`] of the currently selected task, or `None` if on a story.
    ///
    /// Derived from [`TaskListState::selected_task_id`].
    #[allow(dead_code)]
    pub fn selected_task(&self) -> Option<&TaskId> {
        self.task_list_state.selected_task_id()
    }

    /// Creates a default [`App`] for use in unit tests.
    ///
    /// Wires up a local mpsc channel and an empty session map so callers
    /// do not need to repeat the boilerplate.
    #[cfg(test)]
    pub fn test_default() -> Self {
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::RwLock;
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let session_map = Arc::new(RwLock::new(HashMap::new()));
        App::new(TaskStore::new(), None, session_map, tx)
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
            AppMessage::Tick => {
                let msgs = std::mem::take(&mut self.pending_messages);

                // Periodic liveness poll: every 5s, check sessions awaiting > 3s.
                let awaiting_tasks = self.tab2_state.check_timeouts(Duration::from_secs(3));
                if !awaiting_tasks.is_empty()
                    && self.last_status_poll.elapsed() >= Duration::from_secs(5)
                {
                    self.last_status_poll = Instant::now();

                    // Collect (task_id, session_id) pairs for sessions to verify.
                    let sessions_to_check: Vec<(TaskId, String)> = awaiting_tasks
                        .into_iter()
                        .filter_map(|tid| {
                            self.workflow_engine
                                .state(&tid)
                                .and_then(|s| s.session_id.clone().map(|sid| (tid, sid)))
                        })
                        .collect();

                    if !sessions_to_check.is_empty() {
                        if let Some(client) = self.opencode_client.clone() {
                            let async_tx = self.async_tx.clone();
                            tokio::spawn(async move {
                                let statuses = match client.get_session_statuses().await {
                                    Ok(s) => s,
                                    Err(e) => {
                                        tracing::warn!("Session status poll failed: {}", e);
                                        return;
                                    }
                                };
                                for (task_id, session_id) in sessions_to_check {
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
                                                if entry.info.role
                                                    == crate::opencode::types::MessageRole::Assistant
                                                {
                                                    if let Some(ref err) = entry.info.error {
                                                        return err.message.clone();
                                                    }
                                                    if entry.info.finish.as_deref() == Some("error") {
                                                        return Some(
                                                            "Session finished with error status"
                                                                .to_string(),
                                                        );
                                                    }
                                                }
                                                None
                                            })
                                            .unwrap_or_else(|| {
                                                "Session was idle after prompt -- OpenCode may have crashed silently".to_string()
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
                    }
                }
                msgs
            }

            // --- Streaming/tool activity ---
            AppMessage::StreamingUpdate {
                task_id,
                session_id: _,
                message_id,
                parts,
            } => {
                self.tab2_state.clear_awaiting(&task_id);
                self.tab2_state
                    .push_streaming(&task_id, &message_id, &parts);
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
                        err_info.fix_error = None;
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
                        err_info.fix_error = Some(error);
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

            // --- StartTask: set task InProgress then forward to engine ---
            AppMessage::StartTask { task_id } => {
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    task.status = TaskStatus::InProgress;
                }
                let mut msgs = self.workflow_engine.process(AppMessage::StartTask {
                    task_id: task_id.clone(),
                });
                msgs.push(AppMessage::TaskUpdated {
                    task_id: task_id.clone(),
                });
                msgs
            }

            // --- SessionError: record error in work log then forward to engine ---
            AppMessage::SessionError {
                task_id,
                session_id,
                error,
            } => {
                let current_agent = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|s| s.current_agent)
                    .unwrap_or(AgentKind::Intake);
                self.tab2_state.clear_awaiting(&task_id);
                self.tab2_state.push_banner(
                    &task_id,
                    format!("ERROR ({}): {}", current_agent.display_name(), error),
                );
                tracing::warn!(
                    "Session error for task {} ({}): {}",
                    task_id,
                    current_agent.display_name(),
                    error
                );
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    let seq = task.work_log.len() as u32 + 1;
                    task.work_log.push(WorkLogEntry::Parsed {
                        sequence: seq,
                        timestamp: chrono::Utc::now(),
                        agent: current_agent,
                        description: format!("Session error: {}", error),
                    });
                }
                let mut msgs = self.workflow_engine.process(AppMessage::SessionError {
                    task_id: task_id.clone(),
                    session_id,
                    error,
                });
                msgs.push(AppMessage::TaskUpdated { task_id });
                msgs
            }

            // --- HumanAnswered: show answer in activity tab, record on question, forward to engine ---
            AppMessage::HumanAnswered {
                task_id,
                question_index,
                answer,
            } => {
                self.tab2_state
                    .push_banner(&task_id, format!("[You] {}", answer));
                // Record the answer on the question model.
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    if let Some(q) = task.questions.get_mut(question_index) {
                        q.answer = Some(answer.clone());
                    }
                }
                // Rebuild answer textareas so the answered question's textarea is removed.
                if let Some(task) = self.task_store.get(&task_id) {
                    let task = task.clone();
                    self.questions_state.sync_answer_inputs(&task);
                }
                let mut msgs = self.workflow_engine.process(AppMessage::HumanAnswered {
                    task_id: task_id.clone(),
                    question_index,
                    answer,
                });
                msgs.push(AppMessage::TaskUpdated { task_id });
                msgs
            }

            // --- Workflow messages: forward to engine ---
            AppMessage::AgentCompleted { .. }
            | AppMessage::AgentKickedBack { .. }
            | AppMessage::AgentAskedQuestion { .. }
            | AppMessage::HumanApprovedReview { .. }
            | AppMessage::HumanRequestedRevisions { .. }
            | AppMessage::SessionCreated { .. } => self.workflow_engine.process(msg),

            // --- SessionCompleted: parse agent response and dispatch semantic message ---
            AppMessage::SessionCompleted {
                task_id,
                session_id: _,
                response_text,
            } => {
                self.tab2_state.clear_awaiting(&task_id);
                let current_agent = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|s| s.current_agent);

                match parse_response(&response_text) {
                    Ok(AgentResponse::Complete {
                        summary,
                        updates,
                        commit_message: _,
                    }) => {
                        if let Some(task) = self.task_store.get_mut(&task_id) {
                            if let Some(upd) = updates {
                                if let Some(design) = upd.design {
                                    task.design = Some(design);
                                }
                                if let Some(plan) = upd.implementation_plan {
                                    task.implementation_plan = Some(plan);
                                }
                            }
                            let seq = task.work_log.len() as u32 + 1;
                            task.work_log.push(WorkLogEntry::Parsed {
                                sequence: seq,
                                timestamp: chrono::Utc::now(),
                                agent: current_agent.unwrap_or(AgentKind::Intake),
                                description: summary.clone(),
                            });
                        }
                        let agent = current_agent.unwrap_or(AgentKind::Intake);
                        let truncated = if summary.len() > 80 {
                            format!("{}...", &summary[..80])
                        } else {
                            summary.clone()
                        };
                        self.tab2_state.push_banner(
                            &task_id,
                            format!("{} completed: {}", agent.display_name(), truncated),
                        );
                        let mut msgs = self.workflow_engine.process(AppMessage::AgentCompleted {
                            task_id: task_id.clone(),
                            agent,
                            summary,
                        });
                        msgs.push(AppMessage::TaskUpdated {
                            task_id: task_id.clone(),
                        });
                        msgs
                    }
                    Ok(AgentResponse::Question { question, .. }) => {
                        let agent = current_agent.unwrap_or(AgentKind::Intake);
                        if let Some(task) = self.task_store.get_mut(&task_id) {
                            task.questions.push(Question {
                                agent,
                                text: question.clone(),
                                answer: None,
                            });
                        }
                        self.tab2_state.push_banner(
                            &task_id,
                            format!("{} has a question (see Task Details)", agent.display_name()),
                        );
                        let mut msgs =
                            self.workflow_engine
                                .process(AppMessage::AgentAskedQuestion {
                                    task_id: task_id.clone(),
                                    agent,
                                    question,
                                });
                        msgs.push(AppMessage::TaskUpdated {
                            task_id: task_id.clone(),
                        });
                        msgs
                    }
                    Ok(AgentResponse::Kickback {
                        target_agent,
                        reason,
                    }) => {
                        let from = current_agent.unwrap_or(AgentKind::Intake);
                        let to = AgentKind::from_display_name(&target_agent).unwrap_or(from);
                        let truncated_reason = if reason.len() > 80 {
                            format!("{}...", &reason[..80])
                        } else {
                            reason.clone()
                        };
                        self.tab2_state.push_banner(
                            &task_id,
                            format!(
                                "{} kicked back to {}: {}",
                                from.display_name(),
                                to.display_name(),
                                truncated_reason
                            ),
                        );
                        self.workflow_engine.process(AppMessage::AgentKickedBack {
                            task_id,
                            from,
                            to,
                            reason,
                        })
                    }
                    Err(_) => {
                        // Fallback: advance pipeline with a placeholder summary.
                        let agent = current_agent.unwrap_or(AgentKind::Intake);
                        tracing::warn!(
                            "Could not parse structured output for task {}; advancing with fallback",
                            task_id
                        );
                        self.tab2_state.push_banner(
                            &task_id,
                            "Agent output could not be parsed; advancing".to_string(),
                        );
                        self.workflow_engine.process(AppMessage::AgentCompleted {
                            task_id,
                            agent,
                            summary: "(no structured output)".to_string(),
                        })
                    }
                }
            }

            // --- Async session operations ---
            AppMessage::CreateSession {
                task_id,
                agent,
                context,
                prompt: _,
            } => {
                // Push lifecycle banners before spawning so Tab 2 shows activity immediately.
                let agent_name = agent.display_name().to_string();
                self.tab2_state
                    .push_banner(&task_id, format!("--- {} ---", agent_name));
                self.tab2_state
                    .push_banner(&task_id, "Creating session...".to_string());

                // Build the real prompt from task context; ignore the placeholder prompt field.
                let prompt = self
                    .task_store
                    .get(&task_id)
                    .map(|task| compose_user_message(&agent, task, context.as_deref()))
                    .unwrap_or_else(|| {
                        format!("Begin task {} as {} agent.", task_id, agent.display_name())
                    });

                // Show prompt content in the activity tab (truncated to 500 chars).
                let display_len = prompt.len().min(500);
                let prompt_preview = if prompt.len() > 500 {
                    format!("{}...", &prompt[..display_len])
                } else {
                    prompt.clone()
                };
                self.tab2_state
                    .push_banner(&task_id, format!("[Prompt] {}", prompt_preview));

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
                        .send_prompt_async(&session.id, Some(&agent), &prompt)
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
                    if let Err(e) = client
                        .send_prompt_async(&session_id, Some(&agent), &prompt)
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
                vec![]
            }

            AppMessage::PromptSent { task_id, .. } => {
                let agent_name = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|s| s.current_agent.display_name().to_string())
                    .unwrap_or_else(|| "Agent".to_string());
                self.tab2_state
                    .push_banner(&task_id, "Prompt sent.".to_string());
                self.tab2_state.set_awaiting_response(&task_id, agent_name);
                vec![]
            }

            AppMessage::VerifySessionIdle {
                task_id,
                session_id,
                error,
            } => {
                // Guard: only act if the workflow engine still has this exact session
                // in the Running phase. If the session already completed or moved on,
                // this is a stale verification and should be ignored.
                let is_active = self.workflow_engine.state(&task_id).is_some_and(|s| {
                    s.session_id.as_deref() == Some(&session_id)
                        && s.phase == WorkflowPhase::Running
                });
                if is_active {
                    self.tab2_state.clear_awaiting(&task_id);
                    vec![AppMessage::SessionError {
                        task_id,
                        session_id,
                        error,
                    }]
                } else {
                    vec![]
                }
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
                self.tab4_state.set_displayed_task(Some(&task_id));
                self.tab4_state.reset_for_diffs();
                self.active_tab = 4;
                vec![]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_app_new() {
        let app = App::test_default();
        assert!(app.selected_task().is_none());
        assert_eq!(app.active_tab, 0);
        assert!(!app.should_quit);
        assert!(!app.show_quit_confirm);
        assert!(app.show_status_picker.is_none());
        assert_eq!(app.task_list_state.selected_index, 0);
        assert!(app.task_list_state.expanded_stories.is_empty());
    }

    #[test]
    fn test_dismiss_quit_confirm() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        app.dismiss_quit_confirm();
        assert!(!app.show_quit_confirm);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_open_status_picker_preselects_current() {
        use crate::tasks::models::{Task, TaskId, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("1. Story".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // Navigate to the task row (index 1).
        app.task_list_state.selected_index = 1;

        app.open_status_picker();
        // InProgress is index 1 in ALL_STATUSES.
        assert_eq!(app.show_status_picker, Some(1));
    }

    #[test]
    fn test_dismiss_status_picker() {
        let mut app = App::test_default();
        app.show_status_picker = Some(2);
        app.dismiss_status_picker();
        assert!(app.show_status_picker.is_none());
    }

    #[test]
    fn test_handle_message_tick_drains_pending() {
        let mut app = App::test_default();
        // Inject a pending message to verify Tick drains the buffer.
        app.pending_messages.push(AppMessage::TaskUpdated {
            task_id: TaskId::from_path("tasks/1.1.md"),
        });
        let responses = app.handle_message(AppMessage::Tick);
        // pending_messages is returned as follow-up work.
        assert_eq!(responses.len(), 1);
        assert!(app.pending_messages.is_empty());
    }

    /// Verifies that Tick does not emit SessionError for a freshly started awaiting session.
    #[test]
    fn test_handle_tick_does_not_timeout_fresh_session() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);
        // Simulate a prompt having just been sent.
        app.tab2_state
            .set_awaiting_response(&task_id, "Intake Agent".to_string());
        // Tick with a fresh session should not produce a SessionError.
        let msgs = app.handle_message(AppMessage::Tick);
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, AppMessage::SessionError { .. })),
            "fresh awaiting session should not trigger a timeout on Tick"
        );
    }

    /// Verifies two-phase quit: first Shutdown shows dialog, second sets should_quit.
    #[test]
    fn test_handle_shutdown() {
        let mut app = App::test_default();
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

        let mut app = App::test_default();
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
        let mut app = App::test_default();
        let task_id = TaskId::from_path("tasks/1.1.md");
        let msgs = app.handle_message(AppMessage::StartTask {
            task_id: task_id.clone(),
        });
        // Expect CreateSession + TaskUpdated.
        assert_eq!(msgs.len(), 2);
        assert!(
            matches!(
                &msgs[0],
                AppMessage::CreateSession { agent, context: None, .. }
                    if *agent == crate::workflow::agents::AgentKind::Intake
            ),
            "expected CreateSession for Intake with context: None, got: {:?}",
            msgs[0]
        );
        assert!(
            matches!(&msgs[1], AppMessage::TaskUpdated { .. }),
            "expected TaskUpdated as second message"
        );
    }

    /// Verifies that StartTask sets task status to InProgress.
    #[test]
    fn test_handle_start_task_sets_in_progress() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
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
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();
        app.task_store.insert(task);

        app.handle_message(AppMessage::StartTask {
            task_id: task_id.clone(),
        });

        let task = app.task_store.get(&task_id).expect("task should exist");
        assert_eq!(
            task.status,
            TaskStatus::InProgress,
            "StartTask should set task status to InProgress"
        );
    }

    /// Verifies that StreamingUpdate with text parts updates tab2_state.
    #[test]
    fn test_handle_streaming_update_updates_tab2() {
        use crate::opencode::types::MessagePart;

        let mut app = App::test_default();
        let task_id = TaskId::from_path("tasks/1.1.md");
        let msgs = app.handle_message(AppMessage::StreamingUpdate {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            message_id: "msg-1".to_string(),
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
    fn test_handle_task_file_changed_reloads_store() {
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

        let mut app = App::test_default();
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
                fix_error: None,
            }),
        }
    }

    #[test]
    fn test_handle_request_fix_sets_in_progress() {
        use crate::tasks::TaskId;

        let mut app = App::test_default();
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

        let mut app = App::test_default();
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
                fix_error: None,
            }),
        };
        let id = task.id.clone();
        let mut app = App::test_default();
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
                fix_error: None,
            }),
        };
        let id = task.id.clone();
        let mut app = App::test_default();
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

    /// Helper: builds a minimal valid task and starts it through the workflow engine.
    fn make_task_in_progress(app: &mut App) -> TaskId {
        use crate::tasks::models::{Task, TaskStatus};

        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: "implement feature".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();
        app.task_store.insert(task);
        // Start task to initialize workflow state (current_agent = Intake).
        app.workflow_engine.process(AppMessage::StartTask {
            task_id: task_id.clone(),
        });
        task_id
    }

    #[test]
    fn test_handle_session_completed_complete_response() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        let response_json =
            r#"{"action":"complete","summary":"Intake done","updates":{"design":"New design"}}"#;
        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: response_json.to_string(),
        });

        // Should emit AgentCompleted -> CreateSession for Design + TaskUpdated.
        assert!(
            msgs.iter().any(|m| matches!(m, AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Design)),
            "expected CreateSession for Design, got: {msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "expected TaskUpdated"
        );

        let task = app.task_store.get(&task_id).expect("task should exist");
        assert_eq!(
            task.design.as_deref(),
            Some("New design"),
            "design should be updated"
        );
        assert_eq!(task.work_log.len(), 1, "work log should have one entry");
    }

    #[test]
    fn test_handle_session_completed_question_response() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        let response_json =
            r#"{"action":"question","question":"What is scope?","context":"Need clarity"}"#;
        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: response_json.to_string(),
        });

        // AgentAskedQuestion pauses workflow -- no CreateSession should be emitted.
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, AppMessage::CreateSession { .. })),
            "no CreateSession expected for question"
        );
        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "expected TaskUpdated"
        );

        let task = app.task_store.get(&task_id).expect("task should exist");
        assert_eq!(task.questions.len(), 1, "question should be recorded");
        assert_eq!(task.questions[0].text, "What is scope?");
    }

    #[test]
    fn test_handle_session_completed_kickback_response() {
        let mut app = App::test_default();

        // Build a task already at CodeQuality (advance engine past Intake through Design, Planning, Impl).
        let task_id = make_task_in_progress(&mut app);
        // Advance through Intake -> Design -> Planning -> Implementation -> CodeQuality.
        for _ in 0..4 {
            app.workflow_engine.process(AppMessage::AgentCompleted {
                task_id: task_id.clone(),
                agent: app.workflow_engine.state(&task_id).unwrap().current_agent,
                summary: "done".to_string(),
            });
        }

        let response_json =
            r#"{"action":"kickback","target_agent":"Implementation Agent","reason":"Needs tests"}"#;
        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: response_json.to_string(),
        });

        // AgentKickedBack -> CreateSession for Implementation.
        assert!(
            msgs.iter().any(|m| matches!(m, AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Implementation)),
            "expected CreateSession for Implementation after kickback, got: {msgs:?}"
        );
        let _ = app.task_store.get(&task_id).expect("task should exist");
    }

    #[test]
    fn test_handle_session_completed_unparseable_fallback() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Unparseable text should fallback to AgentCompleted.
        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: "I could not produce structured output.".to_string(),
        });

        // Fallback AgentCompleted -> pipeline advances -> CreateSession for Design.
        assert!(
            msgs.iter().any(|m| matches!(m, AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Design)),
            "expected CreateSession for Design after fallback, got: {msgs:?}"
        );
    }

    /// Verifies that SessionError adds a work log entry and emits TaskUpdated.
    #[test]
    fn test_handle_session_error_adds_work_log_entry() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: "desc".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();
        app.task_store.insert(task);
        // Initialize workflow state so current_agent is known.
        app.workflow_engine.process(AppMessage::StartTask {
            task_id: task_id.clone(),
        });

        let msgs = app.handle_message(AppMessage::SessionError {
            task_id: task_id.clone(),
            session_id: "sess-err".to_string(),
            error: "rate limit exceeded".to_string(),
        });

        // TaskUpdated must be emitted.
        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "expected TaskUpdated after SessionError"
        );

        // Work log should contain the error entry.
        let task = app.task_store.get(&task_id).expect("task should exist");
        assert_eq!(task.work_log.len(), 1, "work log should have one entry");
        let description = match &task.work_log[0] {
            WorkLogEntry::Parsed { description, .. } => description.as_str(),
            WorkLogEntry::Raw { text, .. } => text.as_str(),
        };
        assert!(
            description.contains("rate limit exceeded"),
            "work log entry should include the error message"
        );
    }

    /// Verifies that CreateSession pushes banner lines including the prompt text into Tab2.
    #[test]
    fn test_handle_create_session_pushes_banners() {
        use crate::tui::tabs::agent_activity::ActivityLine;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Dispatch CreateSession (no opencode client -- will produce SessionError,
        // but the banners should be pushed synchronously before spawning).
        app.handle_message(AppMessage::CreateSession {
            task_id: task_id.clone(),
            agent: AgentKind::Intake,
            context: None,
            prompt: String::new(),
        });

        let lines = app.tab2_state.lines_for(&task_id);
        assert!(
            lines.len() >= 2,
            "expected at least 2 banner lines, got {}",
            lines.len()
        );
        assert!(
            matches!(&lines[0], ActivityLine::AgentBanner { message } if message.contains("Intake")),
            "first line should be the agent name banner: {:?}",
            lines[0]
        );
        assert!(
            matches!(&lines[1], ActivityLine::AgentBanner { message } if message.contains("Creating session")),
            "second line should be 'Creating session...': {:?}",
            lines[1]
        );
    }

    /// Verifies that CreateSession pushes a [Prompt] banner with the composed prompt text.
    #[test]
    fn test_handle_create_session_pushes_prompt_banner() {
        use crate::tui::tabs::agent_activity::ActivityLine;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        app.handle_message(AppMessage::CreateSession {
            task_id: task_id.clone(),
            agent: AgentKind::Intake,
            context: None,
            prompt: String::new(),
        });

        let lines = app.tab2_state.lines_for(&task_id);
        let prompt_banner = lines.iter().find(|l| {
            matches!(l, ActivityLine::AgentBanner { message } if message.starts_with("[Prompt]"))
        });
        assert!(
            prompt_banner.is_some(),
            "expected a [Prompt] banner in Tab2 lines: {:?}",
            lines
        );
    }

    /// Verifies that HumanAnswered pushes a [You] banner before forwarding to the engine.
    #[test]
    fn test_handle_human_answered_pushes_banner() {
        use crate::tasks::models::{Question, Task, TaskStatus};
        use crate::tui::tabs::agent_activity::ActivityLine;

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: "desc".to_string(),
            starting_prompt: None,
            questions: vec![Question {
                agent: AgentKind::Intake,
                text: "What is the scope?".to_string(),
                answer: None,
            }],
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();
        app.task_store.insert(task);
        app.workflow_engine.process(AppMessage::StartTask {
            task_id: task_id.clone(),
        });

        app.handle_message(AppMessage::HumanAnswered {
            task_id: task_id.clone(),
            question_index: 0,
            answer: "The scope is minimal.".to_string(),
        });

        let lines = app.tab2_state.lines_for(&task_id);
        assert!(
            lines.iter().any(|l| matches!(
                l,
                ActivityLine::AgentBanner { message }
                if message == "[You] The scope is minimal."
            )),
            "expected a [You] banner in Tab2 lines: {:?}",
            lines
        );
    }

    /// Verifies that HumanAnswered calls sync_answer_inputs, reducing answer_inputs by 1.
    #[test]
    fn test_human_answered_syncs_answer_inputs() {
        use crate::tasks::models::{Question, Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: "desc".to_string(),
            starting_prompt: None,
            questions: vec![
                Question {
                    agent: AgentKind::Intake,
                    text: "Q1?".to_string(),
                    answer: None,
                },
                Question {
                    agent: AgentKind::Intake,
                    text: "Q2?".to_string(),
                    answer: None,
                },
            ],
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();
        app.task_store.insert(task);
        // Initialize answer_inputs for the two unanswered questions.
        if let Some(t) = app.task_store.get(&task_id) {
            let t = t.clone();
            app.questions_state.sync_answer_inputs(&t);
        }
        assert_eq!(app.questions_state.answer_inputs.len(), 2);

        app.workflow_engine.process(AppMessage::StartTask {
            task_id: task_id.clone(),
        });
        app.handle_message(AppMessage::HumanAnswered {
            task_id: task_id.clone(),
            question_index: 0,
            answer: "answer to Q1".to_string(),
        });

        assert_eq!(
            app.questions_state.answer_inputs.len(),
            1,
            "answer_inputs should decrease by 1 after answering one question"
        );
    }

    /// Verifies that HumanAnswered records the answer on the question model.
    #[test]
    fn test_human_answered_records_answer_on_question() {
        use crate::tasks::models::{Question, Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: "desc".to_string(),
            starting_prompt: None,
            questions: vec![Question {
                agent: AgentKind::Intake,
                text: "What is the scope?".to_string(),
                answer: None,
            }],
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();
        app.task_store.insert(task);
        app.workflow_engine.process(AppMessage::StartTask {
            task_id: task_id.clone(),
        });

        app.handle_message(AppMessage::HumanAnswered {
            task_id: task_id.clone(),
            question_index: 0,
            answer: "Minimal scope.".to_string(),
        });

        let task = app.task_store.get(&task_id).expect("task should exist");
        assert_eq!(
            task.questions[0].answer,
            Some("Minimal scope.".to_string()),
            "question.answer should be recorded after HumanAnswered"
        );
    }

    /// Verifies that DiffReady stores diffs, switches to Tab 4, and resets navigation.
    #[test]
    fn test_handle_diff_ready_switches_to_review_tab() {
        use crate::opencode::types::{DiffStatus, FileDiff};

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);
        assert_eq!(app.active_tab, 0, "starts on Tab 0");

        let diffs = vec![FileDiff {
            path: "src/foo.rs".to_string(),
            status: DiffStatus::Modified,
            hunks: vec![],
        }];
        let msgs = app.handle_message(AppMessage::DiffReady {
            task_id: task_id.clone(),
            diffs,
        });

        assert!(msgs.is_empty());
        assert_eq!(app.active_tab, 4, "should switch to Tab 4 (Review)");
        assert_eq!(
            app.tab4_state.diffs_for(&task_id).len(),
            1,
            "diffs should be stored"
        );
        assert_eq!(
            app.tab4_state.selected_file, 0,
            "file selection should reset to 0"
        );
    }

    /// Verifies that PromptSent pushes a banner and sets awaiting state.
    #[test]
    fn test_handle_prompt_sent_sets_awaiting() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        let msgs = app.handle_message(AppMessage::PromptSent {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
        });
        assert!(msgs.is_empty());

        // The status line should now be active.
        let status = app.tab2_state.elapsed_status(&task_id);
        assert!(
            status.is_some(),
            "elapsed_status should be set after PromptSent"
        );
    }

    /// Verifies that StreamingUpdate clears the awaiting state.
    #[test]
    fn test_handle_streaming_update_clears_awaiting() {
        use crate::opencode::types::MessagePart;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Manually set awaiting state.
        app.tab2_state
            .set_awaiting_response(&task_id, "Intake Agent".to_string());
        assert!(app.tab2_state.elapsed_status(&task_id).is_some());

        app.handle_message(AppMessage::StreamingUpdate {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            message_id: "msg-1".to_string(),
            parts: vec![MessagePart::Text {
                text: "hello".to_string(),
            }],
        });

        assert!(
            app.tab2_state.elapsed_status(&task_id).is_none(),
            "StreamingUpdate should clear the awaiting state"
        );
    }

    /// Verifies that SessionError pushes an error banner.
    #[test]
    fn test_handle_session_error_pushes_banner() {
        use crate::tui::tabs::agent_activity::ActivityLine;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        app.handle_message(AppMessage::SessionError {
            task_id: task_id.clone(),
            session_id: "sess-err".to_string(),
            error: "timeout".to_string(),
        });

        let lines = app.tab2_state.lines_for(&task_id);
        assert!(
            lines
                .iter()
                .any(|l| matches!(l, ActivityLine::AgentBanner { message }
                if message.contains("ERROR") && message.contains("timeout"))),
            "expected an error banner in Tab2 lines: {:?}",
            lines
        );
    }

    /// Helper: builds a task in progress and registers its session ID in the workflow engine.
    fn make_task_with_active_session(app: &mut App, session_id: &str) -> TaskId {
        let task_id = make_task_in_progress(app);
        // Register the session in the workflow engine so phase == Running and session_id is set.
        app.workflow_engine.process(AppMessage::SessionCreated {
            task_id: task_id.clone(),
            session_id: session_id.to_string(),
        });
        // Set awaiting state on tab2.
        app.tab2_state
            .set_awaiting_response(&task_id, "Intake Agent".to_string());
        task_id
    }

    /// VerifySessionIdle with a matching active session escalates to SessionError.
    #[test]
    fn test_handle_verify_session_idle_active_session() {
        let mut app = App::test_default();
        let task_id = make_task_with_active_session(&mut app, "sess-abc");

        let msgs = app.handle_message(AppMessage::VerifySessionIdle {
            task_id: task_id.clone(),
            session_id: "sess-abc".to_string(),
            error: "agent.model on undefined".to_string(),
        });

        assert_eq!(msgs.len(), 1, "should produce exactly one SessionError");
        assert!(
            matches!(
                &msgs[0],
                AppMessage::SessionError { session_id, error, .. }
                    if session_id == "sess-abc" && error.contains("agent.model")
            ),
            "expected SessionError with correct fields, got: {:?}",
            msgs[0]
        );
        // Awaiting state should be cleared.
        assert!(
            app.tab2_state.elapsed_status(&task_id).is_none(),
            "awaiting state should be cleared after VerifySessionIdle"
        );
    }

    /// VerifySessionIdle is ignored when the session has already completed.
    #[test]
    fn test_handle_verify_session_idle_session_already_completed() {
        let mut app = App::test_default();
        let task_id = make_task_with_active_session(&mut app, "sess-abc");

        // Advance workflow to PendingReview (session completed normally).
        app.workflow_engine.process(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-abc".to_string(),
            response_text: String::new(),
        });

        let msgs = app.handle_message(AppMessage::VerifySessionIdle {
            task_id: task_id.clone(),
            session_id: "sess-abc".to_string(),
            error: "stale idle detection".to_string(),
        });

        assert!(
            msgs.is_empty(),
            "stale VerifySessionIdle should be ignored, got: {:?}",
            msgs
        );
    }

    /// VerifySessionIdle is ignored when the session_id no longer matches.
    #[test]
    fn test_handle_verify_session_idle_mismatched_session_id() {
        let mut app = App::test_default();
        // Task uses "sess-new", but VerifySessionIdle refers to "sess-old".
        make_task_with_active_session(&mut app, "sess-new");
        let task_id = TaskId::from_path("tasks/1.1.md");

        let msgs = app.handle_message(AppMessage::VerifySessionIdle {
            task_id: task_id.clone(),
            session_id: "sess-old".to_string(),
            error: "stale session".to_string(),
        });

        assert!(
            msgs.is_empty(),
            "mismatched session_id should be ignored, got: {:?}",
            msgs
        );
    }

    /// TaskFixFailed stores the error message in parse_error.fix_error.
    #[test]
    fn test_handle_task_fix_failed_stores_error() {
        use crate::tasks::TaskId;

        let mut app = App::test_default();
        app.task_store.insert(make_malformed_task());

        let id = TaskId::from_path("tasks/1.1.md");
        let responses = app.handle_message(AppMessage::TaskFixFailed {
            task_id: id.clone(),
            error: "OpenCode server unavailable".to_string(),
        });
        assert!(responses.is_empty());
        let task = app.task_store.get(&id).unwrap();
        let err_info = task.parse_error.as_ref().unwrap();
        assert!(!err_info.fix_in_progress);
        assert_eq!(
            err_info.fix_error.as_deref(),
            Some("OpenCode server unavailable")
        );
    }

    /// RequestTaskFix clears any previous fix_error.
    #[test]
    fn test_request_task_fix_clears_previous_error() {
        use crate::tasks::TaskId;

        let mut app = App::test_default();
        let mut task = make_malformed_task();
        // Pre-seed a fix error from a previous failed attempt.
        if let Some(ref mut e) = task.parse_error {
            e.fix_error = Some("previous error".to_string());
        }
        app.task_store.insert(task);

        let id = TaskId::from_path("tasks/1.1.md");
        let responses = app.handle_message(AppMessage::RequestTaskFix {
            task_id: id.clone(),
        });
        assert!(responses.is_empty());
        let task = app.task_store.get(&id).unwrap();
        let err_info = task.parse_error.as_ref().unwrap();
        assert!(err_info.fix_in_progress);
        assert!(err_info.fix_error.is_none(), "fix_error should be cleared");
    }

    /// VerifySessionIdle is ignored for a task with no workflow state.
    #[test]
    fn test_handle_verify_session_idle_unknown_task() {
        let mut app = App::test_default();
        let unknown_id = TaskId::from_path("tasks/99.99.md");

        let msgs = app.handle_message(AppMessage::VerifySessionIdle {
            task_id: unknown_id,
            session_id: "sess-xyz".to_string(),
            error: "ghost session".to_string(),
        });

        assert!(
            msgs.is_empty(),
            "VerifySessionIdle for unknown task should be ignored"
        );
    }

    /// Tick does not emit SessionError for a freshly awaiting session (< 3s).
    #[test]
    fn test_tick_does_not_poll_before_3s() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);
        // Simulate a prompt just sent.
        app.tab2_state
            .set_awaiting_response(&task_id, "Intake Agent".to_string());
        // Tick immediately: check_timeouts(3s) returns empty, so no poll fires.
        let msgs = app.handle_message(AppMessage::Tick);
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, AppMessage::SessionError { .. })),
            "Tick should not emit SessionError for a fresh awaiting session"
        );
    }

    /// Tick does not poll when last_status_poll was less than 5s ago.
    #[test]
    fn test_tick_does_not_poll_within_5s_cooldown() {
        let mut app = App::test_default();
        // last_status_poll defaults to Instant::now() in App::new(), so it's very recent.
        let task_id = make_task_in_progress(&mut app);
        app.tab2_state
            .set_awaiting_response(&task_id, "Intake Agent".to_string());
        // Tick: even if there were timed-out sessions, last_status_poll.elapsed() < 5s.
        let msgs = app.handle_message(AppMessage::Tick);
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, AppMessage::SessionError { .. })),
            "Tick should not poll when last_status_poll is recent"
        );
    }
}
