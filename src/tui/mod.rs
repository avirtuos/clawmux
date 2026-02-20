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

/// Draws the full TUI frame with placeholder content.
///
/// Renders header, left pane, right pane, and footer using the computed layout regions.
pub fn draw(frame: &mut Frame, _app: &App) {
    let areas = layout::render_layout(frame.area());

    let header = Paragraph::new("ClawdMux v0.1.0").block(Block::default().borders(Borders::BOTTOM));
    frame.render_widget(header, areas.header);

    let left_pane = Block::default()
        .title("Stories & Tasks")
        .borders(Borders::ALL);
    frame.render_widget(left_pane, areas.left_pane);

    let right_pane = Block::default().title("Details").borders(Borders::ALL);
    frame.render_widget(right_pane, areas.right_pane);

    let footer = Paragraph::new("Mode: Normal").block(Block::default().borders(Borders::TOP));
    frame.render_widget(footer, areas.footer);
}

/// Converts a crossterm event into an optional [`AppMessage`].
///
/// - `q` (no modifiers) -> [`AppMessage::Shutdown`]
/// - `Ctrl-C` -> [`AppMessage::Shutdown`]
/// - Any other key -> `None`
pub fn handle_input(event: Event, _app: &App) -> Option<AppMessage> {
    if let Event::Key(key) = event {
        match key.code {
            KeyCode::Char('q') if key.modifiers == KeyModifiers::NONE => Some(AppMessage::Shutdown),
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                Some(AppMessage::Shutdown)
            }
            _ => None,
        }
    } else {
        None
    }
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
        let app = App::new(crate::tasks::TaskStore::new());
        let event = key_event(KeyCode::Char('q'), KeyModifiers::NONE);
        let result = handle_input(event, &app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_ctrl_c_quits() {
        let app = App::new(crate::tasks::TaskStore::new());
        let event = key_event(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = handle_input(event, &app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_other_key_none() {
        let app = App::new(crate::tasks::TaskStore::new());
        let event = key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        let result = handle_input(event, &app);
        assert!(result.is_none());
    }
}
