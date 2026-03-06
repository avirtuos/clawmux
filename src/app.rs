//! Top-level application state and message dispatcher.
//!
//! The `App` struct holds all runtime state and coordinates between subsystems:
//! the TUI layer, workflow engine, task store, and agent backend.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::backend::AgentBackend;
use crate::messages::AppMessage;
use crate::notifications::Notifier;
use crate::opencode::events::SessionMap;
use crate::opencode::types::{DiffStatus, ModelId};
use crate::tasks::models::{status_to_index, Question, SuggestedFix, TaskStatus, WorkLogEntry};
use crate::tasks::{Story, TaskId, TaskStore};
use crate::tui::tabs::agent_activity::Tab2State;
use crate::tui::tabs::code_review::Tab4State;
use crate::tui::tabs::design::DesignTabState;
use crate::tui::tabs::plan::PlanTabState;
use crate::tui::tabs::questions::QuestionsTabState;
use crate::tui::tabs::research::ResearchTabState;
use crate::tui::tabs::review::ReviewTabState;
use crate::tui::tabs::task_details::Tab1State;
use crate::tui::tabs::team_status::Tab3State;
use crate::tui::task_list::TaskListState;
use crate::workflow::agents::AgentKind;
use crate::workflow::prompt_composer::compose_user_message;
use crate::workflow::response_parser::{parse_response, AgentResponse};
use crate::workflow::transitions::{WorkflowEngine, WorkflowPhase};

/// State for the commit confirmation dialog.
///
/// Shown when the human presses `[a]` on a task in `PendingReview`.  The
/// `editor` textarea is pre-filled with the CodeReview agent's proposed commit
/// message (or a default) and can be edited before confirmation.
pub struct CommitDialogState {
    /// The task being committed.
    pub task_id: TaskId,
    /// Editable commit message textarea.
    pub editor: tui_textarea::TextArea<'static>,
    /// Changed files from the latest diff snapshot: `(path, status)`.
    pub file_summary: Vec<(String, DiffStatus)>,
}

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
    /// UI state for Tab 2 (Design): design document scroll.
    pub design_state: DesignTabState,
    /// UI state for Tab 3 (Plan): implementation plan scroll.
    pub plan_state: PlanTabState,
    /// UI state for Tab 4 (Agent Activity): per-task activity lines and scroll.
    pub tab2_state: Tab2State,
    /// Pure state machine driving tasks through the 7-agent pipeline.
    pub workflow_engine: WorkflowEngine,
    /// UI state for Tab 3 (Team Status): work log scroll and current task.
    pub tab3_state: Tab3State,
    /// UI state for Tab 4 (Code Review): per-task diff storage.
    pub tab4_state: Tab4State,
    /// UI state for Tab 6 (Review Discussion): per-task review timeline.
    pub review_state: ReviewTabState,
    /// UI state for Tab 8 (Research): global AI chat scratchpad.
    pub research_state: ResearchTabState,
    /// Agent backend for all session lifecycle operations.
    pub backend: Box<dyn AgentBackend>,
    /// Shared map from session ID to (TaskId, AgentKind), used by EventStreamConsumer.
    pub session_map: SessionMap,
    /// Sender used by tokio::spawn callbacks to post results back to the main loop.
    pub async_tx: mpsc::Sender<AppMessage>,
    /// Buffer of messages produced by spawned tasks, drained on each Tick.
    pub pending_messages: Vec<AppMessage>,
    /// Timestamp of the last `GET /session/status` poll to throttle requests.
    pub last_status_poll: Instant,
    /// When `Some`, the commit confirmation dialog is open.
    pub commit_dialog: Option<CommitDialogState>,
    /// Maps session_id to task_id for in-flight commit sessions.
    ///
    /// When a `SessionCompleted` arrives for a session_id found here, it is
    /// routed to `CommitCompleted` instead of the normal agent pipeline.
    pub pending_commit_sessions: HashMap<String, TaskId>,
    /// Model IDs parsed from each agent's embedded `.md` frontmatter at startup.
    ///
    /// Used to pass an explicit model in every `send_prompt_async` call, taking
    /// highest priority in OpenCode's resolution order.
    pub agent_models: HashMap<AgentKind, ModelId>,
    /// Default model from the global provider config, used for commit/fix sessions.
    pub default_model: Option<ModelId>,
    /// Sends terminal bell and desktop notifications when human attention is needed.
    notifier: Notifier,
}

/// Parses `git status --porcelain` output into `(path, DiffStatus)` pairs.
///
/// Handles the common porcelain v1 status codes:
/// - `??` / `A ` → [`DiffStatus::Added`]
/// - `D ` / ` D` → [`DiffStatus::Deleted`]
/// - `R ` (rename) → [`DiffStatus::Modified`] using the new path
/// - All other non-empty codes → [`DiffStatus::Modified`]
fn parse_git_status_porcelain(output: &str) -> Vec<(String, DiffStatus)> {
    output
        .lines()
        .filter_map(|line| {
            if line.len() < 3 {
                return None;
            }
            let xy = &line[..2];
            let path_part = &line[3..];
            // Renames are formatted as "R  old -> new"; use the new path.
            let path = if xy.starts_with('R') {
                path_part.split(" -> ").last().unwrap_or(path_part)
            } else {
                path_part
            };
            let status = match xy {
                "??" | "A " | "AD" => DiffStatus::Added,
                "D " | " D" | "DD" => DiffStatus::Deleted,
                _ => DiffStatus::Modified,
            };
            Some((path.to_string(), status))
        })
        .collect()
}

/// Runs `git status --porcelain` and returns all working-tree changes.
///
/// Returns an empty list if the command fails or the output cannot be parsed.
/// This is intentionally synchronous: `git status` is fast and is only called
/// when the user opens the commit dialog.
fn git_status_files() -> Vec<(String, DiffStatus)> {
    match std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .output()
    {
        Ok(out) => parse_git_status_porcelain(&String::from_utf8_lossy(&out.stdout)),
        Err(e) => {
            tracing::warn!("Failed to run git status --porcelain: {}", e);
            Vec::new()
        }
    }
}

impl App {
    /// Creates a new `App` with the given task store and default UI state.
    ///
    /// # Arguments
    ///
    /// * `task_store` - The in-memory task store loaded from disk.
    /// * `backend` - Agent backend for all session lifecycle operations.
    /// * `session_map` - Shared map correlating session IDs to tasks and agents.
    /// * `async_tx` - Channel sender for routing async task results back to the event loop.
    /// * `approval_gate` - When `true`, pause and require human approval between agents.
    /// * `notifications` - When `true`, ring the bell and send desktop notifications.
    /// * `agent_models` - Per-agent model IDs parsed from embedded agent frontmatter.
    /// * `default_model` - Fallback model for sessions without a dedicated agent.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task_store: TaskStore,
        backend: Box<dyn AgentBackend>,
        session_map: SessionMap,
        async_tx: mpsc::Sender<AppMessage>,
        approval_gate: bool,
        notifications: bool,
        agent_models: HashMap<AgentKind, ModelId>,
        default_model: Option<ModelId>,
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
            design_state: DesignTabState::new(),
            plan_state: PlanTabState::new(),
            tab2_state: Tab2State::new(),
            workflow_engine: WorkflowEngine::new(approval_gate),
            tab3_state: Tab3State::new(),
            tab4_state: Tab4State::new(),
            review_state: ReviewTabState::new(),
            research_state: ResearchTabState::new(),
            backend,
            session_map,
            async_tx,
            pending_messages: Vec::new(),
            last_status_poll: Instant::now(),
            commit_dialog: None,
            pending_commit_sessions: HashMap::new(),
            agent_models,
            default_model,
            notifier: Notifier::new(notifications),
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

    /// Opens the commit confirmation dialog for the given task.
    ///
    /// Pre-fills the editor textarea with the commit message stored by the
    /// CodeReview agent, falling back to `"Complete task <name>"` if none is
    /// available. Populates `file_summary` from the current diff snapshot.
    pub fn open_commit_dialog(&mut self, task_id: &TaskId) {
        let commit_msg = self
            .tab4_state
            .get_commit_message(task_id)
            .map(str::to_owned)
            .or_else(|| {
                self.task_store
                    .get(task_id)
                    .map(|t| format!("Complete task {}", t.name))
            })
            .unwrap_or_else(|| format!("Complete task {}", task_id));

        let mut file_summary = git_status_files();

        // Include the task's own markdown file so the user can see it will be committed.
        if let Some(task) = self.task_store.get(task_id) {
            let task_path = task.file_path.to_string_lossy().to_string();
            if !file_summary.iter().any(|(p, _)| p == &task_path) {
                file_summary.push((task_path, DiffStatus::Modified));
            }
        }

        let mut editor = tui_textarea::TextArea::default();
        editor.insert_str(&commit_msg);
        self.commit_dialog = Some(CommitDialogState {
            task_id: task_id.clone(),
            editor,
            file_summary,
        });
    }

