//! Main layout: header, left pane (25%), right pane (75%), and footer.
//!
//! Splits the terminal area into the primary ClawdMux layout regions using
//! ratatui `Layout` constraints. The left pane holds the task list; the right
//! pane holds the 4-tab widget.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Computed layout regions for the ClawdMux TUI.
pub struct AppLayout {
    /// Three-row header bar at the top of the terminal.
    pub header: Rect,
    /// Left pane occupying 25% of the main area width (task list).
    pub left_pane: Rect,
    /// Right pane occupying 75% of the main area width (tab content).
    pub right_pane: Rect,
    /// Two-row footer bar at the bottom of the terminal.
    pub footer: Rect,
}

/// Splits the given terminal area into the 4-region ClawdMux layout.
///
/// - Header: 3 rows at the top
/// - Main area: split 25% left / 75% right
/// - Footer: 2 rows at the bottom
pub fn render_layout(area: Rect) -> AppLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(area);

    let header = vertical[0];
    let main_area = vertical[1];
    let footer = vertical[2];

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(25), Constraint::Percentage(75)])
        .split(main_area);

    let left_pane = horizontal[0];
    let right_pane = horizontal[1];

    AppLayout {
        header,
        left_pane,
        right_pane,
        footer,
    }
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::*;

    #[test]
    fn test_render_layout_proportions() {
        let area = Rect::new(0, 0, 80, 24);
        let layout = render_layout(area);

        assert_eq!(layout.header.height, 3);
        assert_eq!(layout.footer.height, 2);

        // Left pane should be ~25% of 80 = 20 columns.
        assert_eq!(layout.left_pane.width, 20);
        // Right pane should be ~75% of 80 = 60 columns.
        assert_eq!(layout.right_pane.width, 60);

        // Left + right should span the full terminal width.
        assert_eq!(layout.left_pane.width + layout.right_pane.width, area.width);
    }
}
