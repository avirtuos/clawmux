//! Tab 3: Implementation plan display.
//!
//! Renders the selected task's implementation plan as a scrollable markdown paragraph.

use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::tasks::Task;
use crate::tui::markdown::markdown_to_lines;

/// UI state for Tab 3 (Plan).
pub struct PlanTabState {
    /// Vertical scroll offset for the plan paragraph.
    pub scroll: u16,
}

impl PlanTabState {
    /// Creates a new `PlanTabState` with zero scroll.
    pub fn new() -> Self {
        PlanTabState { scroll: 0 }
    }

    /// Scrolls the plan content up by one line (clamped at 0).
    pub fn scroll_up(&mut self) {
        self.scroll = self.scroll.saturating_sub(1);
    }

    /// Scrolls the plan content down by one line.
    pub fn scroll_down(&mut self) {
        self.scroll = self.scroll.saturating_add(1);
    }
}

impl Default for PlanTabState {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders the Plan tab into `area`.
///
/// When no task is selected (`task` is `None`) or the task has no implementation plan,
/// displays a gray placeholder. Otherwise renders the plan as a scrollable paragraph.
pub fn render(frame: &mut Frame, area: Rect, task: Option<&Task>, state: &PlanTabState) {
    let content = task.and_then(|t| t.implementation_plan.as_deref());
    let block = Block::default()
        .title("Implementation Plan")
        .borders(Borders::ALL);
    match content {
        Some(text) => {
            let para = Paragraph::new(markdown_to_lines(text))
                .block(block)
                .wrap(Wrap { trim: false })
                .scroll((state.scroll, 0));
            frame.render_widget(para, area);
        }
        None => {
            let para = Paragraph::new("No implementation plan available yet.")
                .style(Style::default().fg(Color::DarkGray))
                .block(block);
            frame.render_widget(para, area);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::tasks::models::{Task, TaskId, TaskStatus};

    fn make_task(plan: Option<&str>) -> Task {
        Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Test Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: plan.map(|s| s.to_string()),
            work_log: Vec::new(),
            file_path: PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        }
    }

    #[test]
    fn test_plan_render_placeholder() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = PlanTabState::new();

        terminal
            .draw(|frame| {
                render(frame, frame.area(), None, &state);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = buf
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(
            content.contains("No implementation plan available yet."),
            "should show placeholder; got: {content:?}"
        );
    }

    #[test]
    fn test_plan_render_with_content() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let task = make_task(Some("Step 1: do this. Step 2: do that."));
        let state = PlanTabState::new();

        terminal
            .draw(|frame| {
                render(frame, frame.area(), Some(&task), &state);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = buf
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();
        assert!(
            content.contains("Step 1: do this."),
            "should show plan content; got: {content:?}"
        );
    }

    /// Verifies that markdown bold syntax renders with the BOLD style modifier.
    #[test]
    fn test_plan_render_markdown_bold() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let task = make_task(Some("**critical step**"));
        let state = PlanTabState::new();

        terminal
            .draw(|frame| {
                render(frame, frame.area(), Some(&task), &state);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let has_bold = buf.content().iter().any(|cell| {
            cell.style()
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        });
        assert!(
            has_bold,
            "**critical step** should render with BOLD modifier; none found in buffer"
        );
    }
}
