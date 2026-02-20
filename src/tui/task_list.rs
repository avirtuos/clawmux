//! Left pane: story and task tree widget.
//!
//! Renders a collapsible story/task tree in the left pane, showing task status
//! indicators and highlighting the selected item.

use std::collections::{HashMap, HashSet};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
use ratatui::Frame;

use crate::tasks::{Story, Task, TaskId, TaskStatus};

/// A single item in the flattened task list display.
enum TaskListItem {
    /// A story header row.
    Story { name: String },
    /// A task row nested under its story.
    Task { task_id: TaskId },
}

/// Navigation and expansion state for the left-pane task list widget.
pub struct TaskListState {
    /// Set of story names that are currently expanded (showing their tasks).
    pub expanded_stories: HashSet<String>,
    /// Index of the currently highlighted item in the flattened list.
    pub selected_index: usize,
    /// Flattened list of displayable items (stories + visible tasks).
    items: Vec<TaskListItem>,
}

impl TaskListState {
    /// Creates a new empty `TaskListState` with all stories collapsed.
    pub fn new() -> Self {
        TaskListState {
            expanded_stories: HashSet::new(),
            selected_index: 0,
            items: Vec::new(),
        }
    }

    /// Rebuilds the flat item list from the given stories.
    ///
    /// Story headers are always included. Task rows are included only for
    /// stories in `expanded_stories`. Clamps `selected_index` to the new list length.
    pub fn refresh(&mut self, stories: &[Story]) {
        self.items.clear();
        for story in stories {
            self.items.push(TaskListItem::Story {
                name: story.name.clone(),
            });
            if self.expanded_stories.contains(&story.name) {
                for task in story.sorted_tasks() {
                    self.items.push(TaskListItem::Task {
                        task_id: task.id.clone(),
                    });
                }
            }
        }
        let max_index = self.items.len().saturating_sub(1);
        if self.selected_index > max_index {
            self.selected_index = max_index;
        }
    }

    /// Moves the selection up by one row (no wrap; stays at 0 if already at top).
    pub fn move_up(&mut self) {
        if self.selected_index > 0 {
            self.selected_index -= 1;
        }
    }

    /// Moves the selection down by one row (no wrap; stays at last if already at bottom).
    pub fn move_down(&mut self) {
        if self.selected_index + 1 < self.items.len() {
            self.selected_index += 1;
        }
    }

    /// Toggles expansion of the currently selected story and rebuilds the item list.
    ///
    /// If the current item is a `Story`, toggles its name in `expanded_stories` and
    /// calls [`refresh`](TaskListState::refresh) with `stories` to rebuild the flat list.
    /// If the current item is a `Task`, this is a no-op.
    pub fn toggle_story(&mut self, stories: &[Story]) {
        if let Some(TaskListItem::Story { name }) = self.items.get(self.selected_index) {
            let name = name.clone();
            if self.expanded_stories.contains(&name) {
                self.expanded_stories.remove(&name);
            } else {
                self.expanded_stories.insert(name);
            }
        }
        self.refresh(stories);
    }

    /// Returns the `TaskId` of the currently selected task, or `None` if on a story or empty.
    pub fn selected_task_id(&self) -> Option<&TaskId> {
        match self.items.get(self.selected_index) {
            Some(TaskListItem::Task { task_id }) => Some(task_id),
            _ => None,
        }
    }
}

impl Default for TaskListState {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns a colored status icon span for the given task status.
fn status_icon(status: &TaskStatus) -> Span<'static> {
    match status {
        TaskStatus::Open => Span::styled("[ ]", Style::default().fg(Color::DarkGray)),
        TaskStatus::InProgress => Span::styled("[*]", Style::default().fg(Color::Yellow)),
        TaskStatus::Completed => Span::styled("[x]", Style::default().fg(Color::Green)),
        TaskStatus::Abandoned => Span::styled("[!]", Style::default().fg(Color::Red)),
        TaskStatus::PendingReview => Span::styled("[?]", Style::default().fg(Color::Cyan)),
    }
}

