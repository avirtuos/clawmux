//! Top-level application state and message dispatcher.
//!
//! The `App` struct holds all runtime state and coordinates between subsystems:
//! the TUI layer, workflow engine, task store, and opencode client.

use crate::messages::AppMessage;
use crate::tasks::models::SuggestedFix;
use crate::tasks::{Story, TaskId, TaskStore};
use crate::tui::tabs::agent_activity::Tab2State;
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
}

impl App {
    /// Creates a new `App` with the given task store and default UI state.
    ///
    /// Initializes the task list widget by loading stories from the store.
    pub fn new(task_store: TaskStore) -> Self {
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
        }
    }

    /// Refreshes [`cached_stories`](App::cached_stories) from the task store and rebuilds
    /// the task list widget state.
    ///
    /// Call this after any mutation to `task_store` to keep the TUI in sync.
    #[allow(dead_code)]
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
            AppMessage::Tick => vec![],
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
            other => {
                tracing::debug!(?other, "unhandled message");
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
        let app = App::new(TaskStore::new());
        assert!(app.selected_task().is_none());
        assert_eq!(app.active_tab, 0);
        assert!(!app.should_quit);
        assert!(!app.show_quit_confirm);
        assert_eq!(app.task_list_state.selected_index, 0);
        assert!(app.task_list_state.expanded_stories.is_empty());
    }

    #[test]
    fn test_handle_message_shutdown() {
        let mut app = App::new(TaskStore::new());
        let responses = app.handle_message(AppMessage::Shutdown);
        // First Shutdown shows the dialog, does not quit.
        assert!(!app.should_quit);
        assert!(app.show_quit_confirm);
        assert!(responses.is_empty());
    }

    #[test]
    fn test_handle_message_shutdown_confirm() {
        let mut app = App::new(TaskStore::new());
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
        let mut app = App::new(TaskStore::new());
        app.show_quit_confirm = true;
        app.dismiss_quit_confirm();
        assert!(!app.show_quit_confirm);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_handle_message_tick_noop() {
        let mut app = App::new(TaskStore::new());
        let responses = app.handle_message(AppMessage::Tick);
        assert!(!app.should_quit);
        assert_eq!(app.active_tab, 0);
        assert!(app.selected_task().is_none());
        assert!(responses.is_empty());
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

        let mut app = App::new(TaskStore::new());
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

        let mut app = App::new(TaskStore::new());
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
        let mut app = App::new(TaskStore::new());
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
        let mut app = App::new(TaskStore::new());
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