    /// Returns the synthetic [`TaskId`] sentinel used for the Research tab session.
    ///
    /// All real task IDs start with `"tasks/"`, so this synthetic value cannot collide.
    pub fn research_task_id() -> TaskId {
        TaskId::from_path("__research__")
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
    /// Wires up a local mpsc channel, an empty session map, and a [`NullBackend`]
    /// so callers do not need to repeat the boilerplate.
    #[cfg(test)]
    pub fn test_default() -> Self {
        use std::sync::Arc;
        use tokio::sync::RwLock;
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let session_map = Arc::new(RwLock::new(HashMap::new()));
        App::new(
            TaskStore::new(),
            Box::new(crate::backend::NullBackend),
            session_map,
            tx,
            false,
            false,
            HashMap::new(),
            None,
        )
    }

    /// Reads the workflow engine's post-transition state for a task and syncs
    /// `task.assigned_to` and `task.status` to match.
    ///
    /// Called after the engine processes an `AgentCompleted` event so the task
    /// model stays in sync with the pipeline phase.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The task to update.
    /// * `actor` - The agent whose action triggered this sync (recorded in the work log).
    fn sync_task_with_workflow(&mut self, task_id: &TaskId, actor: AgentKind) {
        let state_snapshot = self
            .workflow_engine
            .state(task_id)
            .map(|s| (s.phase.clone(), s.current_agent));
        if let Some((phase, current_agent)) = state_snapshot {
            if let Some(task) = self.task_store.get_mut(task_id) {
                match &phase {
                    WorkflowPhase::PendingReview => {
                        task.set_status(TaskStatus::PendingReview, actor);
                        task.assign_to(Some(AgentKind::Human), actor);
                    }
                    WorkflowPhase::Running => {
                        task.assign_to(Some(current_agent), actor);
                    }
                    WorkflowPhase::AwaitingApproval { .. } => {
                        task.assign_to(Some(AgentKind::Human), actor);
                    }
                    _ => {}
                }
            }
        }
    }

    /// Sends a human-attention notification for the given task and reason.
    ///
    /// Delegates to [`Notifier::notify`] with the title `"ClawMux"` and a
    /// body of `"[<task_id>] <reason>"`. Debouncing is handled inside `Notifier`.
    fn notify_human(&mut self, reason: &str, task_id: &TaskId) {
        let body = format!("[{}] {}", task_id, reason);
        self.notifier.notify("ClawMux", &body);
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
                        self.backend
                            .check_session_statuses(sessions_to_check, self.async_tx.clone());
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
                // Intercept streaming for the research tab before the pipeline handler.
                if task_id == App::research_task_id() {
                    let text: String = parts
                        .iter()
                        .filter_map(|p| {
                            if let crate::opencode::types::MessagePart::Text { text } = p {
                                Some(text.clone())
                            } else {
                                None
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    self.research_state.push_streaming(&message_id, text);
                    return vec![];
                }
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
                detail,
            } => {
                // Show tool activity in the research chat so the user can follow agent progress.
                if task_id == App::research_task_id() {
                    let msg = if let Some(d) = detail {
                        format!("[Tool: {}] {} -- {}", tool, status, d)
                    } else {
                        format!("[Tool: {}] {}", tool, status)
                    };
                    self.research_state.push_system_message(msg);
                    return vec![];
                }
                self.tab2_state.push_tool(&task_id, tool, status, detail);
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
                    task.set_status(TaskStatus::InProgress, AgentKind::Human);
                    task.assign_to(Some(AgentKind::Intake), AgentKind::Human);
                }
                let mut msgs = self.workflow_engine.process(AppMessage::StartTask {
                    task_id: task_id.clone(),
                });
                msgs.push(AppMessage::TaskUpdated {
                    task_id: task_id.clone(),
                });
                msgs
            }

            // --- ResumeTask: re-enter pipeline at the last known agent ---
            AppMessage::ResumeTask { task_id } => {
                // Determine resume agent using priority:
                // 1. Workflow engine Errored state's current_agent
                // 2. Task's assigned_to (excluding Human)
                // 3. Fallback: Intake
                let resume_agent = self
                    .workflow_engine
                    .state(&task_id)
                    .filter(|s| matches!(s.phase, WorkflowPhase::Errored { .. }))
                    .map(|s| s.current_agent)
                    .or_else(|| {
                        self.task_store
                            .get(&task_id)
                            .and_then(|t| t.assigned_to)
                            .filter(|a| *a != AgentKind::Human)
                    })
                    .unwrap_or(AgentKind::Intake);

                if let Some(task) = self.task_store.get_mut(&task_id) {
                    task.assign_to(Some(resume_agent), AgentKind::Human);
                    task.log_change(
                        AgentKind::Human,
                        format!("Task resumed at {}", resume_agent.display_name()),
                    );
                }
                let mut msgs = self.workflow_engine.resume(task_id.clone(), resume_agent);
                msgs.push(AppMessage::TaskUpdated { task_id });
                msgs
            }

            // --- SessionError: record error in work log then forward to engine ---
            AppMessage::SessionError {
                task_id,
                session_id,
                error,
            } => {
                // Route commit session errors before the normal pipeline.
                if let Some(commit_task_id) = self.pending_commit_sessions.remove(&session_id) {
                    return vec![AppMessage::CommitFailed {
                        task_id: commit_task_id,
                        error,
                    }];
                }

                // Intercept errors for the research session.
                // Use task_id comparison so that child sessions (OpenCode >= 1.2 conductor/child
                // architecture) are handled correctly even when session_id differs.
                if task_id == App::research_task_id() {
                    self.research_state.session_id = None;
                    self.research_state.session_creating = false;
                    return vec![AppMessage::ResearchResponseError { error }];
                }

                // Guard: ignore errors from child sessions.  When the engine
                // has a registered primary session_id, only that session's
                // errors should advance the pipeline.  A None expected_session
                // means no session is tracked yet (e.g. CreateSession failed
                // before SSE registration), so those errors pass through.
                let expected_session = self
                    .workflow_engine
                    .state(&task_id)
                    .and_then(|s| s.session_id.clone());
                if let Some(ref expected) = expected_session {
                    if *expected != session_id {
                        tracing::debug!(
                            "Ignoring child SessionError for task {} (session {} != expected {})",
                            task_id,
                            session_id,
                            expected
                        );
                        return vec![];
                    }
                }

                let current_agent = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|s| s.current_agent)
                    .unwrap_or(AgentKind::Intake);
                self.tab2_state.clear_awaiting(&task_id);
                self.tab2_state.clear_thinking(&task_id);
                self.tab2_state.push_banner(
                    &task_id,
                    format!("ERROR ({}): {}", current_agent.display_name(), error),
                );
                self.notify_human(&format!("ERROR: {}", error), &task_id);
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
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    task.assign_to(Some(AgentKind::Human), current_agent);
                }
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
                // Record the answer and check for OpenCode request ID.
                let opencode_request_id = if let Some(task) = self.task_store.get_mut(&task_id) {
                    if let Some(q) = task.questions.get_mut(question_index) {
                        q.answer = Some(answer.clone());
                        q.opencode_request_id.clone()
                    } else {
                        None
                    }
                } else {
                    None
                };
                // Branch on whether this is a backend-native question (has a request ID)
                // or a parsed question extracted from agent output.
                let mut msgs = if let Some(req_id) = opencode_request_id {
                    // Backend-native: the original session is still active; send the
                    // reply and resume the workflow without creating a new session.
                    self.backend.reply_question(
                        task_id.clone(),
                        req_id,
                        answer.clone(),
                        self.async_tx.clone(),
                    );
                    self.workflow_engine.resume_from_answer(&task_id);
                    // Restart the idle timer so the resumed session gets a fresh window.
                    let agent_name = self
                        .workflow_engine
                        .state(&task_id)
                        .map(|s| s.current_agent.display_name().to_string())
                        .unwrap_or_default();
                    self.tab2_state.set_awaiting_response(&task_id, agent_name);
                    vec![]
                } else {
                    // Parsed question: the session already completed, so the engine
                    // must create a new session to continue the workflow.
                    self.workflow_engine.process(AppMessage::HumanAnswered {
                        task_id: task_id.clone(),
                        question_index,
                        answer,
                    })
                };
                // Rebuild answer textareas so the answered question's textarea is removed.
                if let Some(task) = self.task_store.get(&task_id) {
                    let task = task.clone();
                    self.questions_state.sync_answer_inputs(&task);
                }
                let next_agent = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|wf| wf.current_agent);
                if let Some(agent) = next_agent {
                    if let Some(task) = self.task_store.get_mut(&task_id) {
                        task.assign_to(Some(agent), AgentKind::Human);
                    }
                }
                msgs.push(AppMessage::TaskUpdated { task_id });
                msgs
            }

            // --- Workflow messages: forward to engine ---
            AppMessage::AgentCompleted { .. }
            | AppMessage::AgentKickedBack { .. }
            | AppMessage::AgentAskedQuestion { .. } => self.workflow_engine.process(msg),

            // Only forward the first (primary) SessionCreated to the engine.
            // OpenCode spawns child sessions internally; when the engine already
            // has a session_id set, the incoming event is from a child and must
            // be ignored to prevent the engine from double-registering.
            AppMessage::SessionCreated {
                ref task_id,
                ref session_id,
            } => {
                let already_has_session = self
                    .workflow_engine
                    .state(task_id)
                    .is_some_and(|s| s.session_id.is_some());
                if already_has_session {
                    tracing::debug!(
                        "Ignoring child SessionCreated for task {} (session {})",
                        task_id,
                        session_id
                    );
                    vec![]
                } else {
                    self.workflow_engine.process(msg)
                }
            }