/// Renders the collapsible story/task tree into the given frame area.
///
/// Builds a flat [`List`] from the current `state.items`, applying status icons and
/// color highlighting. Uses [`ListState`] for keyboard-driven scroll and selection.
pub fn render(frame: &mut Frame, area: Rect, state: &TaskListState, stories: &[Story]) {
    // Build an O(1) lookup from TaskId -> &Task.
    let task_map: HashMap<&TaskId, &Task> = stories
        .iter()
        .flat_map(|s| s.tasks.iter().map(|t| (&t.id, t)))
        .collect();

    let items: Vec<ListItem> = state
        .items
        .iter()
        .map(|item| match item {
            TaskListItem::Story { name } => {
                let arrow = if state.expanded_stories.contains(name) {
                    "v "
                } else {
                    "> "
                };
                let line = Line::from(vec![
                    Span::styled(arrow, Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled(name.clone(), Style::default().add_modifier(Modifier::BOLD)),
                ]);
                ListItem::new(line)
            }
            TaskListItem::Task { task_id } => {
                let (icon, name_text) = if let Some(task) = task_map.get(task_id) {
                    (status_icon(&task.status), task.name.clone())
                } else {
                    (
                        Span::styled("[?]", Style::default().fg(Color::DarkGray)),
                        task_id.to_string(),
                    )
                };
                let line = Line::from(vec![
                    Span::raw("  "),
                    icon,
                    Span::raw(" "),
                    Span::raw(name_text),
                ]);
                ListItem::new(line)
            }
        })
        .collect();

    let block = Block::default()
        .title("Stories & Tasks")
        .borders(Borders::ALL);

    let list = List::new(items)
        .block(block)
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    let mut list_state = ListState::default();
    if !state.items.is_empty() {
        list_state.select(Some(state.selected_index));
    }

    frame.render_stateful_widget(list, area, &mut list_state);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::tasks::models::{Task, TaskStatus};
    use crate::tasks::Story;

    fn make_task(story_name: &str, task_name: &str, status: TaskStatus) -> Task {
        Task {
            id: TaskId::from_path(format!("tasks/{task_name}.md")),
            story_name: story_name.to_string(),
            name: task_name.to_string(),
            status,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: PathBuf::from(format!("tasks/{task_name}.md")),
            extra_sections: Vec::new(),
        }
    }

    fn make_stories() -> Vec<Story> {
        vec![
            Story {
                name: "1. Alpha".to_string(),
                tasks: vec![
                    make_task("1. Alpha", "1.1", TaskStatus::Open),
                    make_task("1. Alpha", "1.2", TaskStatus::InProgress),
                ],
            },
            Story {
                name: "2. Beta".to_string(),
                tasks: vec![make_task("2. Beta", "2.1", TaskStatus::Completed)],
            },
        ]
    }

    #[test]
    fn test_task_list_state_refresh() {
        let stories = make_stories();
        let mut state = TaskListState::new();

        // Expand both stories before refreshing.
        state.expanded_stories.insert("1. Alpha".to_string());
        state.expanded_stories.insert("2. Beta".to_string());
        state.refresh(&stories);

        // 2 story headers + 2 tasks in Alpha + 1 task in Beta = 5 items.
        assert_eq!(state.items.len(), 5);
        assert!(matches!(&state.items[0], TaskListItem::Story { name } if name == "1. Alpha"));
        assert!(matches!(&state.items[1], TaskListItem::Task { .. }));
        assert!(matches!(&state.items[2], TaskListItem::Task { .. }));
        assert!(matches!(&state.items[3], TaskListItem::Story { name } if name == "2. Beta"));
        assert!(matches!(&state.items[4], TaskListItem::Task { .. }));
    }

    #[test]
    fn test_task_list_collapsed_story() {
        let stories = make_stories();
        let mut state = TaskListState::new();

        // Only expand Alpha; Beta stays collapsed.
        state.expanded_stories.insert("1. Alpha".to_string());
        state.refresh(&stories);

        // 2 story headers + 2 tasks in Alpha = 4 items (Beta's task hidden).
        assert_eq!(state.items.len(), 4);
        assert!(matches!(&state.items[3], TaskListItem::Story { name } if name == "2. Beta"));
    }

    #[test]
    fn test_move_up_down_clamps_at_bounds() {
        let stories = make_stories();
        let mut state = TaskListState::new();
        state.expanded_stories.insert("1. Alpha".to_string());
        state.expanded_stories.insert("2. Beta".to_string());
        state.refresh(&stories);

        // At index 0, move_up should stay at 0.
        state.selected_index = 0;
        state.move_up();
        assert_eq!(state.selected_index, 0);

        // At last index, move_down should stay at last.
        state.selected_index = state.items.len() - 1;
        state.move_down();
        assert_eq!(state.selected_index, state.items.len() - 1);

        // Normal navigation.
        state.selected_index = 2;
        state.move_up();
        assert_eq!(state.selected_index, 1);
        state.move_down();
        assert_eq!(state.selected_index, 2);
    }

    #[test]
    fn test_toggle_story_expands_collapses() {
        let stories = make_stories();
        let mut state = TaskListState::new();
        state.refresh(&stories); // all collapsed: 2 items

        // Select the first story header (index 0 = "1. Alpha").
        state.selected_index = 0;

        // Toggle once to expand — refresh is called internally.
        state.toggle_story(&stories);
        assert!(state.expanded_stories.contains("1. Alpha"));
        // Items now include tasks for Alpha.
        assert_eq!(state.items.len(), 4); // 2 headers + 2 tasks for Alpha

        // Toggle again to collapse.
        state.selected_index = 0;
        state.toggle_story(&stories);
        assert!(!state.expanded_stories.contains("1. Alpha"));
        // Items back to headers only.
        assert_eq!(state.items.len(), 2);
    }

    #[test]
    fn test_selected_task_id() {
        let stories = make_stories();
        let mut state = TaskListState::new();
        state.expanded_stories.insert("1. Alpha".to_string());
        state.refresh(&stories);

        // items: [0] Story "1. Alpha", [1] Task "1.1", [2] Task "1.2", [3] Story "2. Beta"

        // On a story header -> no task id.
        state.selected_index = 0;
        assert!(state.selected_task_id().is_none());

        // On first task.
        state.selected_index = 1;
        let id = state.selected_task_id().unwrap();
        assert_eq!(id, &TaskId::from_path("tasks/1.1.md"));

        // On second task.
        state.selected_index = 2;
        let id = state.selected_task_id().unwrap();
        assert_eq!(id, &TaskId::from_path("tasks/1.2.md"));
    }
}
