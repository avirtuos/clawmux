//! Main layout: left pane (25%), right pane (75%), and footer.
//!
//! Splits the terminal area into the primary ClawdMux layout regions using
//! ratatui `Layout` constraints. The left pane holds the task list; the right
//! pane holds the 4-tab widget.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Computed layout regions for the ClawdMux TUI.
pub struct AppLayout {
    /// Left pane occupying 25% of the main area width (task list).
    pub left_pane: Rect,
    /// Right pane occupying 75% of the main area width (tab content).
    pub right_pane: Rect,
    /// Two-row footer bar at the bottom of the terminal.
    pub footer: Rect,
}

/// Splits the given terminal area into the 3-region ClawdMux layout.
///
/// - Main area: split 25% left / 75% right
/// - Footer: 2 rows at the bottom
pub fn render_layout(area: Rect) -> AppLayout {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(2)])
        .split(area);

    let main_area = vertical[0];
    let footer = vertical[1];

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
        .split(main_area);

    let left_pane = horizontal[0];
    let right_pane = horizontal[1];

    AppLayout {
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

        assert_eq!(layout.footer.height, 2);

        // Left pane should be ~20% of 80 = 16 columns.
        assert_eq!(layout.left_pane.width, 16);
        // Right pane should be ~80% of 80 = 64 columns.
        assert_eq!(layout.right_pane.width, 64);

        // Left + right should span the full terminal width.
        assert_eq!(layout.left_pane.width + layout.right_pane.width, area.width);
    }
}
