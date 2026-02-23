//! Tab 3: agent pipeline visualization and work log.
//!
//! Renders a 7-stage pipeline progress indicator showing the current agent and
//! completed stages, plus a scrollable work log of timestamped agent actions.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::tasks::models::{Task, TaskId};
use crate::workflow::agents::AgentKind;
use crate::workflow::transitions::{WorkflowPhase, WorkflowState};

/// UI state for Tab 3 (Team Status).
///
/// Tracks scroll position for the work log and the currently displayed task.
pub struct Tab3State {
    /// Vertical scroll offset for the work log.
    scroll_offset: u16,
    /// The task currently displayed.
    current_task_id: Option<TaskId>,
}

impl Tab3State {
    /// Creates a new `Tab3State` with zero scroll and no task selected.
    pub fn new() -> Self {
        Tab3State {
            scroll_offset: 0,
            current_task_id: None,
        }
    }

    /// Scrolls the work log up by one line (saturates at 0).
    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// Scrolls the work log down by one line, clamped to `max_lines - 1`.
    ///
    /// `max_lines` should be the number of entries in the current task's work log.
    /// Takes `max_lines` as a parameter because `Tab3State` does not own the work log.
    pub fn scroll_down(&mut self, max_lines: usize) {
        let max = max_lines.saturating_sub(1).min(u16::MAX as usize) as u16;
        if self.scroll_offset < max {
            self.scroll_offset += 1;
        }
    }

    /// Updates the currently displayed task, resetting scroll to 0 on change.
    pub fn set_displayed_task(&mut self, task_id: Option<&TaskId>) {
        let new_id = task_id.cloned();
        if new_id != self.current_task_id {
            self.scroll_offset = 0;
            self.current_task_id = new_id;
        }
    }
}

impl Default for Tab3State {
    fn default() -> Self {
        Tab3State::new()
    }
}

/// Returns a short pipeline label for an agent (without the " Agent" suffix).
fn pipeline_label(agent: &AgentKind) -> &'static str {
    match agent {
        AgentKind::Intake => "Intake",
        AgentKind::Design => "Design",
        AgentKind::Planning => "Planning",
        AgentKind::Implementation => "Implementation",
        AgentKind::CodeQuality => "Code Quality",
        AgentKind::SecurityReview => "Security Review",
        AgentKind::CodeReview => "Code Review",
        AgentKind::Human => "Human",
    }
}

/// Maps a `WorkflowPhase` to a human-readable status string.
fn phase_text(phase: &WorkflowPhase) -> &'static str {
    match phase {
        WorkflowPhase::Idle => "Idle",
        WorkflowPhase::Running => "Running",
        WorkflowPhase::AwaitingAnswer { .. } => "Awaiting Answer",
        WorkflowPhase::PendingReview => "Pending Review",
        WorkflowPhase::Completed => "Completed",
        WorkflowPhase::Errored { .. } => "Errored",
    }
}

