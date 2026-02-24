//! Tab 1: task markdown display, supplemental prompt input, and Q&A section.
//!
//! Renders the selected task's metadata and description (top), a `tui-textarea`
//! prompt input field (middle), and the question/answer history (bottom).

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_textarea::TextArea;

use crate::tasks::models::ParseErrorInfo;
use crate::tasks::{Task, TaskId};

/// UI state for Tab 1 (Task Details).
pub struct Tab1State {
    /// Supplemental prompt input field shown below the task description.
    pub prompt_input: TextArea<'static>,
    /// One textarea per unanswered question (indexed to match unanswered questions in order).
    pub answer_inputs: Vec<TextArea<'static>>,
    /// Index into `answer_inputs` of the currently focused answer textarea, if any.
    pub focused_answer: Option<usize>,
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
            answer_inputs: Vec::new(),
            focused_answer: None,
            prompt_focused: false,
            current_task_id: None,
            desc_scroll: 0,
        }
    }

    /// Rebuilds `answer_inputs` to match the number of unanswered questions in `task`.
    ///
    /// Preserves existing textareas up to the new count; appends new empty ones as needed.
    pub fn sync_answer_inputs(&mut self, task: &Task) {
        let unanswered_count = task.questions.iter().filter(|q| q.answer.is_none()).count();
        self.answer_inputs.truncate(unanswered_count);
        while self.answer_inputs.len() < unanswered_count {
            let mut ta = TextArea::default();
            ta.set_block(Block::default().title("Your Answer").borders(Borders::ALL));
            self.answer_inputs.push(ta);
        }
        // Clamp focused_answer.
        if let Some(idx) = self.focused_answer {
            if idx >= unanswered_count {
                self.focused_answer = None;
            }
        }
    }

    /// Resets all per-task UI state and rebuilds answer inputs for `task`.
    ///
    /// Clears prompt text, removes focus, rebuilds answer textareas, and
    /// records `task.id` as the current task so subsequent navigations can
    /// detect when the task changes.
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
        self.focused_answer = None;
        self.desc_scroll = 0;
        self.sync_answer_inputs(task);
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

    /// Sets the answer textarea at `idx` to the focused (yellow border) style.
    pub fn set_answer_focused_style(&mut self, idx: usize) {
        if let Some(ta) = self.answer_inputs.get_mut(idx) {
            ta.set_block(Self::focused_block("Your Answer"));
        }
    }

    /// Sets the answer textarea at `idx` to the unfocused (default border) style.
    pub fn set_answer_unfocused_style(&mut self, idx: usize) {
        if let Some(ta) = self.answer_inputs.get_mut(idx) {
            ta.set_block(Self::unfocused_block("Your Answer"));
        }
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
/// When a task is selected, displays metadata, description, supplemental prompt,
/// and the question/answer section.
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

    // Count answered vs. unanswered questions to size the questions section.
    let answered: Vec<_> = task
        .questions
        .iter()
        .filter(|q| q.answer.is_some())
        .collect();
    let unanswered: Vec<_> = task
        .questions
        .iter()
        .filter(|q| q.answer.is_none())
        .collect();

    // Each unanswered question gets ~5 rows (label + textarea), answered ~3 rows.
    let questions_height = (answered.len() * 3 + unanswered.len() * 5) as u16;

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6), // metadata (4 lines + 2 border rows)
            Constraint::Min(2),    // description (scrollable via PgUp/PgDn)
            Constraint::Length(5), // supplemental prompt
            Constraint::Length(questions_height.max(3)), // questions (at least 3 rows for border)
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

    // --- Questions ---
    if task.questions.is_empty() {
        let no_q = Paragraph::new("No questions.")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title("Questions").borders(Borders::ALL));
        frame.render_widget(no_q, sections[3]);
        return;
    }

    // Stack answered and unanswered questions vertically within the questions area.
    let q_area = sections[3];
    let mut row_constraints: Vec<Constraint> = Vec::new();
    for _q in &answered {
        row_constraints.push(Constraint::Length(3));
    }
    for _q in &unanswered {
        row_constraints.push(Constraint::Length(5));
    }
    if row_constraints.is_empty() {
        return;
    }

    let q_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(row_constraints)
        .split(q_area);

    let mut row_idx = 0usize;
    for q in &answered {
        if row_idx >= q_rows.len() {
            break;
        }
        let answer_text = q.answer.as_deref().unwrap_or("");
        let agent_label = format!("Q ({}): ", q.agent.display_name());
        let lines = vec![
            Line::from(vec![
                Span::styled(agent_label, Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(q.text.clone()),
            ]),
            Line::from(vec![
                Span::styled("A: ", Style::default().fg(Color::Green)),
                Span::raw(answer_text.to_string()),
            ]),
        ];
        let para = Paragraph::new(lines).block(Block::default().borders(Borders::ALL));
        frame.render_widget(para, q_rows[row_idx]);
        row_idx += 1;
    }

    for (answer_input_idx, q) in unanswered.iter().enumerate() {
        if row_idx >= q_rows.len() {
            break;
        }
        let label = format!("Q ({}): {}", q.agent.display_name(), q.text);
        let area = q_rows[row_idx];

        // Split into label row (1) + textarea (4).
        let q_split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(3)])
            .split(area);

        let label_para = Paragraph::new(label).style(Style::default().add_modifier(Modifier::BOLD));
        frame.render_widget(label_para, q_split[0]);

        if let Some(ta) = state.answer_inputs.get(answer_input_idx) {
            frame.render_widget(ta, q_split[1]);
        }

        row_idx += 1;
    }
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
        // Prompt textarea should be empty (no lines of content beyond the initial empty line).
        assert!(state.answer_inputs.is_empty());
        assert!(state.focused_answer.is_none());
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
        use crate::tasks::models::{Question, TaskStatus};
        use crate::workflow::agents::AgentKind;

        let mut state = Tab1State::new();
        // Set some pre-existing state that should be cleared.
        state.prompt_focused = true;
        state.focused_answer = Some(0);

        let mut task = make_task("desc");
        task.questions.push(Question {
            agent: AgentKind::Intake,
            text: "What is the scope?".to_string(),
            answer: None,
        });
        task.questions.push(Question {
            agent: AgentKind::Design,
            text: "Answered already?".to_string(),
            answer: Some("Yes".to_string()),
        });

        state.reset_for_task(&task);

        // prompt cleared.
        assert!(!state.prompt_focused);
        // focus cleared.
        assert!(state.focused_answer.is_none());
        // One unanswered question -> one answer textarea.
        assert_eq!(state.answer_inputs.len(), 1);
        // current_task_id set to the task's id.
        assert_eq!(state.current_task_id, Some(task.id.clone()));
        // Status field is not relevant to this test.
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
    fn test_task_details_questions_section_visible_24_rows() {
        // In a 24-row terminal the questions section must be allocated at least 3 rows
        // (enough for a bordered block) even when the task has no questions.
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let task = make_task("desc");
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
            content.contains("Questions") || content.contains("No questions"),
            "Questions section should be visible in a 24-row terminal, got: {content:?}"
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
}
