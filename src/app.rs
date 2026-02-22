//! Top-level application state and message dispatcher.
//!
//! The `App` struct holds all runtime state and coordinates between subsystems:
//! the TUI layer, workflow engine, task store, and opencode client.

use crate::messages::AppMessage;
use crate::tasks::{Story, TaskId, TaskStore};
use crate::tui::tabs::task_details::Tab1State;
use crate::tui::task_list::TaskListState;

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
}