/// Renders the Team Status tab into `area`.
///
/// Displays a pipeline visualization, current workflow phase, and a scrollable
/// work log (newest-first) for the selected task. Shows a placeholder when no
/// task is selected.
///
/// # Arguments
///
/// * `frame` - The ratatui frame to render into.
/// * `area` - The screen rectangle to occupy.
/// * `task` - The currently selected task, or `None` if none is selected.
/// * `workflow_state` - The current workflow state for the task, or `None`.
/// * `state` - Mutable UI state (scroll offset, current task id).
pub fn render(
    frame: &mut Frame,
    area: Rect,
    task: Option<&Task>,
    workflow_state: Option<&WorkflowState>,
    state: &Tab3State,
) {
    let Some(task) = task else {
        let placeholder = Paragraph::new("Select a task to view team status")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title("Team Status").borders(Borders::ALL));
        frame.render_widget(placeholder, area);
        return;
    };

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);

    let pipeline_area = layout[0];
    let phase_area = layout[1];
    let log_area = layout[2];

    // --- Pipeline bar ---
    let current_agent_idx = workflow_state.map(|ws| ws.current_agent.pipeline_index());
    let mut spans: Vec<Span> = Vec::new();
    for (i, agent) in AgentKind::all().iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(" > ", Style::default().fg(Color::DarkGray)));
        }
        let idx = agent.pipeline_index();
        let style = match current_agent_idx {
            None => Style::default().fg(Color::DarkGray),
            Some(curr) if idx < curr => Style::default().fg(Color::Green),
            Some(curr) if idx == curr => Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
            _ => Style::default().fg(Color::DarkGray),
        };
        spans.push(Span::styled(pipeline_label(agent), style));
    }

    let pipeline_para = Paragraph::new(Line::from(spans))
        .block(Block::default().title("Pipeline").borders(Borders::ALL));
    frame.render_widget(pipeline_para, pipeline_area);

    // --- Phase ---
    let phase_str = workflow_state
        .map(|ws| phase_text(&ws.phase))
        .unwrap_or("No workflow active");
    let phase_para =
        Paragraph::new(phase_str).block(Block::default().title("Phase").borders(Borders::ALL));
    frame.render_widget(phase_para, phase_area);

    // --- Work log (newest-first) ---
    let log_lines: Vec<Line> = if task.work_log.is_empty() {
        vec![Line::from(Span::styled(
            "No work log entries",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        task.work_log
            .iter()
            .rev()
            .map(|entry| {
                let ts = entry.timestamp.format("%Y-%m-%d %H:%M").to_string();
                let agent = entry.agent.display_name();
                Line::from(format!("[{ts}] [{agent}] {}", entry.description))
            })
            .collect()
    };

    let log_para = Paragraph::new(log_lines)
        .block(Block::default().title("Work Log").borders(Borders::ALL))
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset, 0));
    frame.render_widget(log_para, log_area);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::tasks::models::{Task, TaskId, TaskStatus, WorkLogEntry};
    use crate::workflow::agents::AgentKind;
    use crate::workflow::transitions::{WorkflowPhase, WorkflowState};

    fn make_task(work_log: Vec<WorkLogEntry>) -> Task {
        Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log,
            file_path: PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
        }
    }

    fn buf_content(terminal: &Terminal<TestBackend>) -> String {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect()
    }

    #[test]
    fn test_pipeline_render_all_agents() {
        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let task = make_task(vec![]);
        let wf_state = WorkflowState {
            task_id: TaskId::from_path("tasks/1.1.md"),
            current_agent: AgentKind::Planning,
            session_id: None,
            phase: WorkflowPhase::Running,
        };

        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    Some(&task),
                    Some(&wf_state),
                    &Tab3State::new(),
                );
            })
            .unwrap();

        let content = buf_content(&terminal);
        assert!(content.contains("Intake"), "missing Intake in pipeline");
        assert!(content.contains("Design"), "missing Design in pipeline");
        assert!(content.contains("Planning"), "missing Planning in pipeline");
        assert!(
            content.contains("Implementation"),
            "missing Implementation in pipeline"
        );
        assert!(
            content.contains("Code Quality"),
            "missing Code Quality in pipeline"
        );
        assert!(
            content.contains("Security Review"),
            "missing Security Review in pipeline"
        );
        assert!(
            content.contains("Code Review"),
            "missing Code Review in pipeline"
        );
    }

    #[test]
    fn test_work_log_empty() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let task = make_task(vec![]);

        terminal
            .draw(|frame| {
                render(frame, frame.area(), Some(&task), None, &Tab3State::new());
            })
            .unwrap();

        let content = buf_content(&terminal);
        assert!(
            content.contains("No work log entries"),
            "missing empty-state text; got: {content}"
        );
    }

    #[test]
    fn test_tab3_no_task() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|frame| {
                render(frame, frame.area(), None, None, &Tab3State::new());
            })
            .unwrap();

        let content = buf_content(&terminal);
        assert!(
            content.contains("Select a task to view team status"),
            "missing placeholder text; got: {content}"
        );
    }

    #[test]
    fn test_tab3_scroll_up_saturates() {
        let mut state = Tab3State::new();
        state.scroll_up();
        assert_eq!(state.scroll_offset, 0);
    }

    #[test]
    fn test_tab3_scroll_down_clamps() {
        let mut state = Tab3State::new();
        // With 3 entries, max offset is 2.
        state.scroll_down(3);
        assert_eq!(state.scroll_offset, 1);
        state.scroll_down(3);
        assert_eq!(state.scroll_offset, 2);
        state.scroll_down(3);
        assert_eq!(state.scroll_offset, 2); // clamped
    }

    #[test]
    fn test_work_log_with_entries() {
        use chrono::{TimeZone, Utc};

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let ts1 = Utc.with_ymd_and_hms(2024, 1, 15, 10, 30, 0).unwrap();
        let ts2 = Utc.with_ymd_and_hms(2024, 1, 16, 14, 0, 0).unwrap();
        let task = make_task(vec![
            WorkLogEntry {
                sequence: 1,
                timestamp: ts1,
                agent: AgentKind::Intake,
                description: "Gathered requirements".to_string(),
            },
            WorkLogEntry {
                sequence: 2,
                timestamp: ts2,
                agent: AgentKind::Design,
                description: "Produced design notes".to_string(),
            },
        ]);

        terminal
            .draw(|frame| {
                render(frame, frame.area(), Some(&task), None, &Tab3State::new());
            })
            .unwrap();

        let content = buf_content(&terminal);
        // Entries are rendered newest-first, so ts2 (Design) appears before ts1 (Intake).
        assert!(
            content.contains("2024-01-16 14:00"),
            "missing ts2 timestamp in work log; got: {content}"
        );
        assert!(
            content.contains("Design Agent"),
            "missing Design Agent in work log; got: {content}"
        );
        assert!(
            content.contains("Produced design notes"),
            "missing ts2 description in work log; got: {content}"
        );
        assert!(
            content.contains("2024-01-15 10:30"),
            "missing ts1 timestamp in work log; got: {content}"
        );
        assert!(
            content.contains("Intake Agent"),
            "missing Intake Agent in work log; got: {content}"
        );
        assert!(
            content.contains("Gathered requirements"),
            "missing ts1 description in work log; got: {content}"
        );
    }

    #[test]
    fn test_pipeline_coloring() {
        use ratatui::style::Color;

        let backend = TestBackend::new(120, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let task = make_task(vec![]);
        let wf_state = WorkflowState {
            task_id: TaskId::from_path("tasks/1.1.md"),
            current_agent: AgentKind::Planning,
            session_id: None,
            phase: WorkflowPhase::Running,
        };

        terminal
            .draw(|frame| {
                render(
                    frame,
                    frame.area(),
                    Some(&task),
                    Some(&wf_state),
                    &Tab3State::new(),
                );
            })
            .unwrap();

        // Pipeline content is on row 1 (row 0 is the top border of the Pipeline block).
        // Labels start at col 1. Layout with Planning as current (index 2):
        //   "Intake" (6 chars) at cols 1-6    -> index 0, green (completed)
        //   " > " (3 chars)    at cols 7-9
        //   "Design" (6 chars) at cols 10-15  -> index 1, green (completed)
        //   " > " (3 chars)    at cols 16-18
        //   "Planning" (8 chars) at cols 19-26 -> index 2, yellow+bold (current)
        //   " > " (3 chars)    at cols 27-29
        //   "Implementation" (14 chars) at cols 30-43 -> index 3, dark gray (pending)
        let buffer = terminal.backend().buffer().clone();

        let intake_cell = &buffer[(1, 1)];
        assert_eq!(
            intake_cell.fg,
            Color::Green,
            "Intake should be green (completed)"
        );

        let design_cell = &buffer[(10, 1)];
        assert_eq!(
            design_cell.fg,
            Color::Green,
            "Design should be green (completed)"
        );

        let planning_cell = &buffer[(19, 1)];
        assert_eq!(
            planning_cell.fg,
            Color::Yellow,
            "Planning should be yellow (current)"
        );
        assert!(
            planning_cell.modifier.contains(Modifier::BOLD),
            "Planning should be bold (current)"
        );

        let impl_cell = &buffer[(30, 1)];
        assert_eq!(
            impl_cell.fg,
            Color::DarkGray,
            "Implementation should be dark gray (pending)"
        );
    }

    #[test]
    fn test_tab3_set_displayed_task_resets_scroll() {
        let mut state = Tab3State::new();
        let id1 = TaskId::from_path("tasks/1.1.md");
        let id2 = TaskId::from_path("tasks/1.2.md");

        // Advance scroll before setting a task.
        state.scroll_down(10);
        assert_eq!(state.scroll_offset, 1);

        // Switching from None to id1 resets scroll.
        state.set_displayed_task(Some(&id1));
        assert_eq!(state.scroll_offset, 0, "scroll should reset on task change");

        // Advance scroll again.
        state.scroll_down(10);
        assert_eq!(state.scroll_offset, 1);

        // Setting the same task does not reset scroll.
        state.set_displayed_task(Some(&id1));
        assert_eq!(
            state.scroll_offset, 1,
            "scroll should not reset for same task"
        );

        // Switching to a different task resets scroll.
        state.set_displayed_task(Some(&id2));
        assert_eq!(
            state.scroll_offset, 0,
            "scroll should reset on different task"
        );
    }
}
