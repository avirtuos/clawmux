//! Tab 0: task metadata display, description, and supplemental prompt input.
//!
//! Renders the selected task's metadata and description (top sections) and a
//! `tui-textarea` supplemental prompt input field (bottom). Questions have
//! been moved to Tab 1 (questions.rs).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_textarea::TextArea;

use crate::tasks::models::ParseErrorInfo;
use crate::tasks::{Task, TaskId};

/// UI state for Tab 0 (Task Details).
pub struct Tab1State {
    /// Supplemental prompt input field shown below the task description.
    pub prompt_input: TextArea<'static>,
    /// Whether the supplemental prompt input has keyboard focus.
    pub prompt_focused: bool,
    /// The ID of the task currently displayed, used to detect task changes.
    pub current_task_id: Option<TaskId>,
    /// Vertical scroll offset for the description paragraph.
    pub desc_scroll: u16,
}

impl Tab1State {
    /// Creates a new `Tab1State` with an empty prompt input and no focus.
    pub fn new() -> Self {
        let mut prompt_input = TextArea::default();
        prompt_input.set_block(
            Block::default()
                .title("Supplemental Prompt")
                .borders(Borders::ALL),
        );
        Tab1State {
            prompt_input,
            prompt_focused: false,
            current_task_id: None,
            desc_scroll: 0,
        }
    }

    /// Resets all per-task UI state for `task`.
    ///
    /// Clears prompt text, removes focus, resets scroll, and records `task.id`
    /// as the current task so subsequent navigations can detect when the task changes.
    pub fn reset_for_task(&mut self, task: &Task) {
        self.prompt_input = {
            let mut ta = TextArea::default();
            ta.set_block(
                Block::default()
                    .title("Supplemental Prompt")
                    .borders(Borders::ALL),
            );
            ta
        };
        self.prompt_focused = false;
        self.desc_scroll = 0;
        self.current_task_id = Some(task.id.clone());
    }

    /// Scrolls the description up by one line (clamped at 0).
    pub fn scroll_desc_up(&mut self) {
        self.desc_scroll = self.desc_scroll.saturating_sub(1);
    }

    /// Scrolls the description down by one line.
    pub fn scroll_desc_down(&mut self) {
        self.desc_scroll = self.desc_scroll.saturating_add(1);
    }

    /// Returns a [`Block`] with a yellow border for focused widgets.
    fn focused_block(title: &'static str) -> Block<'static> {
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
    }

    /// Returns a [`Block`] with the default border style for unfocused widgets.
    fn unfocused_block(title: &'static str) -> Block<'static> {
        Block::default().title(title).borders(Borders::ALL)
    }

    /// Sets the supplemental prompt textarea to the focused (yellow border) style.
    pub fn set_prompt_focused_style(&mut self) {
        self.prompt_input
            .set_block(Self::focused_block("Supplemental Prompt"));
    }

    /// Sets the supplemental prompt textarea to the unfocused (default border) style.
    pub fn set_prompt_unfocused_style(&mut self) {
        self.prompt_input
            .set_block(Self::unfocused_block("Supplemental Prompt"));
    }
}

impl Default for Tab1State {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders the malformed-task view into `area`.
///
/// Displays three vertical sections:
/// 1. A red-bordered error banner with the parse error message.
/// 2. A scrollable paragraph showing the raw file content.
/// 3. An action area indicating fix status or prompting the user.
fn render_malformed_view(
    frame: &mut Frame,
    area: Rect,
    task: &Task,
    error_info: &ParseErrorInfo,
    state: &Tab1State,
) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // error banner
            Constraint::Min(2),    // raw content (scrollable)
            Constraint::Length(6), // action area
        ])
        .split(area);

