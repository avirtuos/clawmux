//! Tab 1: Question/Answer display and navigation.
//!
//! Renders the selected task's questions one at a time with full-screen
//! treatment. Unanswered questions include an editable textarea; answered
//! questions show the stored answer text. Navigation between questions uses
//! PgUp/PgDn.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_textarea::TextArea;

use crate::tasks::{Task, TaskId};

/// UI state for the Questions tab (Tab 1).
pub struct QuestionsTabState {
    /// Index of the currently displayed question (into task.questions).
    pub selected_question: usize,
    /// One textarea per unanswered question, in the order they appear in task.questions.
    pub answer_inputs: Vec<TextArea<'static>>,
    /// Index into `answer_inputs` of the focused textarea, if any.
    pub focused_answer: Option<usize>,
    /// The ID of the task currently displayed, used to detect task changes.
    pub current_task_id: Option<TaskId>,
}

impl QuestionsTabState {
    /// Creates a new `QuestionsTabState` with empty defaults.
    pub fn new() -> Self {
        QuestionsTabState {
            selected_question: 0,
            answer_inputs: Vec::new(),
            focused_answer: None,
            current_task_id: None,
        }
    }

    /// Rebuilds `answer_inputs` to match the number of unanswered questions in `task`.
    ///
    /// Preserves existing textareas up to the new count; appends new empty ones as needed.
    /// Clamps `focused_answer` to remain in bounds.
    pub fn sync_answer_inputs(&mut self, task: &Task) {
        let unanswered_count = task.questions.iter().filter(|q| q.answer.is_none()).count();
        self.answer_inputs.truncate(unanswered_count);
        while self.answer_inputs.len() < unanswered_count {
            let mut ta = TextArea::default();
            ta.set_block(Self::unfocused_block("Your Answer"));
            self.answer_inputs.push(ta);
        }
        if let Some(idx) = self.focused_answer {
            if idx >= unanswered_count {
                self.focused_answer = None;
            }
        }
    }

    /// Resets all per-task UI state and rebuilds answer inputs for `task`.
    ///
    /// Resets `selected_question` to 0, clears focus, rebuilds answer textareas,
    /// and records `task.id` as the current task.
    pub fn reset_for_task(&mut self, task: &Task) {
        self.selected_question = 0;
        self.focused_answer = None;
        self.sync_answer_inputs(task);
        self.current_task_id = Some(task.id.clone());
    }

    /// Decrements `selected_question`, clamped at 0.
    pub fn select_prev(&mut self) {
        self.selected_question = self.selected_question.saturating_sub(1);
    }

    /// Increments `selected_question`, clamped at `total - 1`.
    ///
    /// If `total` is 0 this is a no-op.
    pub fn select_next(&mut self, total: usize) {
        if total > 0 {
            self.selected_question = (self.selected_question + 1).min(total - 1);
        }
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
}

impl Default for QuestionsTabState {
    fn default() -> Self {
        Self::new()
    }
}

/// Maps a display index (0 = newest) to the underlying `task.questions` vector index.
///
/// Display index 0 shows the most recently added question (highest vector index).
/// Returns 0 when `total` is 0 to avoid underflow.
pub(crate) fn display_to_question_idx(display_idx: usize, total: usize) -> usize {
    total.saturating_sub(1).saturating_sub(display_idx)
}

/// Renders the Questions tab into `area`.
///
/// When no task is selected (`task` is `None`), displays a placeholder.
/// When the task has no questions, displays a "No questions yet." message.
/// Otherwise shows one question at a time (indexed by `state.selected_question`)
/// with an answer textarea for unanswered questions or the stored answer text
/// for answered ones. Navigation hint is shown at the bottom.
pub fn render(frame: &mut Frame, area: Rect, task: Option<&Task>, state: &QuestionsTabState) {
    let Some(task) = task else {
        let placeholder = Paragraph::new("Select a task from the list")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(placeholder, area);
        return;
    };

    if task.questions.is_empty() {
        let no_q = Paragraph::new("No questions yet.")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title("Questions").borders(Borders::ALL));
        frame.render_widget(no_q, area);
        return;
    }

    let total = task.questions.len();
    let sel_display = state.selected_question.min(total.saturating_sub(1));
    let sel = display_to_question_idx(sel_display, total);
    let q = &task.questions[sel];

