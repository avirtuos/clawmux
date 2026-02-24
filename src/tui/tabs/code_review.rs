//! Tab 4: unified diff view and comment input.
//!
//! Renders file diffs retrieved from the opencode `/session/:id/diff` endpoint
//! with syntax-highlighted hunks, and provides a `tui-textarea` comment input
//! area for human review feedback.
//! Task 3.6 implements the full code review tab.

use std::collections::HashMap;

use crate::opencode::types::FileDiff;
use crate::tasks::models::TaskId;

/// UI state for Tab 4 (Code Review): per-task diff storage and current display task.
///
/// Stores diffs indexed by task ID so they survive tab switches.
/// The full rendering and comment input are deferred to Task 3.6.
#[allow(dead_code)]
pub struct Tab4State {
    /// Per-task list of file diffs fetched from the opencode diff endpoint.
    diffs: HashMap<TaskId, Vec<FileDiff>>,
    /// The task whose diffs are currently displayed, if any.
    current_task_id: Option<TaskId>,
}

#[allow(dead_code)]
impl Tab4State {
    /// Creates a new `Tab4State` with empty diff storage.
    pub fn new() -> Self {
        Self {
            diffs: HashMap::new(),
            current_task_id: None,
        }
    }

    /// Stores or replaces the diffs for the given task.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The task whose diffs are being stored.
    /// * `diffs` - The file diffs to store.
    pub fn set_diffs(&mut self, task_id: &TaskId, diffs: Vec<FileDiff>) {
        self.diffs.insert(task_id.clone(), diffs);
    }

    /// Returns the diffs for the given task, or an empty slice if none are stored.
    pub fn diffs_for(&self, task_id: &TaskId) -> &[FileDiff] {
        self.diffs.get(task_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Sets the task whose diffs should be displayed.
    ///
    /// # Arguments
    ///
    /// * `task_id` - The task to display, or `None` to clear the display.
    pub fn set_displayed_task(&mut self, task_id: Option<&TaskId>) {
        self.current_task_id = task_id.cloned();
    }
}

impl Default for Tab4State {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_set_and_get_diffs() {
        let mut state = Tab4State::new();
        let task_id = TaskId::from_path("tasks/1.1.md");
        assert!(state.diffs_for(&task_id).is_empty());

        state.set_diffs(&task_id, vec![]);
        assert!(state.diffs_for(&task_id).is_empty());
    }

    #[test]
    fn test_set_displayed_task() {
        let mut state = Tab4State::new();
        assert!(state.current_task_id.is_none());

        let task_id = TaskId::from_path("tasks/1.1.md");
        state.set_displayed_task(Some(&task_id));
        assert_eq!(state.current_task_id, Some(task_id.clone()));

        state.set_displayed_task(None);
        assert!(state.current_task_id.is_none());
    }
}
