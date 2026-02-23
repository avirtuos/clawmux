//! Tab 2: streaming agent activity view.
//!
//! Renders streaming agent output from SSE message events, tool execution
//! activity indicators, and agent reasoning text. Replaces PTY-based terminal
//! emulation with structured streaming text display.

use std::collections::HashMap;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::opencode::types::MessagePart;
use crate::tasks::TaskId;

/// A single line of activity in the agent activity tab.
#[derive(Debug, Clone)]
pub enum ActivityLine {
    /// A text segment from streaming output (Text, Reasoning, or File parts).
    Text { content: String },
    /// A tool invocation status update.
    ToolActivity { tool: String, status: String },
    #[allow(dead_code)]
    /// A banner message from the agent (e.g. session start/complete).
    // TODO: Task 7.x -- wire on CreateSession/AgentCompleted
    AgentBanner { message: String },
}

/// UI state for Tab 2 (Agent Activity).
///
/// Buffers per-task activity lines and manages scroll position for the
/// currently displayed task.
pub struct Tab2State {
    /// Per-task activity line buffers.
    lines: HashMap<TaskId, Vec<ActivityLine>>,
    /// Vertical scroll offset for the currently displayed task.
    scroll_offset: usize,
    /// The task currently displayed in the viewport.
    current_task_id: Option<TaskId>,
}

impl Tab2State {
    /// Creates a new `Tab2State` with empty buffers and no task selected.
    pub fn new() -> Self {
        Tab2State {
            lines: HashMap::new(),
            scroll_offset: 0,
            current_task_id: None,
        }
    }

    /// Converts `parts` to `ActivityLine` entries and appends them to the buffer for `task_id`.
    ///
    /// Automatically scrolls to the bottom if the task is currently displayed.
    /// `MessagePart::Unknown` parts are silently skipped.
    pub fn push_streaming(&mut self, task_id: &TaskId, parts: &[MessagePart]) {
        let buffer = self.lines.entry(task_id.clone()).or_default();
        for part in parts {
            match part {
                MessagePart::Text { text } => {
                    buffer.push(ActivityLine::Text {
                        content: text.clone(),
                    });
                }
                MessagePart::Reasoning { text } => {
                    buffer.push(ActivityLine::Text {
                        content: text.clone(),
                    });
                }
                MessagePart::File { path, .. } => {
                    buffer.push(ActivityLine::Text {
                        content: format!("[File: {path}]"),
                    });
                }
                MessagePart::Tool { name, .. } => {
                    buffer.push(ActivityLine::ToolActivity {
                        tool: name.clone(),
                        status: "called".to_string(),
                    });
                }
                MessagePart::Unknown => {}
            }
        }
        if self.current_task_id.as_ref() == Some(task_id) {
            self.scroll_to_bottom(task_id);
        }
    }

    /// Appends a `ToolActivity` line for `task_id`.
    ///
    /// Automatically scrolls to the bottom if the task is currently displayed.
    pub fn push_tool(&mut self, task_id: &TaskId, tool: String, status: String) {
        let buffer = self.lines.entry(task_id.clone()).or_default();
        buffer.push(ActivityLine::ToolActivity { tool, status });
        if self.current_task_id.as_ref() == Some(task_id) {
            self.scroll_to_bottom(task_id);
        }
    }

    /// Removes all buffered lines for `task_id` and resets scroll if it is displayed.
    #[allow(dead_code)]
    pub fn clear(&mut self, task_id: &TaskId) {
        self.lines.remove(task_id);
        if self.current_task_id.as_ref() == Some(task_id) {
            self.scroll_offset = 0;
        }
    }

    /// Scrolls the viewport up by one line (saturates at 0).
    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    /// Scrolls the viewport down by one line, clamped to `max_scroll()`.
    pub fn scroll_down(&mut self) {
        let max = self.max_scroll();
        if self.scroll_offset < max {
            self.scroll_offset += 1;
        }
    }