    // --- Error banner ---
    let banner_lines = vec![
        Line::from(Span::styled(
            "PARSE ERROR",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::raw(error_info.error_message.clone())),
        Line::from(Span::styled(
            format!("File: {}", task.file_path.display()),
            Style::default().fg(Color::DarkGray),
        )),
    ];
    let banner = Paragraph::new(banner_lines).block(
        Block::default()
            .title("Parse Error")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Red)),
    );
    frame.render_widget(banner, sections[0]);

    // --- Raw content ---
    let raw_para = Paragraph::new(error_info.raw_content.clone())
        .block(
            Block::default()
                .title("Raw File Content")
                .borders(Borders::ALL),
        )
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((state.desc_scroll, 0));
    frame.render_widget(raw_para, sections[1]);

    // --- Action area ---
    let action_widget = if error_info.fix_in_progress {
        Paragraph::new("Requesting fix from AI...").block(
            Block::default()
                .title("Fix Suggestion")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
    } else if let Some(ref fix) = error_info.suggested_fix {
        let lines = vec![
            Line::from(fix.explanation.clone()),
            Line::from(""),
            Line::from(Span::styled(
                "Press Enter to apply fix",
                Style::default().fg(Color::Green),
            )),
        ];
        Paragraph::new(lines).block(
            Block::default()
                .title("Suggested Fix")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Green)),
        )
    } else if let Some(ref err) = error_info.fix_error {
        let lines = vec![
            Line::from(Span::styled(
                "Fix request failed:",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::raw(err.clone())),
            Line::from(""),
            Line::from(Span::styled(
                "Press 'f' to retry",
                Style::default().fg(Color::DarkGray),
            )),
        ];
        Paragraph::new(lines).block(
            Block::default()
                .title("Fix Error")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red)),
        )
    } else {
        Paragraph::new("Press 'f' to request an AI fix suggestion.")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .title("Fix Suggestion")
                    .borders(Borders::ALL),
            )
    };
    frame.render_widget(action_widget, sections[2]);
}

