//! Top-level TUI draw and input handling.
//!
//! Coordinates ratatui rendering across the layout, task list widget, and the
//! 4-tab right pane. Dispatches keyboard events to the focused widget.

use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use tui_textarea::Input;

use crate::app::App;
use crate::messages::AppMessage;
use crate::tasks::models::{status_to_index, TaskStatus, ALL_STATUSES};

pub mod layout;
pub mod tabs;
pub mod task_list;

/// Draws a full-screen loading status indicator.
///
/// Shows the app name centered above a status message. Used during
/// startup before the main `App` state is available.
pub fn draw_loading_screen(frame: &mut Frame, status: &str) {
    let area = frame.area();
    let text = Text::from(vec![
        Line::from("ClawdMux v0.1.0").centered(),
        Line::from(""),
        Line::from(status).centered(),
    ]);
    let y = area.y + area.height.saturating_sub(3) / 2;
    let content_area = Rect::new(area.x, y, area.width, 3);
    frame.render_widget(Paragraph::new(text), content_area);
}

/// Syncs all tab states when the selected task changes after navigation.
///
/// Compares the newly selected task against `tab1_state.current_task_id`.
/// If different, calls [`Tab1State::reset_for_task`] to rebuild answer inputs
/// and clear per-task focus state. Also updates the displayed task for Tab 2
/// and Tab 3.
fn sync_tabs_on_nav(app: &mut App) {
    let new_id = app.task_list_state.selected_task_id().cloned();
    if new_id != app.tab1_state.current_task_id {
        match new_id {
            Some(ref id) => {
                if let Some(task) = app.task_store.get(id) {
                    // SAFETY: We clone task to avoid holding an immutable borrow
                    // while mutating tab1_state.
                    let task = task.clone();
                    app.tab1_state.reset_for_task(&task);
                } else {
                    app.tab1_state.current_task_id = new_id;
                }
            }
            None => {
                app.tab1_state.current_task_id = None;
            }
        }
    }
    app.tab2_state
        .set_displayed_task(app.task_list_state.selected_task_id());
    app.tab3_state
        .set_displayed_task(app.task_list_state.selected_task_id());
    app.tab4_state
        .set_displayed_task(app.task_list_state.selected_task_id());
}

/// Returns a context-sensitive keybinding hint string for the footer.
///
/// - When the quit confirmation dialog is visible: shows confirm/cancel bindings.
/// - When the status picker dialog is visible: shows picker navigation bindings.
/// - When a textarea is focused: shows Esc / editing hint.
/// - On Tab 1 with a malformed task selected: shows fix/apply bindings.
/// - On Tab 1 with an OPEN task selected: shows Enter to start + other bindings.
/// - On Tab 1 with no focus: shows all available bindings.
/// - On other tabs: shows minimal bindings.
pub fn footer_hint_text(
    show_quit_confirm: bool,
    show_status_picker: bool,
    active_tab: usize,
    prompt_focused: bool,
    focused_answer: Option<usize>,
    is_malformed_task: bool,
    is_startable_task: bool,
) -> &'static str {
    if show_quit_confirm {
        "[y/Enter] confirm quit | [n/Esc] cancel"
    } else if show_status_picker {
        "[1-5] select | [Up/Down] navigate | [Enter] confirm | [Esc] cancel"
    } else if prompt_focused {
        "[Esc] exit | Editing prompt"
    } else if focused_answer.is_some() {
        "[Esc] exit | [Tab] next answer | Editing answer"
    } else if active_tab == 0 && is_malformed_task {
        "[f] request fix | [Enter] apply fix | [PgUp/PgDn] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 0 && is_startable_task {
        "[Enter] start | [i] prompt | [s] status | [PgUp/PgDn] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 0 {
        "[i] prompt | [a] answer | [s] status | [PgUp/PgDn] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 1 || active_tab == 2 {
        "[PgUp/PgDn] scroll | [Tab] next tab | [q] quit"
    } else {
        "[Tab] next tab | [q] quit"
    }
}

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

    tabs::render(frame, areas.right_pane, app);

    let selected_task = app.selected_task().and_then(|id| app.task_store.get(id));
    let is_malformed_task = selected_task.map(|t| t.is_malformed()).unwrap_or(false);
    let is_startable_task = selected_task
        .map(|t| !t.is_malformed() && t.status == TaskStatus::Open)
        .unwrap_or(false);
    let hint = footer_hint_text(
        app.show_quit_confirm,
        app.show_status_picker.is_some(),
        app.active_tab,
        app.tab1_state.prompt_focused,
        app.tab1_state.focused_answer,
        is_malformed_task,
        is_startable_task,
    );
    let footer = Paragraph::new(hint).block(Block::default().borders(Borders::TOP));
    frame.render_widget(footer, areas.footer);

    if app.show_quit_confirm {
        render_quit_confirm_dialog(frame, frame.area());
    }

    if let Some(selected_idx) = app.show_status_picker {
        let current_status = app
            .selected_task()
            .and_then(|id| app.task_store.get(id))
            .map(|t| t.status.clone())
            .unwrap_or(TaskStatus::Open);
        render_status_picker_dialog(frame, frame.area(), selected_idx, &current_status);
    }
}

