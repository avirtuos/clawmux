//! Top-level application state and message dispatcher.
//!
//! The `App` struct holds all runtime state and coordinates between subsystems:
//! the TUI layer, workflow engine, task store, and opencode client.

use crate::messages::AppMessage;
use crate::tasks::{TaskId, TaskStore};
use crate::tui::task_list::TaskListState;

/// Top-level application state.
///
/// Coordinates the TUI, workflow engine, task store, and opencode client
/// via the [`AppMessage`] dispatch loop.
#[allow(dead_code)]
pub struct App {
    /// In-memory store for all loaded stories and tasks.
    pub task_store: TaskStore,
    /// The task currently selected in the left pane, if any.
    pub selected_task: Option<TaskId>,
    /// Index of the active tab in the right pane (0-based).
    pub active_tab: usize,
    /// When `true`, the event loop should exit and the TUI should shut down.
    pub should_quit: bool,
    /// Navigation and expansion state for the left-pane task list widget.
    pub task_list_state: TaskListState,
}

impl App {
    /// Creates a new `App` with the given task store and default UI state.
    ///
    /// Initializes the task list widget by loading stories from the store.
    pub fn new(task_store: TaskStore) -> Self {
        let stories = task_store.stories();
        let mut task_list_state = TaskListState::new();
        task_list_state.refresh(&stories);
        App {
            task_store,
            selected_task: None,
            active_tab: 0,
            should_quit: false,
            task_list_state,
        }
    }

    /// Processes a single [`AppMessage`], mutating state and returning
    /// any follow-up messages to dispatch.
    pub fn handle_message(&mut self, msg: AppMessage) -> Vec<AppMessage> {
        match msg {
            AppMessage::Shutdown => {
                self.should_quit = true;
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
        assert!(app.selected_task.is_none());
        assert_eq!(app.active_tab, 0);
        assert!(!app.should_quit);
        assert_eq!(app.task_list_state.selected_index, 0);
        assert!(app.task_list_state.expanded_stories.is_empty());
    }

    #[test]
    fn test_handle_message_shutdown() {
        let mut app = App::new(TaskStore::new());
        let responses = app.handle_message(AppMessage::Shutdown);
        assert!(app.should_quit);
        assert!(responses.is_empty());
    }

    #[test]
    fn test_handle_message_tick_noop() {
        let mut app = App::new(TaskStore::new());
        let responses = app.handle_message(AppMessage::Tick);
        assert!(!app.should_quit);
        assert_eq!(app.active_tab, 0);
        assert!(app.selected_task.is_none());
        assert!(responses.is_empty());
    }
}
