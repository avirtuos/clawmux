//! Tab bar and tab dispatch.
//!
//! Renders the 4-tab right pane (Details, Agent Activity, Team Status, Review)
//! and dispatches input events to the currently active tab.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::{symbols, Frame};

use crate::app::App;

pub mod agent_activity;
pub mod code_review;
pub mod task_details;
pub mod team_status;

/// Labels for the four right-pane tabs.
const TAB_TITLES: &[&str] = &["Details", "Agent Activity", "Team Status", "Review"];

/// Renders the tab bar and the currently active tab's content into `area`.
///
/// Splits `area` into a 3-row tab bar at the top and a content area below.
/// Highlights the tab corresponding to `app.active_tab`.
/// Dispatches rendering to the appropriate tab module.
pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let tab_bar_area = layout[0];
    let content_area = layout[1];

    // Build tab bar.
    let titles: Vec<ratatui::text::Line> = TAB_TITLES
        .iter()
        .map(|t| ratatui::text::Line::from(*t))
        .collect();

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(""))
        .select(app.active_tab)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .divider(symbols::line::VERTICAL);

    frame.render_widget(tabs, tab_bar_area);

    // Dispatch to the active tab.
    match app.active_tab {
        0 => {
            let task = app.selected_task().and_then(|id| app.task_store.get(id));
            task_details::render(frame, content_area, task, &app.tab1_state);
        }
        1 => {
            let task_id = app.selected_task();
            agent_activity::render(frame, content_area, task_id, &app.tab2_state);
        }
        _ => {
            let label = TAB_TITLES.get(app.active_tab).copied().unwrap_or("Unknown");
            let placeholder = Paragraph::new(format!("{label}: Not yet implemented"))
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(placeholder, content_area);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::app::App;
    use crate::tasks::TaskStore;

    #[test]
    fn test_tab_bar_renders_four_tabs() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::new(TaskStore::new());

        terminal
            .draw(|frame| {
                render(frame, frame.area(), &app);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = buf
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();

        assert!(
            content.contains("Details"),
            "Buffer should contain 'Details' tab label"
        );
        assert!(
            content.contains("Agent Activity"),
            "Buffer should contain 'Agent Activity' tab label"
        );
        assert!(
            content.contains("Team Status"),
            "Buffer should contain 'Team Status' tab label"
        );
        assert!(
            content.contains("Review"),
            "Buffer should contain 'Review' tab label"
        );
    }
}