/// Renders a centered modal quit confirmation dialog over the given area.
///
/// Blanks a 40x5 centered region using [`Clear`] then draws a bordered paragraph
/// with a yellow border asking the user to confirm or cancel quitting.
fn render_quit_confirm_dialog(frame: &mut Frame, area: Rect) {
    let dialog_width = 40u16;
    let dialog_height = 5u16;
    let x = area.x + area.width.saturating_sub(dialog_width) / 2;
    let y = area.y + area.height.saturating_sub(dialog_height) / 2;
    let dialog_area = Rect::new(
        x,
        y,
        dialog_width.min(area.width),
        dialog_height.min(area.height),
    );

    frame.render_widget(Clear, dialog_area);
    let block = Block::default()
        .title(" Quit ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let paragraph = Paragraph::new("Are you sure you want to quit?")
        .block(block)
        .alignment(ratatui::layout::Alignment::Center);
    frame.render_widget(paragraph, dialog_area);
}

/// Renders a centered modal status picker dialog over the given area.
///
/// Shows all 5 task statuses, with the highlighted entry in yellow bold
/// and the current status marked with `*`.
fn render_status_picker_dialog(
    frame: &mut Frame,
    area: Rect,
    selected_idx: usize,
    current_status: &TaskStatus,
) {
    let dialog_width = 36u16;
    let dialog_height = 10u16;
    let x = area.x + area.width.saturating_sub(dialog_width) / 2;
    let y = area.y + area.height.saturating_sub(dialog_height) / 2;
    let dialog_area = Rect::new(
        x,
        y,
        dialog_width.min(area.width),
        dialog_height.min(area.height),
    );

    frame.render_widget(Clear, dialog_area);

    let current_idx = status_to_index(current_status);
    let lines: Vec<Line> = ALL_STATUSES
        .iter()
        .enumerate()
        .map(|(i, status)| {
            let marker = if i == current_idx { "*" } else { " " };
            let text = format!("{}{}.  {}", marker, i + 1, status);
            if i == selected_idx {
                Line::from(text).style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Line::from(text)
            }
        })
        .collect();

    let block = Block::default()
        .title(" Status ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, dialog_area);
}

/// Applies a status change by index, mutating the selected task in the store.
///
/// Returns `Some(TaskUpdated)` to trigger persistence, or `None` if the index is
/// out of range or no task is selected.
fn apply_status_change(app: &mut App, status_idx: usize) -> Option<AppMessage> {
    let new_status = ALL_STATUSES.get(status_idx)?.clone();
    let task_id = app.selected_task()?.clone();
    if let Some(task) = app.task_store.get_mut(&task_id) {
        task.status = new_status;
    }
    Some(AppMessage::TaskUpdated { task_id })
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
        // When the quit confirmation dialog is visible, intercept all input.
        if app.show_quit_confirm {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    return Some(AppMessage::Shutdown);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    app.dismiss_quit_confirm();
                    return None;
                }
                _ => return None,
            }
        }

        // When the status picker is open, intercept all input.
        if let Some(selected_idx) = app.show_status_picker {
            match key.code {
                KeyCode::Esc => {
                    app.dismiss_status_picker();
                    return None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.show_status_picker = Some(selected_idx.saturating_sub(1));
                    return None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.show_status_picker = Some((selected_idx + 1).min(4));
                    return None;
                }
                KeyCode::Char(c @ '1'..='5') => {
                    let idx = (c as usize) - ('1' as usize);
                    app.show_status_picker = None;
                    return apply_status_change(app, idx);
                }
                KeyCode::Enter => {
                    app.show_status_picker = None;
                    return apply_status_change(app, selected_idx);
                }
                _ => return None, // swallow all other keys
            }
        }

        // When Tab 1 is active and a textarea has focus, forward input to it.
        if app.active_tab == 0 {
            // Check if a malformed task is selected.
            let selected_malformed_task_id = app
                .selected_task()
                .and_then(|id| app.task_store.get(id))
                .filter(|t| t.is_malformed())
                .map(|t| t.id.clone());

            if let Some(task_id) = selected_malformed_task_id {
                match key.code {
                    KeyCode::Char('f') if key.modifiers == KeyModifiers::NONE => {
                        return Some(AppMessage::RequestTaskFix { task_id });
                    }
                    KeyCode::Enter => {
                        // Only emit ApplyTaskFix if there is a suggestion ready.
                        let has_fix = app
                            .task_store
                            .get(&task_id)
                            .and_then(|t| t.parse_error.as_ref())
                            .map(|e| e.suggested_fix.is_some())
                            .unwrap_or(false);
                        if has_fix {
                            return Some(AppMessage::ApplyTaskFix { task_id });
                        }
                    }
                    KeyCode::PageUp => {
                        app.tab1_state.scroll_desc_up();
                        return None;
                    }
                    KeyCode::PageDown => {
                        app.tab1_state.scroll_desc_down();
                        return None;
                    }
                    _ => {}
                }
                // Fall through to shared navigation handling below.
            } else {
                if app.tab1_state.prompt_focused {
                    if key.code == KeyCode::Esc {
                        app.tab1_state.prompt_focused = false;
                        app.tab1_state.set_prompt_unfocused_style();
                    } else {
                        app.tab1_state.prompt_input.input(Input::from(key));
                    }
                    return None;
                }
                if let Some(idx) = app.tab1_state.focused_answer {
                    if key.code == KeyCode::Esc {
                        app.tab1_state.set_answer_unfocused_style(idx);
                        app.tab1_state.focused_answer = None;
                    } else if key.code == KeyCode::Tab && key.modifiers == KeyModifiers::NONE {
                        let len = app.tab1_state.answer_inputs.len();
                        if len > 0 {
                            let new_idx = (idx + 1) % len;
                            app.tab1_state.set_answer_unfocused_style(idx);
                            app.tab1_state.focused_answer = Some(new_idx);
                            app.tab1_state.set_answer_focused_style(new_idx);
                        }
                    } else if let Some(ta) = app.tab1_state.answer_inputs.get_mut(idx) {
                        ta.input(Input::from(key));
                    }
                    return None;
                }
                // Enter focus on the supplemental prompt with 'i'.
                if key.code == KeyCode::Char('i') && key.modifiers == KeyModifiers::NONE {
                    app.tab1_state.prompt_focused = true;
                    app.tab1_state.set_prompt_focused_style();
                    return None;
                }
                // Open the status picker with 's' when a task is selected.
                if key.code == KeyCode::Char('s')
                    && key.modifiers == KeyModifiers::NONE
                    && app.selected_task().is_some()
                {
                    app.open_status_picker();
                    return None;
                }
                // Start an OPEN task with Enter.
                if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE {
                    if let Some(task_id) = app.selected_task().cloned() {
                        let is_open = app
                            .task_store
                            .get(&task_id)
                            .map(|t| t.status == TaskStatus::Open)
                            .unwrap_or(false);
                        if is_open {
                            return Some(AppMessage::StartTask { task_id });
                        }
                    }
                }
                // Scroll the description paragraph with PgUp/PgDn (no textarea focused).
                match key.code {
                    KeyCode::PageUp => {
                        app.tab1_state.scroll_desc_up();
                        return None;
                    }
                    KeyCode::PageDown => {
                        app.tab1_state.scroll_desc_down();
                        return None;
                    }
                    _ => {}
                }
            }
        }

        if app.active_tab == 1 {
            match key.code {
                KeyCode::PageUp => {
                    app.tab2_state.scroll_up();
                    return None;
                }
                KeyCode::PageDown => {
                    app.tab2_state.scroll_down();
                    return None;
                }
                _ => {}
            }
        }

        if app.active_tab == 2 {
            match key.code {
                KeyCode::PageUp => {
                    app.tab3_state.scroll_up();
                    return None;
                }
                KeyCode::PageDown => {
                    let max = app
                        .selected_task()
                        .and_then(|id| app.task_store.get(id))
                        .map_or(0, |t| t.work_log.len());
                    app.tab3_state.scroll_down(max);
                    return None;
                }
                _ => {}
            }
        }

        // Enter focus on the first answer textarea with 'a' (Tab 1, no focus, task selected).
        if app.active_tab == 0
            && app.tab1_state.focused_answer.is_none()
            && !app.tab1_state.prompt_focused
            && key.code == KeyCode::Char('a')
            && key.modifiers == KeyModifiers::NONE
            && !app.tab1_state.answer_inputs.is_empty()
        {
            app.tab1_state.focused_answer = Some(0);
            app.tab1_state.set_answer_focused_style(0);
            return None;
        }

        match key.code {
            KeyCode::Char('q') if key.modifiers == KeyModifiers::NONE => {
                return Some(AppMessage::Shutdown);
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                return Some(AppMessage::Shutdown);
            }
            KeyCode::Up | KeyCode::Char('k') => {
                app.task_list_state.move_up();
                sync_tabs_on_nav(app);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                app.task_list_state.move_down();
                sync_tabs_on_nav(app);
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
        let mut app = App::test_default();
        let event = key_event(KeyCode::Char('q'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_ctrl_c_quits() {
        let mut app = App::test_default();
        let event = key_event(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_other_key_none() {
        let mut app = App::test_default();
        let event = key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_input_up_moves() {
        let mut app = App::test_default();
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
                    parse_error: None,
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
        let mut app = App::test_default();
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
                parse_error: None,
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
        let mut app = App::test_default();
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
            parse_error: None,
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
        let mut app = App::test_default();
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

    /// Builds a minimal App with one story containing one task that has one unanswered question.
    fn app_with_task_and_question() -> App {
        use crate::tasks::models::{Question, Task, TaskId, TaskStatus};
        use crate::workflow::agents::AgentKind;

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Alpha".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: vec![Question {
                agent: AgentKind::Intake,
                text: "Clarify scope?".to_string(),
                answer: None,
            }],
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);
        app.refresh_stories();
        // Expand the story and navigate to the task row.
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // items: [0] Story "1. Alpha", [1] Task "1.1" — navigate down to select the task.
        let down = key_event(KeyCode::Down, KeyModifiers::NONE);
        handle_input(down, &mut app);
        app
    }

    #[test]
    fn test_handle_input_i_focuses_prompt() {
        let mut app = App::test_default();
        assert_eq!(app.active_tab, 0);
        assert!(!app.tab1_state.prompt_focused);

        let event = key_event(KeyCode::Char('i'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(app.tab1_state.prompt_focused);
    }

    #[test]
    fn test_handle_input_a_focuses_answer() {
        let mut app = app_with_task_and_question();
        assert_eq!(app.active_tab, 0);
        // After navigating to the task, answer_inputs should be populated.
        assert_eq!(app.tab1_state.answer_inputs.len(), 1);
        assert!(app.tab1_state.focused_answer.is_none());

        let event = key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert_eq!(app.tab1_state.focused_answer, Some(0));
    }

    #[test]
    fn test_handle_input_pgdn_scrolls_description() {
        let mut app = App::test_default();
        assert_eq!(app.active_tab, 0);
        assert_eq!(app.tab1_state.desc_scroll, 0);

        let event = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert_eq!(app.tab1_state.desc_scroll, 1);
    }

    #[test]
    fn test_handle_input_pgup_scrolls_description() {
        let mut app = App::test_default();
        app.tab1_state.desc_scroll = 2;

        let event = key_event(KeyCode::PageUp, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert_eq!(app.tab1_state.desc_scroll, 1);
    }

    #[test]
    fn test_handle_input_pgdn_no_scroll_when_prompt_focused() {
        let mut app = App::test_default();
        app.tab1_state.prompt_focused = true;

        let event = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        handle_input(event, &mut app);

        // PgDn was forwarded to the textarea, not the scroll handler.
        assert_eq!(app.tab1_state.desc_scroll, 0);
    }

    #[test]
    fn test_handle_input_pgdn_no_scroll_on_other_tab() {
        let mut app = App::test_default();
        app.active_tab = 1;

        let event = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        handle_input(event, &mut app);

        assert_eq!(app.tab1_state.desc_scroll, 0);
    }

    #[test]
    fn test_footer_hints_normal_tab1() {
        let text = footer_hint_text(false, false, 0, false, None, false, false);
        assert!(text.contains("[i] prompt"), "got: {text}");
        assert!(text.contains("[a] answer"), "got: {text}");
        assert!(text.contains("[s] status"), "got: {text}");
        assert!(text.contains("PgUp/PgDn"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_prompt_focused() {
        let text = footer_hint_text(false, false, 0, true, None, false, false);
        assert!(text.contains("[Esc]"), "got: {text}");
        assert!(text.contains("Editing prompt"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_other_tab() {
        let text = footer_hint_text(false, false, 1, false, None, false, false);
        assert!(text.contains("[Tab] next tab"), "got: {text}");
        assert!(text.contains("[q] quit"), "got: {text}");
        assert!(!text.contains("[i]"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_quit_confirm() {
        let text = footer_hint_text(true, false, 0, false, None, false, false);
        assert!(text.contains("[y/Enter]"), "got: {text}");
        assert!(text.contains("[n/Esc]"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_malformed_task() {
        let text = footer_hint_text(false, false, 0, false, None, true, false);
        assert!(text.contains("[f] request fix"), "got: {text}");
        assert!(text.contains("[Enter] apply fix"), "got: {text}");
        assert!(text.contains("PgUp/PgDn"), "got: {text}");
        // Normal task hints should not appear for malformed tasks.
        assert!(!text.contains("[i] prompt"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_status_picker() {
        let text = footer_hint_text(false, true, 0, false, None, false, false);
        assert!(text.contains("[1-5] select"), "got: {text}");
        assert!(text.contains("[Up/Down] navigate"), "got: {text}");
        assert!(text.contains("[Enter] confirm"), "got: {text}");
        assert!(text.contains("[Esc] cancel"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_startable_task() {
        let text = footer_hint_text(false, false, 0, false, None, false, true);
        assert!(text.contains("[Enter] start"), "got: {text}");
        assert!(text.contains("[s] status"), "got: {text}");
        assert!(text.contains("PgUp/PgDn"), "got: {text}");
        // Should not show [a] answer when task is startable (it's OPEN, no questions yet).
        assert!(!text.contains("[a] answer"), "got: {text}");
    }

    #[test]
    fn test_handle_input_quit_confirm_y_confirms() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Char('y'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
        assert!(app.show_quit_confirm); // still true; app.rs handler will set should_quit
    }

    #[test]
    fn test_handle_input_quit_confirm_enter_confirms() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_quit_confirm_n_cancels() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Char('n'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(result.is_none());
        assert!(!app.show_quit_confirm);
    }

    #[test]
    fn test_handle_input_quit_confirm_esc_cancels() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Esc, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(result.is_none());
        assert!(!app.show_quit_confirm);
    }

    #[test]
    fn test_handle_input_quit_confirm_other_keys_ignored() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Char('x'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(result.is_none());
        assert!(app.show_quit_confirm); // dialog remains
    }

    /// Builds an App with a malformed task selected in Tab 0.
    fn app_with_malformed_task() -> App {
        use crate::tasks::models::{ParseErrorInfo, Task, TaskId, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Alpha".to_string(),
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
        };
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // Navigate down to select the task row.
        let down = key_event(KeyCode::Down, KeyModifiers::NONE);
        handle_input(down, &mut app);
        app
    }

    #[test]
    fn test_handle_input_f_on_malformed() {
        let mut app = app_with_malformed_task();
        let event = key_event(KeyCode::Char('f'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            matches!(result, Some(AppMessage::RequestTaskFix { .. })),
            "expected RequestTaskFix, got: {result:?}"
        );
    }

    #[test]
    fn test_handle_input_enter_on_malformed_without_fix() {
        // No suggested fix -- Enter should not emit ApplyTaskFix.
        let mut app = app_with_malformed_task();
        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            result.is_none(),
            "Enter without a fix should return None, got: {result:?}"
        );
    }

    #[test]
    fn test_handle_input_enter_on_malformed_with_fix() {
        use crate::tasks::models::{SuggestedFix, TaskId};

        let mut app = app_with_malformed_task();
        // Add a suggested fix to the task.
        let id = TaskId::from_path("tasks/1.1.md");
        if let Some(task) = app.task_store.get_mut(&id) {
            if let Some(ref mut err_info) = task.parse_error {
                err_info.suggested_fix = Some(SuggestedFix {
                    corrected_content: "fixed content".to_string(),
                    explanation: "Added Status line".to_string(),
                });
            }
        }
        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            matches!(result, Some(AppMessage::ApplyTaskFix { .. })),
            "expected ApplyTaskFix, got: {result:?}"
        );
    }

    /// Builds an App with a normal (non-malformed) task selected on Tab 0.
    fn app_with_normal_task() -> App {
        use crate::tasks::models::{Task, TaskId, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Alpha".to_string(),
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
            parse_error: None,
        };
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // Navigate down to select the task row.
        let down = key_event(KeyCode::Down, KeyModifiers::NONE);
        handle_input(down, &mut app);
        app
    }

    #[test]
    fn test_handle_input_s_opens_status_picker() {
        let mut app = app_with_normal_task();
        assert!(app.show_status_picker.is_none());

        let event = key_event(KeyCode::Char('s'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        // Open status index 0 (OPEN) should be pre-selected.
        assert_eq!(app.show_status_picker, Some(0));
    }

    #[test]
    fn test_handle_input_s_ignored_no_task() {
        // No task selected (on a story row) -- 's' should not open the picker.
        let mut app = App::test_default();
        assert!(app.selected_task().is_none());

        let event = key_event(KeyCode::Char('s'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(app.show_status_picker.is_none());
    }

    #[test]
    fn test_handle_input_s_ignored_prompt_focused() {
        // When the prompt textarea is focused, 's' goes to the textarea, not the picker.
        let mut app = app_with_normal_task();
        app.tab1_state.prompt_focused = true;
        app.tab1_state.set_prompt_focused_style();

        let event = key_event(KeyCode::Char('s'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(app.show_status_picker.is_none());
    }

    #[test]
    fn test_status_picker_esc_dismisses() {
        let mut app = app_with_normal_task();
        app.show_status_picker = Some(2);

        let event = key_event(KeyCode::Esc, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(app.show_status_picker.is_none());
    }

    #[test]
    fn test_status_picker_up_down_navigation() {
        let mut app = app_with_normal_task();
        app.show_status_picker = Some(2);

        // Up moves to 1.
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(1));

        // Up again to 0.
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(0));

        // Clamped at 0.
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(0));

        // Down to 1.
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(1));

        // Down three more times to reach 4 (Abandoned).
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(4));

        // Clamped at 4.
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(4));
    }

    #[test]
    fn test_status_picker_number_key_selects() {
        let mut app = app_with_normal_task();
        app.show_status_picker = Some(0);

        // Press '3' to select PendingReview (index 2).
        let result = handle_input(key_event(KeyCode::Char('3'), KeyModifiers::NONE), &mut app);

        assert!(app.show_status_picker.is_none(), "picker should close");
        assert!(
            matches!(result, Some(AppMessage::TaskUpdated { .. })),
            "expected TaskUpdated, got: {result:?}"
        );
    }

    #[test]
    fn test_status_picker_enter_confirms() {
        let mut app = app_with_normal_task();
        // Pre-select index 3 (Completed).
        app.show_status_picker = Some(3);

        let result = handle_input(key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);

        assert!(app.show_status_picker.is_none(), "picker should close");
        assert!(
            matches!(result, Some(AppMessage::TaskUpdated { .. })),
            "expected TaskUpdated, got: {result:?}"
        );
    }

    #[test]
    fn test_status_picker_swallows_other_keys() {
        let mut app = app_with_normal_task();
        app.show_status_picker = Some(1);

        // 'q' should NOT quit when picker is open.
        let result = handle_input(key_event(KeyCode::Char('q'), KeyModifiers::NONE), &mut app);
        assert!(result.is_none(), "expected None, got: {result:?}");
        assert_eq!(app.show_status_picker, Some(1), "picker should remain open");

        // Tab should be swallowed.
        let result = handle_input(key_event(KeyCode::Tab, KeyModifiers::NONE), &mut app);
        assert!(result.is_none());
        assert_eq!(app.show_status_picker, Some(1));
    }

    #[test]
    fn test_status_picker_changes_task_status() {
        use crate::tasks::models::{TaskId, TaskStatus};

        let mut app = app_with_normal_task();
        // Task starts as Open (index 0).
        let id = TaskId::from_path("tasks/1.1.md");
        assert_eq!(app.task_store.get(&id).unwrap().status, TaskStatus::Open);

        // Open picker, navigate to Completed (index 3), press Enter.
        app.show_status_picker = Some(3);
        handle_input(key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);

        assert_eq!(
            app.task_store.get(&id).unwrap().status,
            TaskStatus::Completed,
            "task status should be Completed after picker selection"
        );
    }

    #[test]
    fn test_handle_input_enter_starts_open_task() {
        // An OPEN non-malformed task: Enter should emit StartTask.
        let mut app = app_with_normal_task();

        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            matches!(result, Some(AppMessage::StartTask { .. })),
            "expected StartTask for OPEN task, got: {result:?}"
        );
    }

    #[test]
    fn test_handle_input_enter_no_start_non_open_task() {
        use crate::tasks::models::{TaskId, TaskStatus};

        // Change the task status to InProgress; Enter should not start it.
        let mut app = app_with_normal_task();
        let id = TaskId::from_path("tasks/1.1.md");
        if let Some(task) = app.task_store.get_mut(&id) {
            task.status = TaskStatus::InProgress;
        }

        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            result.is_none(),
            "Enter on a non-OPEN task should return None, got: {result:?}"
        );
    }
}