    let block_title = format!("Questions ({}/{})", sel_display + 1, total);
    let block = Block::default().title(block_title).borders(Borders::ALL);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner into content area (top) and hint line (bottom).
    let inner_sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(2), Constraint::Length(1)])
        .split(inner);

    let content_area = inner_sections[0];
    let hint_area = inner_sections[1];

    if let Some(answer_text) = &q.answer {
        // Answered: show Q label and A text.
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    format!("Q ({}): ", q.agent.display_name()),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(q.text.clone()),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "A: ",
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(answer_text.clone()),
            ]),
        ];
        let para = Paragraph::new(lines).wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(para, content_area);
    } else {
        // Unanswered: show Q label above a textarea.
        //
        // `answer_inputs` only covers unanswered questions. The index into it
        // equals the number of unanswered questions appearing before `sel`.
        let answer_idx = task.questions[..sel]
            .iter()
            .filter(|q| q.answer.is_none())
            .count();

        let q_sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(4)])
            .split(content_area);

        let q_label = Paragraph::new(Line::from(vec![
            Span::styled(
                format!("Q ({}): ", q.agent.display_name()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(q.text.clone()),
        ]))
        .wrap(ratatui::widgets::Wrap { trim: false });
        frame.render_widget(q_label, q_sections[0]);

        if let Some(ta) = state.answer_inputs.get(answer_idx) {
            frame.render_widget(ta, q_sections[1]);
        }
    }

    // Render per-tab hint at the bottom of the block interior.
    let hint = Paragraph::new("[PgUp] prev | [PgDn] next | [a] focus | [Alt+Enter] submit")
        .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hint, hint_area);
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::tasks::models::{Question, Task, TaskId, TaskStatus};
    use crate::workflow::agents::AgentKind;

    fn make_task() -> Task {
        Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Test Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: "desc".to_string(),
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

    fn make_question(text: &str, answer: Option<&str>) -> Question {
        Question {
            agent: AgentKind::Intake,
            text: text.to_string(),
            answer: answer.map(|s| s.to_string()),
        }
    }

    #[test]
    fn test_display_to_question_idx() {
        // With 3 questions: display 0 -> index 2 (newest), display 2 -> index 0 (oldest).
        assert_eq!(display_to_question_idx(0, 3), 2);
        assert_eq!(display_to_question_idx(1, 3), 1);
        assert_eq!(display_to_question_idx(2, 3), 0);
        // With 1 question: display 0 -> index 0.
        assert_eq!(display_to_question_idx(0, 1), 0);
    }

    #[test]
    fn test_display_to_question_idx_empty() {
        // With 0 questions: saturating arithmetic should not panic.
        assert_eq!(display_to_question_idx(0, 0), 0);
    }

    #[test]
    fn test_questions_no_task_placeholder() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let state = QuestionsTabState::new();

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
            "should show placeholder when no task; got: {content:?}"
        );
    }

    #[test]
    fn test_questions_no_questions_placeholder() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let task = make_task();
        let state = QuestionsTabState::new();

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
            content.contains("No questions yet."),
            "should show 'No questions yet.' when task has no questions; got: {content:?}"
        );
    }

    #[test]
    fn test_questions_renders_unanswered() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut task = make_task();
        task.questions
            .push(make_question("What is the scope?", None));
        let mut state = QuestionsTabState::new();
        state.sync_answer_inputs(&task);

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
            content.contains("What is the scope"),
            "should show question text; got: {content:?}"
        );
        assert!(
            content.contains("Your Answer"),
            "should show textarea for unanswered question; got: {content:?}"
        );
    }

    #[test]
    fn test_questions_renders_answered() {
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let mut task = make_task();
        task.questions
            .push(make_question("What is the scope?", Some("Minimal scope.")));
        let state = QuestionsTabState::new();

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
            content.contains("Minimal scope."),
            "should show answer text for answered question; got: {content:?}"
        );
        assert!(
            !content.contains("Your Answer"),
            "should not show textarea for answered question; got: {content:?}"
        );
    }

    #[test]
    fn test_select_prev_clamps_at_zero() {
        let mut state = QuestionsTabState::new();
        state.selected_question = 0;
        state.select_prev();
        assert_eq!(state.selected_question, 0, "select_prev should clamp at 0");
    }

    #[test]
    fn test_select_next_clamps_at_max() {
        let mut state = QuestionsTabState::new();
        state.selected_question = 2;
        state.select_next(3); // max index = 2
        assert_eq!(
            state.selected_question, 2,
            "select_next should clamp at total-1"
        );
    }

    #[test]
    fn test_select_next_noop_when_total_zero() {
        let mut state = QuestionsTabState::new();
        state.selected_question = 0;
        state.select_next(0);
        assert_eq!(
            state.selected_question, 0,
            "select_next with total=0 should be a no-op"
        );
    }

    #[test]
    fn test_sync_answer_inputs_count() {
        let mut task = make_task();
        task.questions.push(make_question("Q1?", None)); // unanswered
        task.questions.push(make_question("Q2?", Some("answer"))); // answered
        task.questions.push(make_question("Q3?", None)); // unanswered

        let mut state = QuestionsTabState::new();
        state.sync_answer_inputs(&task);

        assert_eq!(
            state.answer_inputs.len(),
            2,
            "should have exactly 2 textareas for 2 unanswered questions"
        );
    }

    #[test]
    fn test_reset_for_task_clears_state() {
        let mut task = make_task();
        task.questions.push(make_question("Q1?", None));

        let mut state = QuestionsTabState::new();
        state.selected_question = 3;
        state.focused_answer = Some(1);
        state.reset_for_task(&task);

        assert_eq!(
            state.selected_question, 0,
            "selected_question should reset to 0"
        );
        assert!(
            state.focused_answer.is_none(),
            "focused_answer should be cleared"
        );
        assert_eq!(
            state.answer_inputs.len(),
            1,
            "should have 1 textarea for 1 unanswered Q"
        );
        assert_eq!(state.current_task_id, Some(task.id.clone()));
    }
}
