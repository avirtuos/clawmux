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

use crate::tasks::Task;

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
        }
    }

    /// Rebuilds `answer_inputs` to match the number of unanswered questions in `task`.
    ///
    /// Preserves existing textareas up to the new count; appends new empty ones as needed.
    #[allow(dead_code)]
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
}

impl Default for Tab1State {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders the Task Details tab into `area`.
///
/// When no task is selected (`task` is `None`), displays a centered placeholder.
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
    let questions_height = (answered.len() * 3 + unanswered.len() * 5).max(0) as u16;

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),                       // metadata
            Constraint::Min(4),                          // description
            Constraint::Length(5),                       // supplemental prompt
            Constraint::Length(questions_height.max(1)), // questions (at least 1 row)
        ])
        .split(area);

    // --- Metadata ---
    let assigned_str = task
        .assigned_to
        .as_ref()
        .map(|a| format!("{a:?}"))
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
        .wrap(ratatui::widgets::Wrap { trim: false });
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
        let agent_label = format!("Q ({:?}): ", q.agent);
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
        let label = format!("Q ({:?}): {}", q.agent, q.text);
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
        }
    }

    #[test]
    fn test_tab1_state_new() {
        let state = Tab1State::new();
        // Prompt textarea should be empty (no lines of content beyond the initial empty line).
        assert!(state.answer_inputs.is_empty());
        assert!(state.focused_answer.is_none());
        assert!(!state.prompt_focused);
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