            AppMessage::HumanApprovedReview { task_id } => {
                self.review_state
                    .push_banner(&task_id, "Review approved".to_string());
                let mut msgs = self
                    .workflow_engine
                    .process(AppMessage::HumanApprovedReview {
                        task_id: task_id.clone(),
                    });
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    task.set_status(TaskStatus::Completed, AgentKind::Human);
                    task.assign_to(Some(AgentKind::Human), AgentKind::Human);
                }
                msgs.push(AppMessage::TaskUpdated { task_id });
                msgs
            }

            AppMessage::HumanApprovedCommit {
                task_id,
                commit_message,
                mut file_paths,
            } => {
                let first_line = commit_message.lines().next().unwrap_or("").to_string();
                self.tab2_state
                    .push_banner(&task_id, format!("[Commit] Committing: {}", first_line));

                // Set the task status to Completed and persist it to disk BEFORE staging.
                // This ensures the committed file already has the final status so that
                // CommitCompleted's set_status call is a no-op, avoiding a spurious
                // post-commit modification that would show up as an unstaged change.
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    task.set_status(TaskStatus::Completed, AgentKind::Human);
                }
                if let Err(e) = self.task_store.persist(&task_id) {
                    tracing::warn!("Failed to persist task before commit staging: {}", e);
                }

                // Include the task's own markdown file so status/work-log changes are committed.
                if let Some(task) = self.task_store.get(&task_id) {
                    let task_path = task.file_path.to_string_lossy().to_string();
                    if !file_paths.contains(&task_path) {
                        file_paths.push(task_path);
                    }
                }
                // Use the CodeReview agent's configured model (from its frontmatter) so
                // the commit session uses the same Sonnet build as the rest of the pipeline,
                // rather than the global provider default which may not be a valid model id.
                let commit_model = self
                    .agent_models
                    .get(&AgentKind::CodeReview)
                    .cloned()
                    .or_else(|| self.default_model.clone());

                self.backend.commit_changes(
                    task_id,
                    commit_message,
                    file_paths,
                    commit_model,
                    self.session_map.clone(),
                    self.async_tx.clone(),
                );
                vec![]
            }

            AppMessage::RegisterCommitSession {
                task_id,
                session_id,
            } => {
                self.pending_commit_sessions.insert(session_id, task_id);
                vec![]
            }

            AppMessage::CommitCompleted { task_id } => {
                self.tab2_state.push_banner(
                    &task_id,
                    "[Commit] Changes committed successfully".to_string(),
                );
                // Transition the workflow engine to Completed.
                self.workflow_engine
                    .process(AppMessage::HumanApprovedReview {
                        task_id: task_id.clone(),
                    });
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    task.set_status(TaskStatus::Completed, AgentKind::Human);
                }
                vec![AppMessage::TaskUpdated { task_id }]
            }

            AppMessage::CommitFailed { task_id, error } => {
                self.tab2_state
                    .push_banner(&task_id, format!("[Commit] Commit failed: {}", error));
                self.notify_human("Commit failed", &task_id);
                tracing::warn!("Commit failed for task {}: {}", task_id, error);
                vec![]
            }

            AppMessage::HumanApprovedTransition { task_id } => {
                self.tab2_state
                    .push_banner(&task_id, "Transition approved by human".to_string());
                let mut msgs = self
                    .workflow_engine
                    .process(AppMessage::HumanApprovedTransition {
                        task_id: task_id.clone(),
                    });
                let next_agent = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|wf| wf.current_agent);
                if let Some(agent) = next_agent {
                    if let Some(task) = self.task_store.get_mut(&task_id) {
                        task.assign_to(Some(agent), AgentKind::Human);
                    }
                }
                msgs.push(AppMessage::TaskUpdated { task_id });
                msgs
            }
            AppMessage::HumanRequestedRevisions { task_id, comments } => {
                self.review_state.push_user_comments(&task_id, &comments);
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    task.assign_to(Some(AgentKind::CodeReview), AgentKind::Human);
                }
                let mut msgs = self
                    .workflow_engine
                    .process(AppMessage::HumanRequestedRevisions {
                        task_id: task_id.clone(),
                        comments,
                    });
                msgs.push(AppMessage::TaskUpdated { task_id });
                msgs
            }

            // --- SessionCompleted: parse agent response and dispatch semantic message ---
            AppMessage::SessionCompleted {
                task_id,
                session_id,
                response_text,
            } => {
                // Route commit sessions before the normal pipeline.
                if let Some(commit_task_id) = self.pending_commit_sessions.remove(&session_id) {
                    self.tab2_state.push_banner(
                        &commit_task_id,
                        "[Commit] Changes committed successfully".to_string(),
                    );
                    return vec![AppMessage::CommitCompleted {
                        task_id: commit_task_id,
                    }];
                }

                // Intercept completions for the research session (session persists; do not remove).
                // Use task_id comparison so that child sessions (OpenCode >= 1.2 conductor/child
                // architecture) are handled correctly even when session_id differs.
                if task_id == App::research_task_id() {
                    return vec![AppMessage::ResearchResponseCompleted];
                }

                // Guard: only advance the workflow when the current phase is Running.
                // In OpenCode >= 1.2, primary ("conductor") sessions delegate work to
                // child sessions that fire session.idle when the agent's turn completes.
                // The conductor stays alive indefinitely without firing its own
                // session.idle, so a session-id equality check incorrectly drops the
                // child's completion and leaves the task hung.  The phase check prevents
                // double-advancement instead: once the first completion transitions the
                // workflow to AwaitingApproval (or any non-Running state), subsequent
                // completions from the same turn are silently dropped.
                let phase = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|s| s.phase.clone());
                if !matches!(phase, Some(WorkflowPhase::Running)) {
                    tracing::debug!(
                        "Ignoring SessionCompleted for task {} (session {}) - phase is {:?}, not Running",
                        task_id,
                        session_id,
                        phase
                    );
                    return vec![];
                }

                self.tab2_state.clear_awaiting(&task_id);

                // Drain any queued steering prompt before advancing the workflow.
                // If found, send it to the same session so the agent processes the
                // user's input in a new turn. The workflow will advance when that
                // follow-up turn completes (or errors).
                if let Some(queued_text) = self.tab2_state.take_queued_prompt(&task_id) {
                    self.tab2_state
                        .push_banner(&task_id, format!("[You] {}", queued_text));
                    return vec![AppMessage::SendPrompt {
                        task_id,
                        session_id,
                        prompt: queued_text,
                    }];
                }

                self.tab2_state.clear_thinking(&task_id);
                self.tab2_state.strip_response_json(&task_id);
                let current_agent = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|s| s.current_agent);

                match parse_response(&response_text) {
                    Ok(AgentResponse::Complete {
                        summary,
                        updates,
                        commit_message,
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
                        // Store the commit message when CodeReview agent completes.
                        if agent == AgentKind::CodeReview {
                            if let Some(msg) = commit_message {
                                self.tab4_state.set_commit_message(&task_id, msg);
                            }
                        }
                        self.tab2_state.push_banner(
                            &task_id,
                            format!("{} completed: {}", agent.display_name(), summary),
                        );
                        // Push the full summary to the review timeline when it is a CodeReview agent.
                        if agent == AgentKind::CodeReview {
                            self.review_state.push_agent_summary(
                                &task_id,
                                agent.display_name(),
                                &summary,
                            );
                        }
                        let mut msgs = self.workflow_engine.process(AppMessage::AgentCompleted {
                            task_id: task_id.clone(),
                            agent,
                            summary,
                        });
                        self.sync_task_with_workflow(&task_id, agent);
                        // Notify if the phase transitioned to one requiring human attention.
                        match self.workflow_engine.state(&task_id).map(|s| &s.phase) {
                            Some(WorkflowPhase::AwaitingApproval { .. }) => {
                                self.notify_human(
                                    &format!("Awaiting approval for {}", agent.display_name()),
                                    &task_id,
                                );
                            }
                            Some(WorkflowPhase::PendingReview) => {
                                self.notify_human("All agents complete - review needed", &task_id);
                            }
                            _ => {}
                        }
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
                                opencode_request_id: None,
                            });
                        }
                        // Sync answer textareas so the new question gets an input widget.
                        if let Some(task) = self.task_store.get(&task_id) {
                            let task = task.clone();
                            self.questions_state.sync_answer_inputs(&task);
                        }
                        self.tab2_state.push_banner(
                            &task_id,
                            format!("{} has a question (see Task Details)", agent.display_name()),
                        );
                        self.notify_human(
                            &format!("{} has a question", agent.display_name()),
                            &task_id,
                        );
                        let mut msgs =
                            self.workflow_engine
                                .process(AppMessage::AgentAskedQuestion {
                                    task_id: task_id.clone(),
                                    agent,
                                    question,
                                });
                        if let Some(task) = self.task_store.get_mut(&task_id) {
                            task.assign_to(Some(AgentKind::Human), agent);
                        }
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
                        self.tab2_state.push_banner(
                            &task_id,
                            format!(
                                "{} kicked back to {}: {}",
                                from.display_name(),
                                to.display_name(),
                                reason
                            ),
                        );
                        self.review_state.push_kickback(
                            &task_id,
                            from.display_name(),
                            to.display_name(),
                            &reason,
                        );
                        let mut msgs = self.workflow_engine.process(AppMessage::AgentKickedBack {
                            task_id: task_id.clone(),
                            from,
                            to,
                            reason,
                        });
                        if let Some(task) = self.task_store.get_mut(&task_id) {
                            task.assign_to(Some(to), from);
                        }
                        // Notify if the kickback triggered an approval gate.
                        if matches!(
                            self.workflow_engine.state(&task_id).map(|s| &s.phase),
                            Some(WorkflowPhase::AwaitingApproval { .. })
                        ) {
                            self.notify_human(
                                &format!("Awaiting approval for {}", to.display_name()),
                                &task_id,
                            );
                        }
                        msgs.push(AppMessage::TaskUpdated { task_id });
                        msgs
                    }
                    Err(_) => {
                        let agent = current_agent.unwrap_or(AgentKind::Intake);
                        let preview: String = response_text
                            .chars()
                            .rev()
                            .take(500)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect();
                        tracing::warn!(
                            "Could not parse structured output for task {} (response_text len={}); last 500 chars: {}",
                            task_id,
                            response_text.len(),
                            preview
                        );
                        self.tab2_state.push_banner(
                            &task_id,
                            format!(
                                "{} could not produce a structured response. Use [p] to steer the agent.",
                                agent.display_name()
                            ),
                        );
                        // Clear the stale session_id so a fresh SessionCreated can register
                        // the replacement session (prevents [p] from targeting a dead session).
                        self.workflow_engine.reset_session_id(&task_id);
                        self.backend.create_idle_session(
                            task_id.clone(),
                            agent,
                            self.session_map.clone(),
                            self.async_tx.clone(),
                        );
                        vec![AppMessage::TaskUpdated { task_id }]
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

                // Look up the model for this agent; fall back to the global default.
                let model = self
                    .agent_models
                    .get(&agent)
                    .cloned()
                    .or_else(|| self.default_model.clone());
                let model_display = model
                    .as_ref()
                    .map(|m| m.to_string())
                    .unwrap_or_else(|| "default".to_string());
                self.tab2_state.push_banner(
                    &task_id,
                    format!("[{}] Starting with model: {}", agent_name, model_display),
                );

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
                let prompt_preview = truncate_str(&prompt, 500);
                self.tab2_state
                    .push_banner(&task_id, format!("[Prompt] {}", prompt_preview));

                self.backend.create_session(
                    task_id,
                    agent,
                    prompt,
                    model,
                    self.session_map.clone(),
                    self.async_tx.clone(),
                );
                vec![]
            }

            AppMessage::SendPrompt {
                task_id,
                session_id,
                prompt,
            } => {
                // Look up the current agent from the workflow engine (avoids async session_map read).
                let agent = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|s| s.current_agent)
                    .unwrap_or(AgentKind::Intake);
                let model = self
                    .agent_models
                    .get(&agent)
                    .cloned()
                    .or_else(|| self.default_model.clone());
                self.backend.send_prompt(
                    task_id,
                    session_id,
                    agent,
                    prompt,
                    model,
                    self.async_tx.clone(),
                );
                vec![]
            }

            AppMessage::PromptSent {
                task_id,
                session_id,
            } => {
                // Intercept PromptSent for the research session to store the session_id.
                if task_id == App::research_task_id() {
                    self.research_state.session_id = Some(session_id.clone());
                    self.research_state.session_creating = false;
                    // Drain any prompt queued while the session was being created.
                    if let Some(queued) = self.research_state.pending_prompt.take() {
                        let research_id = App::research_task_id();
                        let model = self.default_model.clone();
                        let agent_kind = self.research_state.mode.agent_kind();
                        self.backend.send_prompt(
                            research_id,
                            session_id,
                            agent_kind,
                            queued,
                            model,
                            self.async_tx.clone(),
                        );
                    }
                    return vec![];
                }
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
                let wf_session = self
                    .workflow_engine
                    .state(&task_id)
                    .and_then(|s| s.session_id.clone());
                let session_matches = wf_session.as_deref() == Some(&session_id);

                // Always clear the awaiting spinner when the session ID matches,
                // even if the phase is no longer Running (e.g. already Errored).
                // This prevents stale prompt_sent_at entries from causing indefinite
                // liveness polls after a session fails silently.
                // Do NOT clear when the session ID differs: the task may have been
                // restarted and a new session is now actively awaiting.
                if session_matches {
                    self.tab2_state.clear_awaiting(&task_id);
                }

                // Only escalate to SessionError if the workflow is still expecting
                // a response from this exact session (Running phase).
                let is_active = session_matches
                    && self
                        .workflow_engine
                        .state(&task_id)
                        .is_some_and(|s| s.phase == WorkflowPhase::Running);
                if is_active {
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
                self.backend
                    .abort_session(task_id, session_id, self.async_tx.clone());
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

            AppMessage::TokensUpdated {
                task_id,
                input_tokens,
                output_tokens,
                is_cumulative,
                step_id,
            } => {
                self.tab2_state.update_tokens(
                    &task_id,
                    input_tokens,
                    output_tokens,
                    is_cumulative,
                    step_id.as_deref(),
                );
                vec![]
            }

            // --- Permission handling ---
            AppMessage::PermissionAsked { task_id, request } => {
                self.tab2_state.push_banner(
                    &task_id,
                    format!("[Permission] {} requested", request.permission),
                );
                self.notify_human("Permission requested", &task_id);
                self.tab2_state.push_permission(task_id, request);
                vec![]
            }

            AppMessage::PermissionResolved {
                task_id,
                request,
                response,
                explanation,
            } => {
                self.tab2_state.resolve_permission(&task_id);
                let decision = match response.as_str() {
                    "once" => "approved once",
                    "always" => "always allowed",
                    "reject" => "rejected",
                    other => other,
                };
                self.tab2_state.push_banner(
                    &task_id,
                    format!("[Permission] {} {}", request.permission, decision),
                );

                // Compute the steering prompt message before spawning so it is sent
                // only after resolve_permission completes (avoids a race where the
                // agent receives guidance before the permission is acknowledged).
                let send_prompt_msg = if response == "reject" {
                    if let Some(text) = explanation.filter(|t| !t.trim().is_empty()) {
                        let active_session = self.workflow_engine.state(&task_id).and_then(|s| {
                            if s.phase == WorkflowPhase::Running {
                                s.session_id.clone()
                            } else {
                                None
                            }
                        });
                        if let Some(sess_id) = active_session {
                            self.tab2_state
                                .push_banner(&task_id, format!("[You] {}", text));
                            Some(AppMessage::SendPrompt {
                                task_id: task_id.clone(),
                                session_id: sess_id,
                                prompt: text,
                            })
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                self.backend.resolve_permission(
                    task_id,
                    request,
                    response,
                    send_prompt_msg,
                    self.async_tx.clone(),
                );
                vec![]
            }

            AppMessage::OpenCodeQuestionAsked {
                task_id,
                request_id,
                question,
            } => {
                // Determine which agent is currently running.
                let agent = self
                    .workflow_engine
                    .state(&task_id)
                    .map(|s| s.current_agent)
                    .unwrap_or(AgentKind::Intake);
                // Add the question to the task model with the opencode request ID.
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    task.questions.push(Question {
                        agent,
                        text: question.clone(),
                        answer: None,
                        opencode_request_id: Some(request_id.clone()),
                    });
                }
                // Sync answer textareas so the new question gets an input widget.
                if let Some(task) = self.task_store.get(&task_id) {
                    let task = task.clone();
                    self.questions_state.sync_answer_inputs(&task);
                }
                self.tab2_state.push_banner(
                    &task_id,
                    format!(
                        "{} has a question (see Questions tab)",
                        agent.display_name()
                    ),
                );
                self.notify_human(
                    &format!("{} has a question", agent.display_name()),
                    &task_id,
                );
                // Stop the idle timer while the human is answering; clear thinking indicator.
                self.tab2_state.clear_awaiting(&task_id);
                self.tab2_state.clear_thinking(&task_id);
                let mut msgs = self
                    .workflow_engine
                    .process(AppMessage::AgentAskedQuestion {
                        task_id: task_id.clone(),
                        agent,
                        question,
                    });
                if let Some(task) = self.task_store.get_mut(&task_id) {
                    task.assign_to(Some(AgentKind::Human), agent);
                }
                msgs.push(AppMessage::TaskUpdated { task_id });
                msgs
            }

            AppMessage::SessionDiffChanged {
                task_id,
                session_id,
            } => {
                self.backend
                    .get_diffs(task_id, session_id, self.async_tx.clone());
                vec![]
            }

            // --- Diff storage ---
            AppMessage::DiffReady { task_id, diffs } => {
                self.tab2_state.push_diff(&task_id, &diffs);
                self.tab4_state.set_diffs(&task_id, diffs);
                self.tab4_state.set_displayed_task(Some(&task_id));
                self.tab4_state.reset_for_diffs();
                vec![]
            }

            // --- Research tab ---
            AppMessage::ResearchPromptSubmitted { prompt } => {
                self.research_state.push_user_message(prompt.clone());
                self.research_state.awaiting_response = true;
                let research_id = App::research_task_id();
                let model = self.default_model.clone();
                let agent_kind = self.research_state.mode.agent_kind();
                if let Some(ref session_id) = self.research_state.session_id.clone() {
                    // Session already exists: send a follow-up prompt.
                    self.backend.send_prompt(
                        research_id,
                        session_id.clone(),
                        agent_kind,
                        prompt,
                        model,
                        self.async_tx.clone(),
                    );
                } else if self.research_state.session_creating {
                    // Session creation in flight: queue the prompt and notify the user.
                    self.research_state.pending_prompt = Some(prompt);
                    self.research_state.push_system_message(
                        "Queued -- will send after session is ready.".to_string(),
                    );
                } else {
                    // First prompt: create a new session.
                    self.research_state.session_creating = true;
                    self.research_state
                        .push_system_message("Creating session...".to_string());
                    self.backend.create_session(
                        research_id,
                        agent_kind,
                        prompt,
                        model,
                        self.session_map.clone(),
                        self.async_tx.clone(),
                    );
                }
                vec![]
            }

            AppMessage::ResearchResponseCompleted => {
                self.research_state.finalize_response();
                vec![]
            }

            AppMessage::ResearchResponseError { error } => {
                self.research_state.push_error(error);
                vec![]
            }
        }
    }
}

/// Truncates `s` to at most `max_chars` Unicode scalar values, appending `"..."`
/// when truncation occurs. Always produces a valid UTF-8 string regardless of
/// multi-byte characters in `s`.
fn truncate_str(s: &str, max_chars: usize) -> String {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => format!("{}...", &s[..idx]),
        None => s.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that single quotes in commit messages are shell-escaped correctly.
    ///
    /// The escaping strategy (`replace('\'', "'\\''")`) is the standard POSIX trick for
    /// embedding a literal single-quote inside a single-quoted shell string:
    ///   `it's` → `it'\''s`
    /// which the shell interprets as: `'it'` + `\'` + `'s'` = `it's`.
    #[test]
    fn test_commit_message_single_quote_escaping() {
        let raw = "fix: it's broken";
        let escaped = raw.replace('\'', "'\\''");
        assert_eq!(
            escaped, "fix: it'\\''s broken",
            "single quote should be escaped to '\\''"
        );

        // A message with no single quotes should pass through unchanged.
        let plain = "feat: add feature";
        assert_eq!(plain.replace('\'', "'\\''"), "feat: add feature");

        // Multiple single quotes are each escaped independently.
        let multi = "fix: can't won't don't";
        let escaped_multi = multi.replace('\'', "'\\''");
        assert_eq!(escaped_multi, "fix: can'\\''t won'\\''t don'\\''t");
    }

    #[test]
    fn test_truncate_str_ascii() {
        assert_eq!(truncate_str("hello", 10), "hello");
        assert_eq!(truncate_str("hello world", 5), "hello...");
    }

    #[test]
    fn test_truncate_str_multibyte() {
        // em dash U+2014 is 3 bytes; slicing at byte 80 would panic without this helper.
        // s is 81 chars: 79 'a's + em dash + 'b'.
        let s: String = "a".repeat(79) + "\u{2014}" + "b";
        // Truncating at 80 chars: takes 79 'a's + em dash, appends "..."
        assert_eq!(truncate_str(&s, 80), "a".repeat(79) + "\u{2014}" + "...");
        // Truncating at 79 chars: takes just the 79 'a's, appends "..."
        assert_eq!(truncate_str(&s, 79), "a".repeat(79) + "...");
        // No truncation when limit >= length
        assert_eq!(truncate_str(&s, 81), s);
        assert_eq!(truncate_str(&s, 100), s);
    }

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
        // 1 entry for the agent summary + 1 for the assignment to the next agent.
        assert_eq!(task.work_log.len(), 2, "work log should have two entries");
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

    /// Verifies that a Question response syncs answer_inputs so the new question gets a textarea.
    #[test]
    fn test_handle_session_completed_question_syncs_answer_inputs() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        assert_eq!(
            app.questions_state.answer_inputs.len(),
            0,
            "no answer inputs before any questions"
        );

        let response_json =
            r#"{"action":"question","question":"What is scope?","context":"Need clarity"}"#;
        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: response_json.to_string(),
        });

        assert_eq!(
            app.questions_state.answer_inputs.len(),
            1,
            "answer_inputs should have one entry for the new unanswered question"
        );
    }

    /// Verifies that sync_answer_inputs produces one textarea per unanswered question
    /// when the task directly holds multiple unanswered questions.
    #[test]
    fn test_sync_answer_inputs_multiple_unanswered_questions() {
        use crate::tasks::models::{Question, Task, TaskStatus};
        use std::path::PathBuf;

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: vec![
                Question {
                    agent: AgentKind::Intake,
                    text: "What is scope?".to_string(),
                    answer: None,
                    opencode_request_id: None,
                },
                Question {
                    agent: AgentKind::Intake,
                    text: "Which file?".to_string(),
                    answer: None,
                    opencode_request_id: None,
                },
            ],
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);
        let task = app
            .task_store
            .get(&TaskId::from_path("tasks/1.1.md"))
            .cloned()
            .unwrap();
        app.questions_state.sync_answer_inputs(&task);

        assert_eq!(
            app.questions_state.answer_inputs.len(),
            2,
            "answer_inputs should have two entries for two unanswered questions"
        );
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
        use crate::workflow::transitions::WorkflowPhase;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Unparseable response should keep the workflow in Running and emit only TaskUpdated.
        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: "I could not produce structured output.".to_string(),
        });

        // Workflow must NOT advance (no CreateSession for the next agent).
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, AppMessage::CreateSession { .. })),
            "parse failure must NOT advance to next agent, got: {msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "expected TaskUpdated after parse failure, got: {msgs:?}"
        );
        // Phase must still be Running so the user can steer.
        let state = app.workflow_engine.state(&task_id).unwrap();
        assert_eq!(
            state.phase,
            WorkflowPhase::Running,
            "workflow must remain in Running after parse failure"
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

        // Work log should contain the error entry plus the assignment-to-Human entry.
        let task = app.task_store.get(&task_id).expect("task should exist");
        assert_eq!(task.work_log.len(), 2, "work log should have two entries");
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
        // Banners (in order): agent name, model, "Creating session...", [Prompt]
        assert!(
            lines.len() >= 3,
            "expected at least 3 banner lines, got {}",
            lines.len()
        );
        assert!(
            matches!(&lines[0], ActivityLine::AgentBanner { message } if message.contains("Intake")),
            "first line should be the agent name banner: {:?}",
            lines[0]
        );
        assert!(
            matches!(&lines[1], ActivityLine::AgentBanner { message } if message.contains("model")),
            "second line should be the model banner: {:?}",
            lines[1]
        );
        assert!(
            matches!(&lines[2], ActivityLine::AgentBanner { message } if message.contains("Creating session")),
            "third line should be 'Creating session...': {:?}",
            lines[2]
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
                opencode_request_id: None,
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
                    opencode_request_id: None,
                },
                Question {
                    agent: AgentKind::Intake,
                    text: "Q2?".to_string(),
                    answer: None,
                    opencode_request_id: None,
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
                opencode_request_id: None,
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

    /// Verifies that HumanAnswered with an opencode_request_id does NOT emit CreateSession
    /// and leaves the workflow phase as Running.
    #[test]
    fn test_human_answered_opencode_question_resumes_not_creates() {
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
                opencode_request_id: Some("req-opencode-1".to_string()),
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
        app.workflow_engine.process(AppMessage::AgentAskedQuestion {
            task_id: task_id.clone(),
            agent: AgentKind::Intake,
            question: "What is the scope?".to_string(),
        });

        let msgs = app.handle_message(AppMessage::HumanAnswered {
            task_id: task_id.clone(),
            question_index: 0,
            answer: "Full scope.".to_string(),
        });

        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, AppMessage::CreateSession { .. })),
            "OpenCode-native answer must NOT emit CreateSession, got: {msgs:?}"
        );
        let state = app.workflow_engine.state(&task_id).expect("state");
        assert_eq!(
            state.phase,
            WorkflowPhase::Running,
            "workflow phase must be Running after OpenCode-native answer"
        );
    }

    /// Verifies that HumanAnswered without an opencode_request_id DOES emit CreateSession
    /// (parsed-question path: session already completed, engine must create a new one).
    #[test]
    fn test_human_answered_parsed_question_creates_session() {
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
                opencode_request_id: None,
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
        app.workflow_engine.process(AppMessage::AgentAskedQuestion {
            task_id: task_id.clone(),
            agent: AgentKind::Intake,
            question: "What is the scope?".to_string(),
        });

        let msgs = app.handle_message(AppMessage::HumanAnswered {
            task_id: task_id.clone(),
            question_index: 0,
            answer: "Full scope.".to_string(),
        });

        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::CreateSession { .. })),
            "parsed-question answer must emit CreateSession, got: {msgs:?}"
        );
    }

    /// Verifies that OpenCodeQuestionAsked clears prompt_sent_at so check_timeouts returns empty.
    #[test]
    fn test_opencode_question_clears_awaiting() {
        use std::time::Duration;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Simulate a prompt being sent (starts idle timer).
        app.tab2_state
            .set_awaiting_response(&task_id, "Intake Agent".to_string());

        // A question arrives -- should clear the timer.
        app.handle_message(AppMessage::OpenCodeQuestionAsked {
            task_id: task_id.clone(),
            request_id: "req-1".to_string(),
            question: "What is the scope?".to_string(),
        });

        // The timer must be cleared: check_timeouts with zero duration returns empty.
        let timed_out = app.tab2_state.check_timeouts(Duration::ZERO);
        assert!(
            timed_out.is_empty(),
            "check_timeouts should be empty after OpenCodeQuestionAsked clears the timer"
        );
    }

    /// Verifies that DiffReady stores diffs without switching tabs, and resets navigation.
    #[test]
    fn test_handle_diff_ready_stores_diffs() {
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
        assert_eq!(app.active_tab, 0, "should NOT switch tabs");
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

    /// Verifies that DiffReady pushes a DiffSummary to tab2_state and stores diffs in tab4_state.
    #[test]
    fn test_handle_diff_ready_pushes_to_tab2() {
        use crate::opencode::types::{DiffHunk, DiffLine, DiffLineKind, DiffStatus, FileDiff};
        use crate::tui::tabs::agent_activity::ActivityLine;

        let mut app = App::test_default();
        let task_id = TaskId::from_path("tasks/1.1.md");

        let diffs = vec![FileDiff {
            path: "src/main.rs".to_string(),
            status: DiffStatus::Modified,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Added,
                        content: "fn main() {}".to_string(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Removed,
                        content: "fn old() {}".to_string(),
                    },
                ],
            }],
        }];

        app.handle_message(AppMessage::DiffReady {
            task_id: task_id.clone(),
            diffs,
        });

        // tab2_state should have a DiffSummary line.
        let tab2_lines = app.tab2_state.lines_for(&task_id);
        assert_eq!(tab2_lines.len(), 1, "tab2 should have one DiffSummary line");
        assert!(
            matches!(&tab2_lines[0], ActivityLine::DiffSummary { files } if files.len() == 1),
            "tab2 line should be a DiffSummary with 1 file; got: {:?}",
            tab2_lines[0]
        );

        // tab4_state should also have the diffs stored.
        let tab4_diffs = app.tab4_state.diffs_for(&task_id);
        assert_eq!(tab4_diffs.len(), 1, "tab4 should have 1 FileDiff");
        assert_eq!(tab4_diffs[0].path, "src/main.rs");
    }

    /// Helper: advance the workflow engine to the CodeReview agent stage.
    fn make_task_at_code_review(app: &mut App) -> TaskId {
        let task_id = make_task_in_progress(app);
        // Advance Intake -> Design -> Planning -> Implementation -> CodeQuality -> SecurityReview -> CodeReview (6 steps).
        for _ in 0..6 {
            let agent = app.workflow_engine.state(&task_id).unwrap().current_agent;
            app.workflow_engine.process(AppMessage::AgentCompleted {
                task_id: task_id.clone(),
                agent,
                summary: "done".to_string(),
            });
        }
        task_id
    }

    /// Verifies that a CodeReview SessionCompleted pushes the full summary to review_state.
    #[test]
    fn test_session_completed_code_review_pushes_to_review_state() {
        use crate::tui::tabs::review::ReviewEntry;

        let mut app = App::test_default();
        let task_id = make_task_at_code_review(&mut app);

        let response_json = r#"{"action":"complete","summary":"The code looks well-structured and passes all checks."}"#;
        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-cr".to_string(),
            response_text: response_json.to_string(),
        });

        let entries = app.review_state.entries_for(&task_id);
        assert!(
            !entries.is_empty(),
            "review_state should have at least one entry after CodeReview completion"
        );
        assert!(
            entries.iter().any(|e| matches!(
                e,
                ReviewEntry::AgentSummary { agent, summary }
                    if agent == "Code Review Agent"
                        && summary.contains("The code looks well-structured")
            )),
            "review_state should contain the full summary; entries: {:?}",
            entries
        );
    }

    /// Verifies that HumanRequestedRevisions records comments in review_state.
    #[test]
    fn test_human_requested_revisions_records_in_review_state() {
        use crate::tui::tabs::review::ReviewEntry;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);
        let comments = vec![
            "src/main.rs:1-3: Rename this variable".to_string(),
            "src/lib.rs:10: Remove unused import".to_string(),
        ];

        app.handle_message(AppMessage::HumanRequestedRevisions {
            task_id: task_id.clone(),
            comments: comments.clone(),
        });

        let entries = app.review_state.entries_for(&task_id);
        assert!(
            !entries.is_empty(),
            "review_state should have an entry after HumanRequestedRevisions"
        );
        assert!(
            entries.iter().any(|e| matches!(
                e,
                ReviewEntry::UserComments { comments: c } if c == &comments
            )),
            "review_state should contain a UserComments entry; entries: {:?}",
            entries
        );
    }

    /// Verifies that a kickback from CodeReview records a Kickback entry in review_state.
    #[test]
    fn test_kickback_from_code_review_records_in_review_state() {
        use crate::tui::tabs::review::ReviewEntry;

        let mut app = App::test_default();
        let task_id = make_task_at_code_review(&mut app);

        let response_json = r#"{"action":"kickback","target_agent":"Implementation Agent","reason":"Missing unit tests for edge cases."}"#;
        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-cr".to_string(),
            response_text: response_json.to_string(),
        });

        let entries = app.review_state.entries_for(&task_id);
        assert!(
            !entries.is_empty(),
            "review_state should have an entry after kickback"
        );
        assert!(
            entries.iter().any(|e| matches!(
                e,
                ReviewEntry::Kickback { from, to, reason }
                    if from == "Code Review Agent"
                        && to == "Implementation Agent"
                        && reason.contains("Missing unit tests")
            )),
            "review_state should contain a Kickback entry; entries: {:?}",
            entries
        );
    }

    /// Verifies that when a queued steering prompt exists on SessionCompleted, it is
    /// dispatched as SendPrompt to the same session and the workflow does NOT advance.
    #[test]
    fn test_session_completed_dispatches_queued_prompt() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Queue a steering prompt for this task.
        app.tab2_state
            .queue_prompt(task_id.clone(), "please add more tests".to_string());

        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: r#"{"action":"complete","summary":"done"}"#.to_string(),
        });

        // The queued prompt should be dispatched as SendPrompt to the same session.
        assert_eq!(msgs.len(), 1, "expected exactly one message: {msgs:?}");
        assert!(
            matches!(&msgs[0], AppMessage::SendPrompt { session_id, prompt, .. }
                if session_id == "sess-1" && prompt == "please add more tests"),
            "expected SendPrompt with queued text, got: {msgs:?}"
        );

        // The queue should now be empty.
        assert_eq!(
            app.tab2_state.take_queued_prompt(&task_id),
            None,
            "queue should be empty after dispatch"
        );

        // The workflow should NOT have advanced (no CreateSession).
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, AppMessage::CreateSession { .. })),
            "workflow should not advance when queued prompt was dispatched"
        );
    }

    /// Verifies that when no queued prompt exists, SessionCompleted advances the workflow normally.
    #[test]
    fn test_session_completed_no_queue_advances_workflow() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // No queued prompt.
        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: r#"{"action":"complete","summary":"Intake done"}"#.to_string(),
        });

        // Workflow should advance: CreateSession for Design.
        assert!(
            msgs.iter().any(|m| matches!(m, AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Design)),
            "workflow should advance to Design when no queue, got: {msgs:?}"
        );
    }

    /// Verifies that [You - queued] banner appears in the activity buffer when a
    /// queued prompt is dispatched at the end of a turn.
    #[test]
    fn test_session_completed_queued_prompt_shows_banner() {
        use crate::tui::tabs::agent_activity::ActivityLine;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        app.tab2_state
            .queue_prompt(task_id.clone(), "steer this way".to_string());

        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: r#"{"action":"complete","summary":"done"}"#.to_string(),
        });

        let lines = app.tab2_state.lines_for(&task_id);
        assert!(
            lines
                .iter()
                .any(|l| matches!(l, ActivityLine::AgentBanner { message }
                if message.contains("steer this way"))),
            "expected [You] banner for queued prompt in activity buffer; lines: {lines:?}"
        );
    }

    /// Verifies that StartTask sets assigned_to to Intake and produces 2 work log entries.
    #[test]
    fn test_start_task_assigns_to_intake_and_logs() {
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

        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.assigned_to,
            Some(AgentKind::Intake),
            "assigned_to should be Intake after StartTask"
        );
        // 2 work log entries: status change + assignment.
        assert_eq!(task.work_log.len(), 2, "work log should have 2 entries");
    }

    /// Verifies that SessionCompleted (Complete) mid-pipeline sets assigned_to to the next agent.
    #[test]
    fn test_session_completed_complete_assigns_next_agent() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        let response_json = r#"{"action":"complete","summary":"Intake done","updates":{}}"#;
        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: response_json.to_string(),
        });

        // After Intake completes, workflow advances to Design.
        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.assigned_to,
            Some(AgentKind::Design),
            "assigned_to should advance to Design after Intake completes"
        );
    }

    /// Verifies that when CodeReview completes, status is set to PendingReview and
    /// assigned_to is Human.
    #[test]
    fn test_session_completed_code_review_sets_pending_review() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Advance pipeline through all agents up to and including CodeReview.
        // Pipeline: Intake -> Design -> Planning -> Implementation -> CodeQuality -> SecurityReview -> CodeReview
        let agents_before_code_review = [
            AgentKind::Intake,
            AgentKind::Design,
            AgentKind::Planning,
            AgentKind::Implementation,
            AgentKind::CodeQuality,
            AgentKind::SecurityReview,
        ];
        for agent in agents_before_code_review {
            app.workflow_engine.process(AppMessage::AgentCompleted {
                task_id: task_id.clone(),
                agent,
                summary: "done".to_string(),
            });
        }

        // Now simulate CodeReview completing.
        let response_json = r#"{"action":"complete","summary":"Code review passed","updates":{}}"#;
        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-cr".to_string(),
            response_text: response_json.to_string(),
        });

        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.status,
            crate::tasks::models::TaskStatus::PendingReview,
            "status should be PendingReview after CodeReview completes"
        );
        assert_eq!(
            task.assigned_to,
            Some(AgentKind::Human),
            "assigned_to should be Human while awaiting code review approval"
        );
    }

    /// Verifies that HumanApprovedReview sets status to Completed and assigned_to to Human.
    #[test]
    fn test_human_approved_review_sets_completed() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::PendingReview,
            assigned_to: Some(AgentKind::Human),
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

        let msgs = app.handle_message(AppMessage::HumanApprovedReview {
            task_id: task_id.clone(),
        });

        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.status,
            TaskStatus::Completed,
            "status should be Completed after HumanApprovedReview"
        );
        assert_eq!(
            task.assigned_to,
            Some(AgentKind::Human),
            "assigned_to should remain Human after approval"
        );
        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "HumanApprovedReview should emit TaskUpdated"
        );
    }

    /// Verifies that a Kickback response sets assigned_to to the target agent.
    #[test]
    fn test_session_completed_kickback_assigns_target() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Advance to CodeQuality.
        for _ in 0..4 {
            app.workflow_engine.process(AppMessage::AgentCompleted {
                task_id: task_id.clone(),
                agent: app.workflow_engine.state(&task_id).unwrap().current_agent,
                summary: "done".to_string(),
            });
        }

        let response_json =
            r#"{"action":"kickback","target_agent":"Implementation Agent","reason":"Needs tests"}"#;
        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: response_json.to_string(),
        });

        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.assigned_to,
            Some(AgentKind::Implementation),
            "assigned_to should be Implementation after kickback"
        );
    }

    /// Verifies that a Question response sets assigned_to to Human.
    #[test]
    fn test_session_completed_question_assigns_human() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        let response_json =
            r#"{"action":"question","question":"What is scope?","context":"Need clarity"}"#;
        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: response_json.to_string(),
        });

        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.assigned_to,
            Some(AgentKind::Human),
            "assigned_to should be Human while waiting for answer"
        );
    }

    /// Verifies that HumanAnswered restores assigned_to to the current agent.
    #[test]
    fn test_human_answered_restores_current_agent() {
        use crate::tasks::models::{Question, Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            // Pre-seed: Human currently owns it (waiting for answer).
            assigned_to: Some(AgentKind::Human),
            description: "desc".to_string(),
            starting_prompt: None,
            questions: vec![Question {
                agent: AgentKind::Intake,
                text: "What is the scope?".to_string(),
                answer: None,
                opencode_request_id: None,
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

        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.assigned_to,
            Some(AgentKind::Intake),
            "assigned_to should be restored to the current workflow agent after answer"
        );
    }

    /// Verifies that HumanRequestedRevisions sets assigned_to to CodeReview.
    #[test]
    fn test_human_requested_revisions_assigns_code_review() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::PendingReview,
            assigned_to: Some(AgentKind::Human),
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

        let msgs = app.handle_message(AppMessage::HumanRequestedRevisions {
            task_id: task_id.clone(),
            comments: vec!["Please fix the tests.".to_string()],
        });

        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.assigned_to,
            Some(AgentKind::CodeReview),
            "assigned_to should be CodeReview after HumanRequestedRevisions"
        );
        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "HumanRequestedRevisions should emit TaskUpdated"
        );
    }

    /// Verifies that HumanApprovedTransition sets assigned_to to the next pipeline agent.
    #[test]
    fn test_human_approved_transition_assigns_next_agent() {
        let mut app = App::test_default();
        // Build a task and start it with approval_gate enabled so we hit AwaitingApproval.
        use crate::tasks::models::{Task, TaskStatus};

        let task = Task {
            id: TaskId::from_path("tasks/2.1.md"),
            story_name: "2. Story".to_string(),
            name: "2.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: "desc".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/2.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();
        app.task_store.insert(task);
        // Enable the approval gate so the engine reaches AwaitingApproval.
        app.workflow_engine.set_approval_gate(true);
        app.workflow_engine.process(AppMessage::StartTask {
            task_id: task_id.clone(),
        });
        // Simulate Intake completing -- engine should transition to AwaitingApproval.
        app.workflow_engine.process(AppMessage::AgentCompleted {
            task_id: task_id.clone(),
            agent: AgentKind::Intake,
            summary: "done".to_string(),
        });

        app.handle_message(AppMessage::HumanApprovedTransition {
            task_id: task_id.clone(),
        });

        let task = app.task_store.get(&task_id).unwrap();
        // After approval, the engine starts Design; assigned_to should be Design.
        assert_eq!(
            task.assigned_to,
            Some(AgentKind::Design),
            "assigned_to should advance to Design after HumanApprovedTransition"
        );
        assert!(
            task.work_log.iter().any(
                |e| matches!(e, crate::tasks::models::WorkLogEntry::Parsed { description, .. }
                    if description.contains("Design Agent"))
            ),
            "work log should record the assignment to Design Agent"
        );
    }

    /// Verifies that ResumeTask uses the Errored workflow state's current_agent.
    #[test]
    fn test_handle_resume_task_after_error() {
        use crate::tasks::models::{Task, TaskStatus};
        use crate::workflow::transitions::WorkflowPhase;

        let mut app = App::test_default();
        // Insert an InProgress task.
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: Some(AgentKind::Human),
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

        // Put the workflow engine in Errored state at Design.
        // Use resume() to set up the engine at Design, then inject an error via process().
        app.workflow_engine
            .resume(task_id.clone(), AgentKind::Design);
        app.workflow_engine.process(AppMessage::SessionError {
            task_id: task_id.clone(),
            session_id: "s1".to_string(),
            error: "network error".to_string(),
        });
        // Confirm it is Errored at Design.
        let s = app.workflow_engine.state(&task_id).unwrap();
        assert!(matches!(s.phase, WorkflowPhase::Errored { .. }));
        assert_eq!(s.current_agent, AgentKind::Design);

        let msgs = app.handle_message(AppMessage::ResumeTask {
            task_id: task_id.clone(),
        });

        // Should resume at Design (from errored state) and emit CreateSession + TaskUpdated.
        assert!(
            msgs.iter().any(|m| matches!(
                m,
                AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Design
            )),
            "should resume at Design (errored agent), got: {msgs:?}"
        );
        assert!(msgs
            .iter()
            .any(|m| matches!(m, AppMessage::TaskUpdated { .. })));
        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(task.assigned_to, Some(AgentKind::Design));
    }

    /// Verifies that ResumeTask uses assigned_to when no errored workflow state exists (crash scenario).
    #[test]
    fn test_handle_resume_task_no_workflow_state_uses_assigned_to() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            // Persisted assigned_to from before the crash.
            assigned_to: Some(AgentKind::Implementation),
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
        // No workflow engine state (simulates crash).

        let msgs = app.handle_message(AppMessage::ResumeTask {
            task_id: task_id.clone(),
        });

        assert!(
            msgs.iter().any(|m| matches!(
                m,
                AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Implementation
            )),
            "should resume at Implementation (from assigned_to), got: {msgs:?}"
        );
        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(task.assigned_to, Some(AgentKind::Implementation));
    }

    /// Verifies that ResumeTask falls back to Intake when assigned_to is Human.
    #[test]
    fn test_handle_resume_task_assigned_to_human_falls_back_to_intake() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: Some(AgentKind::Human),
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
        // No workflow engine state; assigned_to is Human -> fallback to Intake.

        let msgs = app.handle_message(AppMessage::ResumeTask {
            task_id: task_id.clone(),
        });

        assert!(
            msgs.iter().any(|m| matches!(
                m,
                AppMessage::CreateSession { agent, .. } if *agent == AgentKind::Intake
            )),
            "should fall back to Intake when assigned_to is Human, got: {msgs:?}"
        );
        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(task.assigned_to, Some(AgentKind::Intake));
    }

    // --- Child session guard tests ---

    /// Verifies that a `SessionCreated` arriving when the engine already has a session_id
    /// (i.e. primary session is registered) is ignored and not forwarded to the engine.
    #[test]
    fn test_child_session_created_not_forwarded_to_engine() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Register the primary session so the engine has session_id = Some("primary").
        app.workflow_engine.process(AppMessage::SessionCreated {
            task_id: task_id.clone(),
            session_id: "primary".to_string(),
        });

        // A child SessionCreated should be ignored.
        let msgs = app.handle_message(AppMessage::SessionCreated {
            task_id: task_id.clone(),
            session_id: "child-sess".to_string(),
        });

        assert!(
            msgs.is_empty(),
            "child SessionCreated should produce no messages, got: {msgs:?}"
        );
        // Engine session_id must still point to the primary session.
        let session_id = app
            .workflow_engine
            .state(&task_id)
            .and_then(|s| s.session_id.clone());
        assert_eq!(
            session_id.as_deref(),
            Some("primary"),
            "engine session_id must remain the primary session"
        );
    }

    /// Verifies that a `SessionCreated` arriving when no session is registered yet
    /// (engine.session_id == None) IS forwarded to the engine.
    #[test]
    fn test_primary_session_created_forwarded_to_engine() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // No SessionCreated has been processed yet; session_id == None.
        let msgs = app.handle_message(AppMessage::SessionCreated {
            task_id: task_id.clone(),
            session_id: "primary".to_string(),
        });

        // Engine should return vec![] for SessionCreated (no side effects), but the
        // session_id must now be registered.
        assert!(
            msgs.is_empty(),
            "SessionCreated has no side-effect messages, got: {msgs:?}"
        );
        let session_id = app
            .workflow_engine
            .state(&task_id)
            .and_then(|s| s.session_id.clone());
        assert_eq!(
            session_id.as_deref(),
            Some("primary"),
            "engine should have registered the primary session"
        );
    }

    /// Verifies that `SessionCompleted` from a child session (different session_id than
    /// the registered primary) is accepted and advances the pipeline when the workflow
    /// phase is Running.
    ///
    /// In OpenCode >= 1.2, primary ("conductor") sessions delegate work to child
    /// sessions that fire session.idle when the agent's turn completes.  The conductor
    /// stays alive without firing its own session.idle, so we must accept child
    /// completions to avoid hanging the task.
    #[test]
    fn test_child_session_completed_advances_pipeline() {
        let mut app = App::test_default();
        let task_id = make_task_with_active_session(&mut app, "primary");

        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "child-sess".to_string(),
            response_text: r#"{"action":"complete","summary":"done"}"#.to_string(),
        });

        // Child completion must be accepted: pipeline should produce at least TaskUpdated.
        assert!(
            msgs.iter().any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "child SessionCompleted should produce TaskUpdated when phase is Running, got: {msgs:?}"
        );
        // The workflow should have advanced: session_id cleared by the engine.
        let state = app
            .workflow_engine
            .state(&task_id)
            .expect("state should exist");
        assert!(
            state.session_id.is_none(),
            "engine session_id should be cleared after advancing, got: {:?}",
            state.session_id
        );
    }

    /// Verifies that a second `SessionCompleted` is dropped when the workflow phase
    /// is no longer Running (prevents double-advancement when both a child session
    /// and the primary conductor fire session.idle for the same turn).
    #[test]
    fn test_secondary_completion_ignored_when_phase_not_running() {
        let mut app = App::test_default();
        // Enable the approval gate so the first completion transitions to
        // AwaitingApproval — a non-Running phase — making the guard testable.
        app.workflow_engine.set_approval_gate(true);
        let task_id = make_task_with_active_session(&mut app, "primary");

        // First completion: accepted, transitions phase to AwaitingApproval.
        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "primary".to_string(),
            response_text: r#"{"action":"complete","summary":"done"}"#.to_string(),
        });

        // Phase should now be AwaitingApproval (not Running).
        let phase = app.workflow_engine.state(&task_id).map(|s| s.phase.clone());
        assert!(
            !matches!(phase, Some(WorkflowPhase::Running)),
            "phase should have advanced past Running after first completion, got: {phase:?}"
        );

        // Second completion (e.g. the conductor session firing late): must be dropped.
        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "conductor".to_string(),
            response_text: String::new(),
        });
        assert!(
            msgs.is_empty(),
            "second SessionCompleted should be ignored when phase is not Running, got: {msgs:?}"
        );
    }

    /// Verifies that `SessionError` for a child session (not the primary) is ignored
    /// and does not trigger an error transition.
    #[test]
    fn test_child_session_error_skipped() {
        let mut app = App::test_default();
        let task_id = make_task_with_active_session(&mut app, "primary");

        let msgs = app.handle_message(AppMessage::SessionError {
            task_id: task_id.clone(),
            session_id: "child-sess".to_string(),
            error: "child error".to_string(),
        });

        assert!(
            msgs.is_empty(),
            "child SessionError should produce no messages, got: {msgs:?}"
        );
        // The engine's primary session must still be registered.
        let state = app
            .workflow_engine
            .state(&task_id)
            .expect("state should exist");
        assert_eq!(
            state.session_id.as_deref(),
            Some("primary"),
            "engine should still track the primary session"
        );
    }

    /// Verifies that `SessionError` with an empty session_id passes through when no
    /// session is registered in the engine (handles CreateSession-level failures).
    #[test]
    fn test_session_error_allowed_when_no_session_registered() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);
        // No SessionCreated processed; engine.session_id == None.

        let msgs = app.handle_message(AppMessage::SessionError {
            task_id: task_id.clone(),
            session_id: String::new(),
            error: "OpenCode client unavailable".to_string(),
        });

        // Should emit TaskUpdated (error recorded) and not be silently dropped.
        assert!(
            msgs.iter().any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "SessionError with no registered session should still produce TaskUpdated, got: {msgs:?}"
        );
    }

    /// Verifies that `PermissionResolved` with a non-empty explanation dispatches `SendPrompt`.
    ///
    /// The message is now sent via `async_tx` rather than returned synchronously,
    /// so this test uses a tokio runtime and reads from the channel.
    #[tokio::test]
    async fn test_reject_with_explanation_emits_send_prompt() {
        use crate::opencode::types::PermissionRequest;
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let session_map = Arc::new(RwLock::new(HashMap::new()));
        let mut app = App::new(
            crate::tasks::TaskStore::new(),
            Box::new(crate::backend::NullBackend),
            session_map,
            tx,
            false,
            false,
            HashMap::new(),
            None,
        );
        let task_id = make_task_in_progress(&mut app);

        // Register the session so the workflow phase is Running with a known session_id.
        app.handle_message(AppMessage::SessionCreated {
            task_id: task_id.clone(),
            session_id: "sess-abc".to_string(),
        });

        let request = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-abc".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["rm -rf".to_string()],
            always: vec![],
        };

        app.handle_message(AppMessage::PermissionResolved {
            task_id: task_id.clone(),
            request,
            response: "reject".to_string(),
            explanation: Some(
                "No, let's consider something else first. Try a safer approach.".to_string(),
            ),
        });

        // The SendPrompt message is sent asynchronously via async_tx.
        let msg = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("timed out waiting for SendPrompt")
            .expect("channel closed");
        assert!(
            matches!(msg, AppMessage::SendPrompt { .. }),
            "expected SendPrompt when explanation is provided, got: {msg:?}"
        );
    }

    /// Verifies that `PermissionResolved` without explanation does not emit `SendPrompt`.
    #[test]
    fn test_reject_without_explanation_no_send_prompt() {
        use crate::opencode::types::PermissionRequest;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Register the session so the workflow phase is Running with a known session_id.
        // This ensures the test is non-vacuous: the handler *could* emit SendPrompt,
        // but should not because explanation is None.
        app.handle_message(AppMessage::SessionCreated {
            task_id: task_id.clone(),
            session_id: "sess-abc".to_string(),
        });

        let request = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-abc".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["rm -rf".to_string()],
            always: vec![],
        };

        let msgs = app.handle_message(AppMessage::PermissionResolved {
            task_id: task_id.clone(),
            request,
            response: "reject".to_string(),
            explanation: None,
        });

        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, AppMessage::SendPrompt { .. })),
            "expected no SendPrompt when explanation is None, got: {msgs:?}"
        );
    }

    /// Verifies that `SessionCompleted` with an unparseable response does NOT advance the workflow.
    ///
    /// The workflow should stay in `Running` phase and only `TaskUpdated` is emitted, so the
    /// user can steer the agent via `[p]` without losing the session.
    #[test]
    fn test_parse_failure_stays_in_running() {
        use crate::workflow::transitions::WorkflowPhase;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);
        // make_task_in_progress -> StartTask -> phase=Running, session_id=None.
        // The guard passes (expected_session=None), so the parse happens.

        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-1".to_string(),
            response_text: "this is not valid json at all".to_string(),
        });

        // Should emit TaskUpdated but NOT advance to a new agent session.
        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "expected TaskUpdated after parse failure, got: {msgs:?}"
        );
        assert!(
            !msgs
                .iter()
                .any(|m| matches!(m, AppMessage::CreateSession { .. })),
            "parse failure must NOT advance to next agent, got: {msgs:?}"
        );

        // The workflow phase must still be Running so the user can steer.
        let state = app.workflow_engine.state(&task_id).unwrap();
        assert_eq!(
            state.phase,
            WorkflowPhase::Running,
            "workflow must remain in Running after parse failure"
        );
    }

    /// Verifies that a CodeReview SessionCompleted with a commit_message stores it in Tab4State.
    #[test]
    fn test_commit_message_stored_from_code_review() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Advance pipeline through all agents before CodeReview.
        for agent in [
            AgentKind::Intake,
            AgentKind::Design,
            AgentKind::Planning,
            AgentKind::Implementation,
            AgentKind::CodeQuality,
            AgentKind::SecurityReview,
        ] {
            app.workflow_engine.process(AppMessage::AgentCompleted {
                task_id: task_id.clone(),
                agent,
                summary: "done".to_string(),
            });
        }

        let response_json =
            r#"{"action":"complete","summary":"LGTM","commit_message":"feat: add commit dialog"}"#;
        app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "sess-cr".to_string(),
            response_text: response_json.to_string(),
        });

        assert_eq!(
            app.tab4_state.get_commit_message(&task_id),
            Some("feat: add commit dialog"),
            "commit_message from CodeReview should be stored in Tab4State"
        );
    }

    /// Verifies that `CommitCompleted` transitions the task to Completed.
    #[test]
    fn test_commit_completed_sets_task_completed() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::PendingReview,
            assigned_to: Some(AgentKind::Human),
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

        let msgs = app.handle_message(AppMessage::CommitCompleted {
            task_id: task_id.clone(),
        });

        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.status,
            TaskStatus::Completed,
            "status should be Completed after CommitCompleted"
        );
        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::TaskUpdated { .. })),
            "CommitCompleted should emit TaskUpdated"
        );
    }

    /// Verifies that `CommitFailed` pushes an error banner and leaves the task in PendingReview.
    #[test]
    fn test_commit_failed_keeps_pending_review() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::PendingReview,
            assigned_to: Some(AgentKind::Human),
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

        let msgs = app.handle_message(AppMessage::CommitFailed {
            task_id: task_id.clone(),
            error: "git error".to_string(),
        });

        // Task should remain PendingReview.
        let task = app.task_store.get(&task_id).unwrap();
        assert_eq!(
            task.status,
            TaskStatus::PendingReview,
            "task should remain PendingReview after CommitFailed"
        );
        // No messages emitted.
        assert!(
            msgs.is_empty(),
            "CommitFailed should emit no messages, got: {msgs:?}"
        );
    }

    /// Verifies that `SessionCompleted` for a session in `pending_commit_sessions` is routed
    /// to `CommitCompleted` instead of going through `parse_response`.
    #[test]
    fn test_commit_session_routed_via_pending_commit_sessions() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::PendingReview,
            assigned_to: Some(AgentKind::Human),
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

        // Register a commit session.
        app.pending_commit_sessions
            .insert("commit-sess-1".to_string(), task_id.clone());

        // SessionCompleted for the commit session should route to CommitCompleted.
        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "commit-sess-1".to_string(),
            response_text: "Committed successfully".to_string(),
        });

        assert!(
            msgs.iter()
                .any(|m| matches!(m, AppMessage::CommitCompleted { .. })),
            "SessionCompleted for commit session should emit CommitCompleted, got: {msgs:?}"
        );
        // Session should be removed from pending_commit_sessions.
        assert!(
            !app.pending_commit_sessions.contains_key("commit-sess-1"),
            "commit session should be removed from pending_commit_sessions after routing"
        );
    }

    /// Verifies that `CreateSession` pushes a model banner to the activity log.
    #[test]
    fn test_create_session_includes_model_in_banner() {
        use crate::opencode::types::ModelId;
        use crate::tui::tabs::agent_activity::ActivityLine;

        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Install a known model for the Intake agent.
        let expected_model = ModelId {
            provider_id: "openrouter".to_string(),
            model_id: "anthropic/claude-sonnet-4.6".to_string(),
        };
        app.agent_models
            .insert(AgentKind::Intake, expected_model.clone());

        // Fire CreateSession (no real opencode client; the session spawn will fail silently).
        app.handle_message(AppMessage::CreateSession {
            task_id: task_id.clone(),
            agent: AgentKind::Intake,
            context: None,
            prompt: String::new(),
        });

        // The activity log should contain a banner with the model string.
        let model_str = expected_model.to_string();
        let lines = app.tab2_state.lines_for(&task_id);
        let found = lines.iter().any(|line| {
            if let ActivityLine::AgentBanner { message } = line {
                message.contains(&model_str)
            } else {
                false
            }
        });
        assert!(
            found,
            "Expected model '{}' in activity banners, got: {:?}",
            model_str, lines
        );
    }

    /// Verifies that `RegisterCommitSession` inserts the session into `pending_commit_sessions`.
    #[test]
    fn test_register_commit_session() {
        let mut app = App::test_default();
        let task_id = TaskId::from_path("tasks/1.1.md");

        let msgs = app.handle_message(AppMessage::RegisterCommitSession {
            task_id: task_id.clone(),
            session_id: "commit-sess-42".to_string(),
        });

        assert!(msgs.is_empty(), "RegisterCommitSession emits no messages");
        assert_eq!(
            app.pending_commit_sessions.get("commit-sess-42"),
            Some(&task_id),
            "session should be registered in pending_commit_sessions"
        );
    }

    /// Verifies that `open_commit_dialog` appends the task's markdown file to `file_summary`
    /// so the user can see it will be staged and committed.
    #[test]
    fn test_open_commit_dialog_includes_task_file() {
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task_id = TaskId::from_path("tasks/2.3.md");
        let task = Task {
            id: task_id.clone(),
            story_name: "2. Story".to_string(),
            name: "2.3".to_string(),
            status: TaskStatus::PendingReview,
            assigned_to: None,
            description: "do the thing".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/2.3.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);

        app.open_commit_dialog(&task_id);

        let dialog = app.commit_dialog.expect("commit dialog should be open");
        let paths: Vec<&str> = dialog
            .file_summary
            .iter()
            .map(|(p, _)| p.as_str())
            .collect();
        assert!(
            paths.contains(&"tasks/2.3.md"),
            "task markdown file should appear in file_summary; got: {:?}",
            paths
        );
    }

    /// Verifies that `HumanApprovedCommit` appends the task markdown file to `file_paths`
    /// before delegating to the backend. The NullBackend sends CommitFailed asynchronously,
    /// confirming the synchronous path (including the task-file append) completed without error.
    #[tokio::test]
    async fn test_human_approved_commit_includes_task_file() {
        use crate::tasks::models::{Task, TaskStatus};
        use std::collections::HashMap;
        use std::sync::Arc;
        use tokio::sync::RwLock;

        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let session_map = Arc::new(RwLock::new(HashMap::new()));
        let mut app = App::new(
            crate::tasks::TaskStore::new(),
            Box::new(crate::backend::NullBackend),
            session_map,
            tx,
            false,
            false,
            HashMap::new(),
            None,
        );

        let task_id = TaskId::from_path("tasks/3.1.md");
        let task = Task {
            id: task_id.clone(),
            story_name: "3. Story".to_string(),
            name: "3.1".to_string(),
            status: TaskStatus::PendingReview,
            assigned_to: None,
            description: "another task".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/3.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);

        // Only code files; task file deliberately omitted from the caller's list.
        app.handle_message(AppMessage::HumanApprovedCommit {
            task_id: task_id.clone(),
            commit_message: "Complete task 3.1".to_string(),
            file_paths: vec!["src/lib.rs".to_string()],
        });

        // NullBackend sends CommitFailed asynchronously via async_tx.
        let msg = tokio::time::timeout(std::time::Duration::from_millis(100), rx.recv())
            .await
            .expect("timed out waiting for CommitFailed")
            .expect("channel closed");
        assert!(
            matches!(msg, AppMessage::CommitFailed { .. }),
            "NullBackend should send CommitFailed, got: {msg:?}"
        );
    }

    /// Verifies that a `SessionCompleted` arriving while the workflow is in
    /// `AwaitingApproval` phase (session_id == None) is silently dropped.
    ///
    /// Regression guard for the "double CodeQuality session" bug: after
    /// `AgentCompleted` the engine clears `session_id` and enters `AwaitingApproval`.
    /// A stale or spurious `SessionCompleted` arriving in that window must be dropped
    /// to prevent a second idle session being spawned for the current agent.
    #[test]
    fn test_session_completed_ignored_during_awaiting_approval() {
        let mut app = App::test_default();
        let task_id = make_task_in_progress(&mut app);

        // Drive the workflow into AwaitingApproval via AgentCompleted with gate enabled.
        app.workflow_engine.set_approval_gate(true);
        app.workflow_engine.process(AppMessage::AgentCompleted {
            task_id: task_id.clone(),
            agent: AgentKind::Intake,
            summary: "done".to_string(),
        });

        // Confirm we are now in AwaitingApproval with session_id == None.
        let state = app.workflow_engine.state(&task_id).unwrap();
        assert!(
            matches!(state.phase, WorkflowPhase::AwaitingApproval { .. }),
            "expected AwaitingApproval, got {:?}",
            state.phase
        );
        assert!(
            state.session_id.is_none(),
            "session_id should be None after AgentCompleted with gate"
        );

        // A spurious / stale SessionCompleted must be dropped entirely.
        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: task_id.clone(),
            session_id: "stale-sess".to_string(),
            response_text: "some text without action key".to_string(),
        });

        assert!(
            msgs.is_empty(),
            "SessionCompleted during AwaitingApproval must return no messages, got: {msgs:?}"
        );

        // Phase must remain AwaitingApproval — no idle session spawned, no advance.
        let state = app.workflow_engine.state(&task_id).unwrap();
        assert!(
            matches!(state.phase, WorkflowPhase::AwaitingApproval { .. }),
            "phase must stay AwaitingApproval after dropped completion, got {:?}",
            state.phase
        );
    }

    /// Verifies that `parse_git_status_porcelain` correctly maps porcelain v1 status codes
    /// to `DiffStatus` values and extracts paths (including the new path for renames).
    #[test]
    fn test_git_status_files_parses_porcelain() {
        use crate::opencode::types::DiffStatus;

        let input = "\
M  src/modified.rs
 M src/unstaged_modified.rs
MM src/both_modified.rs
A  src/added.rs
?? src/untracked.rs
D  src/deleted.rs
 D src/unstaged_deleted.rs
R  old_name.rs -> new_name.rs
";
        let result = parse_git_status_porcelain(input);

        let find = |path: &str| {
            result
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, s)| s.clone())
        };

        assert_eq!(find("src/modified.rs"), Some(DiffStatus::Modified));
        assert_eq!(find("src/unstaged_modified.rs"), Some(DiffStatus::Modified));
        assert_eq!(find("src/both_modified.rs"), Some(DiffStatus::Modified));
        assert_eq!(find("src/added.rs"), Some(DiffStatus::Added));
        assert_eq!(find("src/untracked.rs"), Some(DiffStatus::Added));
        assert_eq!(find("src/deleted.rs"), Some(DiffStatus::Deleted));
        assert_eq!(find("src/unstaged_deleted.rs"), Some(DiffStatus::Deleted));
        // Rename: only the new path should appear
        assert_eq!(find("new_name.rs"), Some(DiffStatus::Modified));
        assert_eq!(find("old_name.rs"), None);
    }

    #[test]
    fn test_research_prompt_creates_session_on_first_prompt() {
        let mut app = App::test_default();
        assert!(app.research_state.session_id.is_none());
        assert!(!app.research_state.session_creating);

        let msgs = app.handle_message(AppMessage::ResearchPromptSubmitted {
            prompt: "What is Rust?".to_string(),
        });

        assert!(msgs.is_empty());
        assert!(app.research_state.awaiting_response);
        assert!(app.research_state.session_creating);
        // User message pushed.
        assert_eq!(app.research_state.messages.len(), 2); // user + "Creating session..."
        assert_eq!(
            app.research_state.messages[0].role,
            crate::tui::tabs::research::ChatRole::User
        );
    }

    #[test]
    fn test_research_prompt_reuses_session() {
        let mut app = App::test_default();
        // Simulate a session already existing.
        app.research_state.session_id = Some("sess-abc".to_string());

        let msgs = app.handle_message(AppMessage::ResearchPromptSubmitted {
            prompt: "Follow-up question".to_string(),
        });

        assert!(msgs.is_empty());
        assert!(app.research_state.awaiting_response);
        // No "Creating session..." system banner.
        assert_eq!(app.research_state.messages.len(), 1);
        assert_eq!(
            app.research_state.messages[0].role,
            crate::tui::tabs::research::ChatRole::User
        );
    }

    #[test]
    fn test_streaming_update_routes_to_research() {
        let mut app = App::test_default();
        let research_id = App::research_task_id();

        let msgs = app.handle_message(AppMessage::StreamingUpdate {
            task_id: research_id,
            session_id: "sess-1".to_string(),
            message_id: "msg-1".to_string(),
            parts: vec![crate::opencode::types::MessagePart::Text {
                text: "Hello there".to_string(),
            }],
        });

        assert!(msgs.is_empty());
        assert_eq!(app.research_state.messages.len(), 1);
        assert_eq!(app.research_state.messages[0].content, "Hello there");
    }

    #[test]
    fn test_session_completed_routes_to_research() {
        let mut app = App::test_default();
        // Stored parent session_id differs from the child session_id in the event.
        app.research_state.session_id = Some("sess-parent".to_string());
        app.research_state.awaiting_response = true;

        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: App::research_task_id(),
            session_id: "sess-child".to_string(),
            response_text: "Done".to_string(),
        });

        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], AppMessage::ResearchResponseCompleted));
        // Session ID preserved for follow-up prompts.
        assert_eq!(
            app.research_state.session_id,
            Some("sess-parent".to_string())
        );
    }

    #[test]
    fn test_session_completed_routes_research_child_session() {
        // Proves that task_id routing works when OpenCode uses conductor/child sessions:
        // the child session_id is completely different from the stored parent session_id.
        let mut app = App::test_default();
        app.research_state.session_id = Some("conductor-abc".to_string());
        app.research_state.awaiting_response = true;

        let msgs = app.handle_message(AppMessage::SessionCompleted {
            task_id: App::research_task_id(),
            session_id: "child-xyz".to_string(),
            response_text: "Research result".to_string(),
        });

        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], AppMessage::ResearchResponseCompleted));
    }

    #[test]
    fn test_session_error_clears_research_session() {
        let mut app = App::test_default();
        // Stored parent session_id differs from the child session_id in the event.
        app.research_state.session_id = Some("sess-parent".to_string());
        app.research_state.awaiting_response = true;

        let msgs = app.handle_message(AppMessage::SessionError {
            task_id: App::research_task_id(),
            session_id: "sess-child".to_string(),
            error: "connection lost".to_string(),
        });

        assert_eq!(msgs.len(), 1);
        assert!(matches!(msgs[0], AppMessage::ResearchResponseError { .. }));
        // Session cleared so the next prompt creates a new one.
        assert!(app.research_state.session_id.is_none());
        assert!(!app.research_state.session_creating);
    }

    #[test]
    fn test_research_response_completed_finalizes() {
        let mut app = App::test_default();
        app.research_state.awaiting_response = true;

        app.handle_message(AppMessage::ResearchResponseCompleted);

        assert!(!app.research_state.awaiting_response);
    }

    #[test]
    fn test_research_response_error_pushes_error() {
        let mut app = App::test_default();
        app.research_state.awaiting_response = true;

        app.handle_message(AppMessage::ResearchResponseError {
            error: "timeout".to_string(),
        });

        assert!(!app.research_state.awaiting_response);
        assert_eq!(app.research_state.messages.len(), 1);
        assert_eq!(
            app.research_state.messages[0].role,
            crate::tui::tabs::research::ChatRole::System
        );
    }

    #[test]
    fn test_research_prompt_queued_during_session_creation() {
        let mut app = App::test_default();
        // Simulate a session being created (first prompt already in flight).
        app.research_state.session_creating = true;

        let msgs = app.handle_message(AppMessage::ResearchPromptSubmitted {
            prompt: "second question".to_string(),
        });

        assert!(msgs.is_empty());
        // Prompt was queued, not sent.
        assert_eq!(
            app.research_state.pending_prompt.as_deref(),
            Some("second question")
        );
        // A queued banner was pushed alongside the user message.
        assert_eq!(app.research_state.messages.len(), 2);
        assert_eq!(
            app.research_state.messages[0].role,
            crate::tui::tabs::research::ChatRole::User
        );
        assert_eq!(
            app.research_state.messages[1].role,
            crate::tui::tabs::research::ChatRole::System
        );
        assert!(app.research_state.messages[1].content.contains("Queued"));
    }

    #[test]
    fn test_queued_prompt_sent_on_prompt_sent() {
        let mut app = App::test_default();
        // A prompt was queued while the session was being created.
        app.research_state.session_creating = true;
        app.research_state.pending_prompt = Some("queued".to_string());

        // PromptSent arrives (session_id now known).
        let msgs = app.handle_message(AppMessage::PromptSent {
            task_id: App::research_task_id(),
            session_id: "sess-new".to_string(),
        });

        assert!(msgs.is_empty());
        // session_id stored and session_creating cleared.
        assert_eq!(app.research_state.session_id, Some("sess-new".to_string()));
        assert!(!app.research_state.session_creating);
        // pending_prompt drained.
        assert!(app.research_state.pending_prompt.is_none());
    }

    #[test]
    fn test_tool_activity_shown_in_research_chat() {
        let mut app = App::test_default();
        app.research_state.session_id = Some("sess-1".to_string());

        app.handle_message(AppMessage::ToolActivity {
            task_id: App::research_task_id(),
            session_id: "sess-1".to_string(),
            tool: "read_file".to_string(),
            status: "running".to_string(),
            detail: Some("src/main.rs".to_string()),
        });

        assert_eq!(app.research_state.messages.len(), 1);
        assert_eq!(
            app.research_state.messages[0].role,
            crate::tui::tabs::research::ChatRole::System
        );
        assert!(app.research_state.messages[0].content.contains("read_file"));
        assert!(app.research_state.messages[0]
            .content
            .contains("src/main.rs"));
    }
}