    /// Updates the currently displayed task.
    ///
    /// If the task changes, resets scroll and scrolls to the bottom of the new task.
    pub fn set_displayed_task(&mut self, task_id: Option<&TaskId>) {
        let new_id = task_id.cloned();
        if new_id != self.current_task_id {
            self.current_task_id = new_id;
            if let Some(ref id) = self.current_task_id.clone() {
                self.scroll_to_bottom(id);
            } else {
                self.scroll_offset = 0;
            }
        }
    }

    /// Returns the buffered activity lines for `task_id`, or an empty slice if none.
    pub fn lines_for(&self, task_id: &TaskId) -> &[ActivityLine] {
        self.lines.get(task_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Returns the maximum valid scroll offset for the currently displayed task.
    fn max_scroll(&self) -> usize {
        if let Some(ref id) = self.current_task_id {
            self.lines_for(id).len().saturating_sub(1)
        } else {
            0
        }
    }

    /// Scrolls to the last line of the buffer for `task_id`.
    fn scroll_to_bottom(&mut self, task_id: &TaskId) {
        let count = self.lines_for(task_id).len();
        self.scroll_offset = count.saturating_sub(1);
    }
}

impl Default for Tab2State {
    fn default() -> Self {
        Tab2State::new()
    }
}

/// Renders the Agent Activity tab into `area`.
///
/// Displays a placeholder when no task is selected. When a task is selected,
/// renders buffered activity lines with distinct styles per variant, scrolled
/// to `state.scroll_offset`.
pub fn render(frame: &mut Frame, area: Rect, task_id: Option<&TaskId>, state: &Tab2State) {
    let block = Block::default()
        .title("Agent Activity")
        .borders(Borders::ALL);

    if task_id.is_none() {
        let placeholder = Paragraph::new("Select a task to view agent activity")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(placeholder, area);
        return;
    }

    let task_id = task_id.unwrap();
    let lines: Vec<Line> = state
        .lines_for(task_id)
        .iter()
        .map(|line| match line {
            ActivityLine::Text { content } => Line::from(content.as_str()),
            ActivityLine::ToolActivity { tool, status } => Line::from(vec![
                Span::styled(
                    format!("[{tool}]"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(status.as_str(), Style::default().fg(Color::Yellow)),
            ]),
            ActivityLine::AgentBanner { message } => Line::from(Span::styled(
                message.as_str(),
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            )),
        })
        .collect();

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((state.scroll_offset as u16, 0));

    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::TaskId;

    fn task_id() -> TaskId {
        TaskId::from_path("tasks/1.1.md")
    }

    #[test]
    fn test_push_streaming_text() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_streaming(
            &id,
            &[MessagePart::Text {
                text: "hello".to_string(),
            }],
        );
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1);
        assert!(matches!(lines[0], ActivityLine::Text { ref content } if content == "hello"));
    }

    #[test]
    fn test_push_tool_activity() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_tool(&id, "bash".to_string(), "running".to_string());
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1);
        assert!(
            matches!(&lines[0], ActivityLine::ToolActivity { tool, status } if tool == "bash" && status == "running")
        );
    }

    #[test]
    fn test_clear_removes_task_lines() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_streaming(
            &id,
            &[MessagePart::Text {
                text: "line".to_string(),
            }],
        );
        assert_eq!(state.lines_for(&id).len(), 1);
        state.clear(&id);
        assert_eq!(state.lines_for(&id).len(), 0);
    }

    #[test]
    fn test_scroll_bounds() {
        let mut state = Tab2State::new();
        let id = task_id();

        // scroll_up from 0 stays 0.
        state.set_displayed_task(Some(&id));
        state.scroll_up();
        assert_eq!(state.scroll_offset, 0);

        // scroll_down with no lines stays 0.
        state.scroll_down();
        assert_eq!(state.scroll_offset, 0);

        // Push 5 lines; set_displayed_task auto-scrolls to bottom (index 4).
        for i in 0..5 {
            state.push_streaming(
                &id,
                &[MessagePart::Text {
                    text: format!("line {i}"),
                }],
            );
        }
        assert_eq!(state.scroll_offset, 4);

        // scroll_down at max stays clamped at 4.
        state.scroll_down();
        assert_eq!(state.scroll_offset, 4);

        // scroll_up decrements.
        state.scroll_up();
        assert_eq!(state.scroll_offset, 3);
    }
}