/// Renders the Task Details tab into `area`.
///
/// When no task is selected (`task` is `None`), displays a centered placeholder.
/// When a task is malformed, delegates to [`render_malformed_view`].
/// When a task is selected, displays metadata, description, and supplemental prompt.
pub fn render(frame: &mut Frame, area: Rect, task: Option<&Task>, state: &Tab1State) {
    let Some(task) = task else {
        let placeholder = Paragraph::new("Select a task from the list")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(placeholder, area);
        return;
    };

    if let Some(ref error_info) = task.parse_error {
        render_malformed_view(frame, area, task, error_info, state);
        return;
    }

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // metadata (4 lines + 2 border rows)
            Constraint::Min(2),    // description (scrollable via PgUp/PgDn)
            Constraint::Length(5), // supplemental prompt
        ])
        .split(area);

    // --- Metadata ---
    let assigned_str = task
        .assigned_to
        .as_ref()
        .map(|a| a.display_name().to_string())
        .unwrap_or_else(|| "None".to_string());

    let meta_lines = vec![
        Line::from(vec![
            Span::styled("Story:  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(task.story_name.clone()),
        ]),
        Line::from(vec![
            Span::styled("Task:   ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(task.name.clone()),
        ]),
        Line::from(vec![
            Span::styled("Status: ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(task.status.to_string()),
        ]),
        Line::from(vec![
            Span::styled("Agent:  ", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(assigned_str),
        ]),
    ];
    let meta_para =
        Paragraph::new(meta_lines).block(Block::default().title("Metadata").borders(Borders::ALL));
    frame.render_widget(meta_para, sections[0]);

    // --- Description ---
    let desc_para = Paragraph::new(task.description.clone())
        .block(Block::default().title("Description").borders(Borders::ALL))
        .wrap(ratatui::widgets::Wrap { trim: false })
        .scroll((state.desc_scroll, 0));
    frame.render_widget(desc_para, sections[1]);

    // --- Supplemental Prompt ---
    frame.render_widget(&state.prompt_input, sections[2]);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::tasks::models::{Task, TaskId, TaskStatus};

    fn make_task(description: &str) -> Task {
        Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Test Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: description.to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        }
    }

    #[test]
    fn test_tab1_state_new() {
        let state = Tab1State::new();
        assert!(!state.prompt_focused);
        assert!(state.current_task_id.is_none());
        assert_eq!(state.desc_scroll, 0);
    }

    #[test]
    fn test_tab1_state_desc_scroll_initial() {
        let state = Tab1State::new();
        assert_eq!(state.desc_scroll, 0);
    }

    #[test]
    fn test_tab1_state_desc_scroll_down() {
        let mut state = Tab1State::new();
        state.scroll_desc_down();
        assert_eq!(state.desc_scroll, 1);
    }

    #[test]
    fn test_tab1_state_desc_scroll_up_clamps_at_zero() {
        let mut state = Tab1State::new();
        state.scroll_desc_up();
        assert_eq!(state.desc_scroll, 0);
    }

    #[test]
    fn test_tab1_state_reset_clears_desc_scroll() {
        let mut state = Tab1State::new();
        state.scroll_desc_down();
        state.scroll_desc_down();
        assert_eq!(state.desc_scroll, 2);
        let task = make_task("desc");
        state.reset_for_task(&task);
        assert_eq!(state.desc_scroll, 0);
    }

    #[test]
    fn test_tab1_state_reset_for_task() {
        let mut state = Tab1State::new();
        state.prompt_focused = true;

        let task = make_task("desc");
        state.reset_for_task(&task);

        assert!(!state.prompt_focused);
        assert_eq!(state.current_task_id, Some(task.id.clone()));
        assert_eq!(state.desc_scroll, 0);
        let _ = TaskStatus::Open;
    }

    #[test]
    fn test_task_details_no_task() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = Tab1State::new();

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
            content.contains("Select a task from the list"),
            "Buffer should contain placeholder text, got: {content:?}"
        );
    }

    #[test]
    fn test_render_malformed_shows_error() {
        use crate::tasks::models::ParseErrorInfo;

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut task = make_task("desc");
        task.parse_error = Some(ParseErrorInfo {
            error_message: "missing required Status field".to_string(),
            raw_content: "raw bad content".to_string(),
            suggested_fix: None,
            fix_in_progress: false,
            fix_error: None,
        });
        let state = Tab1State::new();

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
            content.contains("PARSE ERROR"),
            "should show PARSE ERROR banner; got: {content:?}"
        );
        assert!(
            content.contains("missing required Status field"),
            "should show error message; got: {content:?}"
        );
    }

    #[test]
    fn test_render_malformed_shows_raw_content() {
        use crate::tasks::models::ParseErrorInfo;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut task = make_task("desc");
        task.parse_error = Some(ParseErrorInfo {
            error_message: "parse error".to_string(),
            raw_content: "raw file content here".to_string(),
            suggested_fix: None,
            fix_in_progress: false,
            fix_error: None,
        });
        let state = Tab1State::new();

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
            content.contains("raw file content here"),
            "should show raw file content; got: {content:?}"
        );
    }

    #[test]
    fn test_render_malformed_with_fix() {
        use crate::tasks::models::{ParseErrorInfo, SuggestedFix};

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut task = make_task("desc");
        task.parse_error = Some(ParseErrorInfo {
            error_message: "parse error".to_string(),
            raw_content: "bad content".to_string(),
            suggested_fix: Some(SuggestedFix {
                corrected_content: "fixed content".to_string(),
                explanation: "Added missing Status line".to_string(),
            }),
            fix_in_progress: false,
            fix_error: None,
        });
        let state = Tab1State::new();

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
            content.contains("Added missing Status line"),
            "should show fix explanation; got: {content:?}"
        );
        assert!(
            content.contains("Press Enter to apply fix"),
            "should show apply prompt; got: {content:?}"
        );
    }

    #[test]
    fn test_task_details_shows_description() {
        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let task = make_task("Hello world");
        let state = Tab1State::new();

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
            content.contains("Hello world"),
            "Buffer should contain description, got: {content:?}"
        );
    }

    #[test]
    fn test_render_malformed_shows_fix_error() {
        use crate::tasks::models::ParseErrorInfo;

        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut task = make_task("desc");
        task.parse_error = Some(ParseErrorInfo {
            error_message: "parse error".to_string(),
            raw_content: "bad content".to_string(),
            suggested_fix: None,
            fix_in_progress: false,
            fix_error: Some("OpenCode server unavailable".to_string()),
        });
        let state = Tab1State::new();

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
            content.contains("Fix request failed:"),
            "should show fix error header; got: {content:?}"
        );
        assert!(
            content.contains("OpenCode server unavailable"),
            "should show fix error message; got: {content:?}"
        );
        assert!(
            content.contains("Press 'f' to retry"),
            "should show retry hint; got: {content:?}"
        );
    }

    #[test]
    fn test_task_details_no_questions_section() {
        // The details tab no longer shows questions -- verify by checking that
        // no "Questions" or "Your Answer" label appears in the rendered output.
        let backend = TestBackend::new(80, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut task = make_task("desc");
        task.questions.push(crate::tasks::models::Question {
            agent: crate::workflow::agents::AgentKind::Intake,
            text: "What is scope?".to_string(),
            answer: None,
            opencode_request_id: None,
        });
        let state = Tab1State::new();

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
            !content.contains("Your Answer"),
            "details tab should not show answer textarea; got: {content:?}"
        );
    }
}
