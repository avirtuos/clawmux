//! Top-level TUI draw and input handling.
//!
//! Coordinates ratatui rendering across the layout, task list widget, and the
//! 4-tab right pane. Dispatches keyboard events to the focused widget.

use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::App;
use crate::messages::AppMessage;

pub mod layout;
pub mod tabs;
pub mod task_list;

/// Draws the full TUI frame with layout and task list widget.
///
/// Renders header, left pane (task list), right pane, and footer using the computed layout regions.
pub fn draw(frame: &mut Frame, app: &App) {
    let areas = layout::render_layout(frame.area());

    let header = Paragraph::new("ClawdMux v0.1.0").block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(header, areas.header);

    task_list::render(
        frame,
        areas.left_pane,
        &app.task_list_state,
        &app.cached_stories,
    );

    let right_pane = Block::default().title("Details").borders(Borders::ALL);
    frame.render_widget(right_pane, areas.right_pane);

    let footer = Paragraph::new("Mode: Normal").block(Block::default().borders(Borders::TOP));
    frame.render_widget(footer, areas.footer);
}

/// Converts a crossterm event into an optional [`AppMessage`], mutating `app` for navigation.
///
/// - `Up` / `k` -> move task list selection up
/// - `Down` / `j` -> move task list selection down
/// - `Enter` / `Space` -> toggle story expansion (no-op if a task is selected)
/// - `Tab` -> cycle `app.active_tab` (0-3)
/// - `q` (no modifiers) -> [`AppMessage::Shutdown`]
/// - `Ctrl-C` -> [`AppMessage::Shutdown`]
/// - Any other key -> `None`
pub fn handle_input(event: Event, app: &mut App) -> Option<AppMessage> {
    if let Event::Key(key) = event {
        match key.code {
            KeyCode::Char('q') if key.modifiers == KeyModifiers::NONE => {
                return Some(AppMessage::Shutdown);
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                return Some(AppMessage::Shutdown);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.task_list_state.move_up();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.task_list_state.move_down();
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if app.task_list_state.selected_task_id().is_none() {
                    let stories = app.cached_stories.clone();
                    app.task_list_state.toggle_story(&stories);
                }
            }
            KeyCode::Tab => {
                app.active_tab = (app.active_tab + 1) % 4;
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState};

    use super::*;

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn test_handle_input_q_quits() {
        let mut app = App::new(crate::tasks::TaskStore::new());
        let event = key_event(KeyCode::Char('q'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_ctrl_c_quits() {
        let mut app = App::new(crate::tasks::TaskStore::new());
        let event = key_event(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_other_key_none() {
        let mut app = App::new(crate::tasks::TaskStore::new());
        let event = key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_input_up_moves() {
        let mut app = App::new(crate::tasks::TaskStore::new());
        // Seed two story headers by directly setting up state.
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        app.task_list_state
            .expanded_stories
            .insert("2. Beta".to_string());
        let stories = vec![
            crate::tasks::Story {
                name: "1. Alpha".to_string(),
                tasks: vec![crate::tasks::models::Task {
                    id: crate::tasks::TaskId::from_path("tasks/1.1.md"),
                    story_name: "1. Alpha".to_string(),
                    name: "1.1".to_string(),
                    status: crate::tasks::TaskStatus::Open,
                    assigned_to: None,
                    description: String::new(),
                    starting_prompt: None,
                    questions: Vec::new(),
                    design: None,
                    implementation_plan: None,
                    work_log: Vec::new(),
                    file_path: std::path::PathBuf::from("tasks/1.1.md"),
                    extra_sections: Vec::new(),
                }],
            },
            crate::tasks::Story {
                name: "2. Beta".to_string(),
                tasks: vec![],
            },
        ];
        app.task_list_state.refresh(&stories);
        // items: [0] Story "1. Alpha", [1] Task "1.1", [2] Story "2. Beta"
        app.task_list_state.selected_index = 1;

        let event = key_event(KeyCode::Up, KeyModifiers::NONE);
        handle_input(event, &mut app);
        assert_eq!(app.task_list_state.selected_index, 0);
        // Now on a story — selected_task should be None.
        assert!(app.selected_task().is_none());
    }

    #[test]
    fn test_handle_input_down_moves() {
        let mut app = App::new(crate::tasks::TaskStore::new());
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        let stories = vec![crate::tasks::Story {
            name: "1. Alpha".to_string(),
            tasks: vec![crate::tasks::models::Task {
                id: crate::tasks::TaskId::from_path("tasks/1.1.md"),
                story_name: "1. Alpha".to_string(),
                name: "1.1".to_string(),
                status: crate::tasks::TaskStatus::Open,
                assigned_to: None,
                description: String::new(),
                starting_prompt: None,
                questions: Vec::new(),
                design: None,
                implementation_plan: None,
                work_log: Vec::new(),
                file_path: std::path::PathBuf::from("tasks/1.1.md"),
                extra_sections: Vec::new(),
            }],
        }];
        app.task_list_state.refresh(&stories);
        // items: [0] Story "1. Alpha", [1] Task "1.1"
        app.task_list_state.selected_index = 0;

        let event = key_event(KeyCode::Down, KeyModifiers::NONE);
        handle_input(event, &mut app);
        assert_eq!(app.task_list_state.selected_index, 1);
        // Now on a task.
        assert_eq!(
            app.selected_task(),
            Some(&crate::tasks::TaskId::from_path("tasks/1.1.md"))
        );
    }

    #[test]
    fn test_handle_input_enter_toggles_story() {
        let mut app = App::new(crate::tasks::TaskStore::new());
        // Insert a task so the store has a story to display.
        app.task_store.insert(crate::tasks::models::Task {
            id: crate::tasks::TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Alpha".to_string(),
            name: "1.1".to_string(),
            status: crate::tasks::TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
        });
        // Refresh cached stories and task list from the store.
        app.refresh_stories();
        // Before toggle: story is collapsed, selected_index = 0.
        assert!(!app.task_list_state.expanded_stories.contains("1. Alpha"));

        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        handle_input(event, &mut app);
        // After pressing Enter on story header, it should be expanded.
        assert!(app.task_list_state.expanded_stories.contains("1. Alpha"));
    }

    #[test]
    fn test_handle_input_tab_cycles() {
        let mut app = App::new(crate::tasks::TaskStore::new());
        assert_eq!(app.active_tab, 0);

        let tab = key_event(KeyCode::Tab, KeyModifiers::NONE);
        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 1);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 2);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 3);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 0);
    }
}
