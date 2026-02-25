//! Tab 2: streaming agent activity view.
//!
//! Renders streaming agent output from SSE message events, tool execution
//! activity indicators, and agent reasoning text. Replaces PTY-based terminal
//! emulation with structured streaming text display.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;
use tui_textarea::TextArea;

use crate::opencode::types::{MessagePart, PermissionRequest};
use crate::tasks::TaskId;

/// Maximum number of buffer entries per task before old entries are trimmed.
const MAX_BUFFER_ENTRIES: usize = 500;

/// A single line of activity in the agent activity tab.
#[derive(Debug, Clone)]
pub enum ActivityLine {
    /// A text segment from streaming output (Text, Reasoning, or File parts).
    Text { content: String },
    /// A tool invocation status update.
    ToolActivity { tool: String, status: String },
    /// A banner message from the agent (e.g. session start/complete).
    AgentBanner { message: String },
    /// A pending or resolved permission request from the agent.
    PermissionRequest {
        /// The permission request ID (used to resolve the permission via the API).
        #[allow(dead_code)]
        id: String,
        /// The permission type (e.g. "bash").
        permission: String,
        /// The specific command patterns being requested.
        patterns: Vec<String>,
        /// Patterns already permanently allowed (stored for display/future use).
        #[allow(dead_code)]
        always: Vec<String>,
        /// Whether the user has already responded to this request.
        resolved: bool,
    },
}

/// An entry in a task's per-task activity buffer.
///
/// Message entries are keyed by `message_id` and replaced in place when a new
/// `StreamingUpdate` arrives for the same message (because opencode sends the
/// full accumulated text on every update, not a delta). Tool entries are
/// discrete events that are always appended.
#[derive(Debug, Clone)]
enum BufferEntry {
    /// Streaming message content; replaced on each update for the same id.
    Message {
        id: String,
        lines: Vec<ActivityLine>,
    },
    /// A discrete tool activity event; always appended, never replaced.
    Tool(ActivityLine),
    /// A banner lifecycle event; always appended, never replaced.
    Banner(ActivityLine),
}

/// UI state for Tab 2 (Agent Activity).
///
/// Buffers per-task activity and manages scroll position for the currently
/// displayed task. Each streaming message is stored as a single replaceable
/// slot keyed by `message_id`, so subsequent updates show the current state
/// rather than accumulating duplicate lines.
///
/// Scroll is managed via a follow-tail mode: when `follow_tail` is `true` the
/// viewport automatically tracks the bottom of the buffer. Scrolling up disables
/// follow-tail; scrolling back down to the bottom re-enables it.
pub struct Tab2State {
    /// Per-task ordered buffer of message slots and tool events.
    buffers: HashMap<TaskId, Vec<BufferEntry>>,
    /// Saved scroll offset used when `follow_tail` is false.
    scroll_offset: usize,
    /// When `true`, the viewport tracks the bottom of the buffer (auto-scroll).
    follow_tail: bool,
    /// Maximum scroll offset from the last render, used for scroll clamping.
    ///
    /// Uses `Cell` for interior mutability so `render()` can update it through `&self`.
    last_max_scroll: Cell<usize>,
    /// The task currently displayed in the viewport.
    current_task_id: Option<TaskId>,
    /// Tracks when the prompt was sent for each task, for elapsed-time display.
    prompt_sent_at: HashMap<TaskId, Instant>,
    /// The currently active agent name per task, shown in the status line.
    active_agent: HashMap<TaskId, String>,
    /// Tracks tasks where an agent is actively working (from PromptSent until SessionCompleted/Error).
    ///
    /// Unlike `prompt_sent_at`/`active_agent` which are cleared on first `StreamingUpdate`,
    /// this persists through streaming and is only cleared when the session truly ends.
    thinking_tasks: HashMap<TaskId, (Instant, String)>,
    /// Steering prompt textarea for injecting guidance to the active agent.
    pub steering_input: TextArea<'static>,
    /// Whether the steering textarea currently has keyboard focus.
    pub steering_focused: bool,
    /// The currently pending permission request for the displayed task, if any.
    ///
    /// Set when a `PermissionAsked` message arrives; cleared when the user responds.
    pub pending_permission: Option<(TaskId, PermissionRequest)>,
}

impl Tab2State {
    /// Creates a new `Tab2State` with empty buffers and no task selected.
    pub fn new() -> Self {
        let mut steering_input = TextArea::default();
        steering_input.set_block(
            Block::default()
                .title("Steering Prompt")
                .borders(Borders::ALL),
        );
        Tab2State {
            buffers: HashMap::new(),
            scroll_offset: 0,
            follow_tail: true,
            last_max_scroll: Cell::new(0),
            current_task_id: None,
            prompt_sent_at: HashMap::new(),
            active_agent: HashMap::new(),
            thinking_tasks: HashMap::new(),
            steering_input,
            steering_focused: false,
            pending_permission: None,
        }
    }

    /// Updates the streaming content for a message within `task_id`'s buffer.
    ///
    /// Because opencode sends the **full accumulated text** of a message with
    /// every `message.updated` SSE event, this method replaces the existing
    /// entry for `message_id` rather than appending to it. If no entry exists
    /// yet, a new slot is appended.
    ///
    /// `MessagePart::Unknown` parts are silently skipped.
    /// Automatically scrolls to the bottom if the task is currently displayed.
    pub fn push_streaming(&mut self, task_id: &TaskId, message_id: &str, parts: &[MessagePart]) {
        let new_lines: Vec<ActivityLine> = parts
            .iter()
            .filter_map(|part| match part {
                MessagePart::Text { text } => Some(ActivityLine::Text {
                    content: text.clone(),
                }),
                MessagePart::Reasoning { text } => Some(ActivityLine::Text {
                    content: text.clone(),
                }),
                MessagePart::File { path, .. } => Some(ActivityLine::Text {
                    content: format!("[File: {path}]"),
                }),
                MessagePart::Tool { name, .. } => Some(ActivityLine::ToolActivity {
                    tool: name.clone(),
                    status: "called".to_string(),
                }),
                MessagePart::Unknown => None,
            })
            .collect();

        let buffer = self.buffers.entry(task_id.clone()).or_default();
        // Replace in place if this message_id is already in the buffer.
        if let Some(entry) = buffer
            .iter_mut()
            .find(|e| matches!(e, BufferEntry::Message { id, .. } if id == message_id))
        {
            if let BufferEntry::Message { lines, .. } = entry {
                *lines = new_lines;
            }
        } else {
            buffer.push(BufferEntry::Message {
                id: message_id.to_string(),
                lines: new_lines,
            });
        }

        self.trim_buffer(task_id);
        if self.current_task_id.as_ref() == Some(task_id) {
            self.scroll_to_bottom(task_id);
        }
    }

    /// Appends a discrete `ToolActivity` line for `task_id`.
    ///
    /// Tool activity events are always appended (never deduplicated).
    /// Automatically scrolls to the bottom if the task is currently displayed.
    pub fn push_tool(&mut self, task_id: &TaskId, tool: String, status: String) {
        let buffer = self.buffers.entry(task_id.clone()).or_default();
        buffer.push(BufferEntry::Tool(ActivityLine::ToolActivity {
            tool,
            status,
        }));
        self.trim_buffer(task_id);
        if self.current_task_id.as_ref() == Some(task_id) {
            self.scroll_to_bottom(task_id);
        }
    }

    /// Appends a lifecycle banner line for `task_id`.
    ///
    /// Banner events are always appended (never deduplicated).
    /// Automatically scrolls to the bottom if the task is currently displayed.
    pub fn push_banner(&mut self, task_id: &TaskId, message: String) {
        let buffer = self.buffers.entry(task_id.clone()).or_default();
        buffer.push(BufferEntry::Banner(ActivityLine::AgentBanner { message }));
        self.trim_buffer(task_id);
        if self.current_task_id.as_ref() == Some(task_id) {
            self.scroll_to_bottom(task_id);
        }
    }

    /// Records that a prompt was sent for `task_id` by `agent_name`, starting the elapsed timer.
    ///
    /// Call this after a prompt has been successfully dispatched so the status line
    /// shows the elapsed wait time. Also starts the thinking indicator which persists
    /// until [`clear_thinking`] is called on session completion or error.
    pub fn set_awaiting_response(&mut self, task_id: &TaskId, agent_name: String) {
        self.prompt_sent_at.insert(task_id.clone(), Instant::now());
        self.active_agent
            .insert(task_id.clone(), agent_name.clone());
        self.thinking_tasks
            .insert(task_id.clone(), (Instant::now(), agent_name));
    }

    /// Clears the "awaiting response" state for `task_id`.
    ///
    /// Call this on the first `StreamingUpdate` so the elapsed status line disappears.
    /// Does NOT clear the thinking indicator; use [`clear_thinking`] for that.
    pub fn clear_awaiting(&mut self, task_id: &TaskId) {
        self.prompt_sent_at.remove(task_id);
        self.active_agent.remove(task_id);
    }

    /// Clears the "thinking" indicator for `task_id`.
    ///
    /// Call this when the agent session is truly done (on `SessionCompleted` or `SessionError`)
    /// so the footer indicator disappears. Unlike [`clear_awaiting`], this is not called on
    /// `StreamingUpdate` because the agent is still working during streaming.
    pub fn clear_thinking(&mut self, task_id: &TaskId) {
        self.thinking_tasks.remove(task_id);
    }

    /// Returns task IDs whose awaiting state has exceeded `timeout`.
    ///
    /// A task enters the awaiting state via [`set_awaiting_response`] and exits
    /// via [`clear_awaiting`]. Sessions that fail silently (OpenCode drops the
    /// error without emitting a `session.error` SSE event) will accumulate
    /// elapsed time indefinitely. Callers should call [`clear_awaiting`] and
    /// emit a [`crate::messages::AppMessage::SessionError`] for each returned ID.
    pub fn check_timeouts(&self, timeout: Duration) -> Vec<TaskId> {
        self.prompt_sent_at
            .iter()
            .filter(|(_, sent_at)| sent_at.elapsed() > timeout)
            .map(|(task_id, _)| task_id.clone())
            .collect()
    }

    /// Returns a live elapsed-time status string for `task_id`, if awaiting a response.
    ///
    /// Returns `None` if the task is not currently waiting for a response.
    pub fn elapsed_status(&self, task_id: &TaskId) -> Option<String> {
        match (
            self.prompt_sent_at.get(task_id),
            self.active_agent.get(task_id),
        ) {
            (Some(sent_at), Some(agent)) => {
                let elapsed = sent_at.elapsed().as_secs();
                Some(format!("{}: waiting for response... ({}s)", agent, elapsed))
            }
            _ => None,
        }
    }

    /// Trims the buffer for `task_id` to at most `MAX_BUFFER_ENTRIES`, removing the oldest entries.
    fn trim_buffer(&mut self, task_id: &TaskId) {
        if let Some(buffer) = self.buffers.get_mut(task_id) {
            if buffer.len() > MAX_BUFFER_ENTRIES {
                let excess = buffer.len() - MAX_BUFFER_ENTRIES;
                buffer.drain(..excess);
            }
        }
    }

    /// Removes all buffered lines for `task_id` and resets scroll if it is displayed.
    #[allow(dead_code)]
    pub fn clear(&mut self, task_id: &TaskId) {
        self.buffers.remove(task_id);
        if self.current_task_id.as_ref() == Some(task_id) {
            self.scroll_offset = 0;
        }
    }

    /// Scrolls the viewport up by one visual line.
    ///
    /// If in follow-tail mode, first snaps to the bottom offset recorded from the
    /// last render and then disables follow-tail. If already scrolled, decrements
    /// the saved offset (saturates at 0).
    pub fn scroll_up(&mut self) {
        if self.follow_tail {
            self.scroll_offset = self.last_max_scroll.get().saturating_sub(1);
            self.follow_tail = false;
        } else {
            self.scroll_offset = self.scroll_offset.saturating_sub(1);
        }
    }

    /// Scrolls the viewport down by one visual line.
    ///
    /// If already in follow-tail mode this is a no-op. When the saved offset
    /// reaches or exceeds `last_max_scroll`, follow-tail mode is re-enabled.
    pub fn scroll_down(&mut self) {
        if self.follow_tail {
            return;
        }
        self.scroll_offset += 1;
        if self.scroll_offset >= self.last_max_scroll.get() {
            self.follow_tail = true;
        }
    }

    /// Updates the currently displayed task.
    ///
    /// If the task changes, resets scroll offset and enables follow-tail mode so
    /// the new task's buffer is shown from the bottom.
    pub fn set_displayed_task(&mut self, task_id: Option<&TaskId>) {
        let new_id = task_id.cloned();
        if new_id != self.current_task_id {
            self.current_task_id = new_id;
            self.scroll_offset = 0;
            self.follow_tail = true;
        }
    }

    /// Returns the flattened activity lines for `task_id` in display order.
    ///
    /// Message slots appear in first-seen order; their lines are the **current**
    /// state of that message. Tool events appear interleaved at the position
    /// they were received.
    pub fn lines_for(&self, task_id: &TaskId) -> Vec<ActivityLine> {
        self.buffers
            .get(task_id)
            .map(|entries| {
                entries
                    .iter()
                    .flat_map(|e| match e {
                        BufferEntry::Message { lines, .. } => lines.clone(),
                        BufferEntry::Tool(line) | BufferEntry::Banner(line) => vec![line.clone()],
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Returns a thinking status string for any task where an agent is actively working.
    ///
    /// Scans all tasks in the thinking state and returns a formatted string for the
    /// first one found, or `None` if no agents are active. The thinking state persists
    /// through streaming (unlike the awaiting state) and is only cleared on session
    /// completion or error. Intended for display in the global footer so the user knows
    /// an agent is working regardless of active tab.
    pub fn any_thinking_status(&self) -> Option<String> {
        self.thinking_tasks
            .values()
            .next()
            .map(|(started, agent_name)| {
                let elapsed = started.elapsed().as_secs();
                format!("{} is thinking... ({}s)", agent_name, elapsed)
            })
    }

    /// Enables follow-tail mode so the next render tracks the bottom of the buffer.
    fn scroll_to_bottom(&mut self, _task_id: &TaskId) {
        self.follow_tail = true;
    }

    /// Returns whether an agent is actively working on the given task.
    ///
    /// Based on the `thinking_tasks` map which persists from prompt-sent to session completion.
    pub fn is_agent_active(&self, task_id: &TaskId) -> bool {
        self.thinking_tasks.contains_key(task_id)
    }

    /// Sets the steering textarea to the focused (yellow border) style.
    pub fn set_steering_focused_style(&mut self) {
        self.steering_input.set_block(
            Block::default()
                .title("Steering Prompt")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );
    }

    /// Sets the steering textarea to the unfocused (default border) style.
    pub fn set_steering_unfocused_style(&mut self) {
        self.steering_input.set_block(
            Block::default()
                .title("Steering Prompt")
                .borders(Borders::ALL),
        );
    }

    /// Stores a pending permission request and appends a `PermissionRequest` activity line.
    ///
    /// Call this when a `PermissionAsked` message is received. The pending permission
    /// is stored so keybinding handlers can reference it, and a UI line is added to the
    /// activity buffer for the task.
    pub fn push_permission(&mut self, task_id: TaskId, request: PermissionRequest) {
        let line = ActivityLine::PermissionRequest {
            id: request.id.clone(),
            permission: request.permission.clone(),
            patterns: request.patterns.clone(),
            always: request.always.clone(),
            resolved: false,
        };
        let buffer = self.buffers.entry(task_id.clone()).or_default();
        buffer.push(BufferEntry::Banner(line));
        self.trim_buffer(&task_id);
        if self.current_task_id.as_ref() == Some(&task_id) {
            self.scroll_to_bottom(&task_id);
        }
        self.pending_permission = Some((task_id, request));
    }

    /// Clears the pending permission request and marks the activity line as resolved.
    ///
    /// Call this after the user has responded to a permission request. This updates
    /// the existing `PermissionRequest` activity line to show `resolved: true`.
    pub fn resolve_permission(&mut self, task_id: &TaskId) {
        self.pending_permission = None;
        if let Some(buffer) = self.buffers.get_mut(task_id) {
            // Mark the last unresolved PermissionRequest line as resolved.
            for entry in buffer.iter_mut().rev() {
                if let BufferEntry::Banner(ActivityLine::PermissionRequest {
                    ref mut resolved,
                    ..
                }) = entry
                {
                    if !*resolved {
                        *resolved = true;
                        break;
                    }
                }
            }
        }
    }

    /// Resets the steering textarea to empty with unfocused style.
    pub fn reset_steering(&mut self) {
        let mut ta = TextArea::default();
        ta.set_block(
            Block::default()
                .title("Steering Prompt")
                .borders(Borders::ALL),
        );
        self.steering_input = ta;
        self.steering_focused = false;
    }
}

impl Default for Tab2State {
    fn default() -> Self {
        Tab2State::new()
    }
}

/// Converts a markdown string into styled ratatui [`Line`]s.
///
/// Uses `pulldown-cmark` to parse the input and applies ratatui styles:
/// - Bold (`**text**`) → [`Modifier::BOLD`]
/// - Italic (`*text*`) → [`Modifier::ITALIC`]
/// - Inline code (`` `code` ``) → cyan text
/// - Code blocks → dark gray text, split on newlines
/// - Headings → bold + color (H1=Cyan, H2=Blue, other=LightBlue)
/// - Soft/hard breaks → new [`Line`]
/// - List items → `- ` prefix
///
/// Returns a `Vec<Line<'static>>` suitable for rendering with ratatui's [`Paragraph`].
fn markdown_to_lines(input: &str) -> Vec<Line<'static>> {
    use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();
    let mut bold = false;
    let mut italic = false;
    let mut in_code_block = false;
    let mut heading_color: Option<Color> = None;
    let mut list_depth: usize = 0;

    let parser = Parser::new_ext(input, Options::empty());

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                bold = true;
                heading_color = Some(match level {
                    HeadingLevel::H1 => Color::Cyan,
                    HeadingLevel::H2 => Color::Blue,
                    _ => Color::LightBlue,
                });
            }
            Event::End(TagEnd::Heading(_)) => {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
                lines.push(Line::from(""));
                bold = false;
                heading_color = None;
            }
            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
                lines.push(Line::from(""));
            }
            Event::Start(Tag::Strong) => {
                bold = true;
            }
            Event::End(TagEnd::Strong) => {
                bold = false;
            }
            Event::Start(Tag::Emphasis) => {
                italic = true;
            }
            Event::End(TagEnd::Emphasis) => {
                italic = false;
            }
            Event::Start(Tag::CodeBlock(_)) => {
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
                lines.push(Line::from(""));
                in_code_block = false;
            }
            Event::Code(text) => {
                // Inline code span.
                let style = Style::default().fg(Color::Cyan);
                current_spans.push(Span::styled(format!("`{}`", text.as_ref()), style));
            }
            Event::Start(Tag::List(_)) => {
                list_depth += 1;
            }
            Event::End(TagEnd::List(_)) => {
                list_depth = list_depth.saturating_sub(1);
            }
            Event::Start(Tag::Item) => {
                let indent = "  ".repeat(list_depth.saturating_sub(1));
                current_spans.push(Span::raw(format!("{}- ", indent)));
            }
            Event::End(TagEnd::Item) => {
                if !current_spans.is_empty() {
                    lines.push(Line::from(std::mem::take(&mut current_spans)));
                }
            }
            Event::Text(text) => {
                let mut style = Style::default();
                if bold {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if italic {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if let Some(color) = heading_color {
                    style = style.fg(color);
                }
                if in_code_block {
                    style = style.fg(Color::DarkGray);
                    // Code block text may contain embedded newlines; split into separate lines.
                    for (i, segment) in text.as_ref().split('\n').enumerate() {
                        if i > 0 {
                            lines.push(Line::from(std::mem::take(&mut current_spans)));
                        }
                        if !segment.is_empty() {
                            current_spans.push(Span::styled(segment.to_string(), style));
                        }
                    }
                } else {
                    current_spans.push(Span::styled(text.to_string(), style));
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                lines.push(Line::from(std::mem::take(&mut current_spans)));
            }
            _ => {}
        }
    }

    if !current_spans.is_empty() {
        lines.push(Line::from(current_spans));
    }

    lines
}

/// Computes the total number of visual (wrapped) lines for `lines` at a given `width`.
///
/// Each `Line` whose display width exceeds `width` wraps into multiple visual rows.
/// Empty lines always contribute exactly one visual row.
fn visual_line_count(lines: &[Line], width: u16) -> usize {
    let w = width.max(1) as usize;
    lines
        .iter()
        .map(|line| {
            let lw = line.width();
            if lw == 0 {
                1
            } else {
                lw.div_ceil(w)
            }
        })
        .sum()
}

/// Renders the Agent Activity tab into `area`.
///
/// Displays a placeholder when no task is selected. When a task is selected,
/// renders buffered activity lines with distinct styles per variant. In follow-tail
/// mode the viewport is pinned to the bottom; otherwise `state.scroll_offset` is
/// used (clamped to the computed maximum).
///
/// Updates `state.last_max_scroll` via interior mutability so that `scroll_up` and
/// `scroll_down` can reference the correct visual line count on the next interaction.
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
    let activity_lines = state.lines_for(task_id);
    let mut lines: Vec<Line> = activity_lines
        .into_iter()
        .flat_map(|line| match line {
            ActivityLine::Text { content } => markdown_to_lines(&content),
            ActivityLine::ToolActivity { tool, status } => vec![Line::from(vec![
                Span::styled(
                    format!("[{tool}]"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(status, Style::default().fg(Color::Yellow)),
            ])],
            ActivityLine::AgentBanner { message } => vec![Line::from(Span::styled(
                message,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ))],
            ActivityLine::PermissionRequest {
                permission,
                patterns,
                resolved,
                ..
            } => {
                let mut result = vec![Line::from(vec![
                    Span::styled(
                        "[Permission Request] ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        permission,
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                ])];
                for pattern in &patterns {
                    result.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("cmd: {pattern}"), Style::default().fg(Color::White)),
                    ]));
                }
                if resolved {
                    result.push(Line::from(Span::styled(
                        "  (resolved)",
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    result.push(Line::from(Span::styled(
                        "  [y] approve once | [a] always allow | [n] reject",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                result
            }
        })
        .collect();

    if let Some(status) = state.elapsed_status(task_id) {
        lines.push(Line::from(Span::styled(
            status,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::ITALIC),
        )));
    }

    // Determine whether the steering textarea should be shown.
    let show_steering = state.is_agent_active(task_id);

    // Compute the activity area: when steering is shown, split off 6 rows at the bottom
    // (4 text rows + 2 border rows). When not shown, use the full area.
    let (activity_area, steering_area_opt) = if show_steering {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(6)])
            .split(area);
        (chunks[0], Some(chunks[1]))
    } else {
        (area, None)
    };

    // Compute the effective scroll offset using visual (wrapped) line counts.
    // Subtract 2 from each dimension to account for the surrounding border.
    let content_width = activity_area.width.saturating_sub(2);
    let viewport_height = activity_area.height.saturating_sub(2) as usize;
    let total_visual = visual_line_count(&lines, content_width);
    let max_scroll = total_visual.saturating_sub(viewport_height);
    state.last_max_scroll.set(max_scroll);

    let effective_scroll = if state.follow_tail {
        max_scroll
    } else {
        state.scroll_offset.min(max_scroll)
    };

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((effective_scroll as u16, 0));

    frame.render_widget(paragraph, activity_area);

    if let Some(steering_area) = steering_area_opt {
        frame.render_widget(&state.steering_input, steering_area);
    }
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
            "msg-1",
            &[MessagePart::Text {
                text: "hello".to_string(),
            }],
        );
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1);
        assert!(matches!(lines[0], ActivityLine::Text { ref content } if content == "hello"));
    }

    /// Verifies that a second push with the same message_id replaces (not appends) the lines.
    #[test]
    fn test_push_streaming_replaces_same_message() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_streaming(
            &id,
            "msg-1",
            &[MessagePart::Text {
                text: "Hello".to_string(),
            }],
        );
        state.push_streaming(
            &id,
            "msg-1",
            &[MessagePart::Text {
                text: "Hello, world".to_string(),
            }],
        );
        // Still exactly one line -- the second update replaced the first.
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1, "second push should replace, not append");
        assert!(
            matches!(&lines[0], ActivityLine::Text { content } if content == "Hello, world"),
            "line should contain the latest text, got: {:?}",
            lines[0]
        );
    }

    /// Verifies that pushes with distinct message_ids produce separate lines.
    #[test]
    fn test_push_streaming_distinct_messages_accumulate() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_streaming(
            &id,
            "msg-1",
            &[MessagePart::Text {
                text: "first".to_string(),
            }],
        );
        state.push_streaming(
            &id,
            "msg-2",
            &[MessagePart::Text {
                text: "second".to_string(),
            }],
        );
        let lines = state.lines_for(&id);
        assert_eq!(
            lines.len(),
            2,
            "distinct message ids should each contribute a line"
        );
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
            "msg-1",
            &[MessagePart::Text {
                text: "line".to_string(),
            }],
        );
        assert_eq!(state.lines_for(&id).len(), 1);
        state.clear(&id);
        assert_eq!(state.lines_for(&id).len(), 0);
    }

    #[test]
    fn test_push_banner() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_banner(&id, "--- Intake Agent ---".to_string());
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1);
        assert!(
            matches!(&lines[0], ActivityLine::AgentBanner { message } if message == "--- Intake Agent ---")
        );
    }

    #[test]
    fn test_elapsed_status() {
        let mut state = Tab2State::new();
        let id = task_id();
        // Before setting awaiting, elapsed_status returns None.
        assert!(state.elapsed_status(&id).is_none());
        state.set_awaiting_response(&id, "Intake Agent".to_string());
        let status = state.elapsed_status(&id).expect("should have a status");
        assert!(
            status.contains("Intake Agent"),
            "status should contain agent name: {}",
            status
        );
        assert!(
            status.contains("waiting for response"),
            "status should contain 'waiting for response': {}",
            status
        );
    }

    #[test]
    fn test_clear_awaiting() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_awaiting_response(&id, "Intake Agent".to_string());
        assert!(state.elapsed_status(&id).is_some());
        state.clear_awaiting(&id);
        assert!(state.elapsed_status(&id).is_none());
    }

    #[test]
    fn test_check_timeouts_empty_when_not_awaiting() {
        let state = Tab2State::new();
        let result = state.check_timeouts(Duration::from_secs(1));
        assert!(result.is_empty(), "no timeouts when no task is awaiting");
    }

    #[test]
    fn test_check_timeouts_empty_within_deadline() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_awaiting_response(&id, "Intake Agent".to_string());
        // A very large timeout should not fire immediately after set_awaiting_response.
        let result = state.check_timeouts(Duration::from_secs(3600));
        assert!(
            result.is_empty(),
            "fresh session should not trigger a timeout with a large deadline"
        );
    }

    #[test]
    fn test_check_timeouts_cleared_after_clear_awaiting() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_awaiting_response(&id, "Intake Agent".to_string());
        state.clear_awaiting(&id);
        // Even with a zero timeout, a cleared session should not appear.
        let result = state.check_timeouts(Duration::from_secs(0));
        assert!(
            result.is_empty(),
            "cleared awaiting state should not appear in timeouts"
        );
    }

    /// Verifies that push_streaming trims the buffer when it exceeds MAX_BUFFER_ENTRIES.
    #[test]
    fn test_buffer_trim_on_push_streaming() {
        let mut state = Tab2State::new();
        let id = task_id();
        // Push MAX_BUFFER_ENTRIES + 10 distinct messages (each a separate buffer entry).
        for i in 0..MAX_BUFFER_ENTRIES + 10 {
            state.push_streaming(
                &id,
                &format!("msg-{i}"),
                &[MessagePart::Text {
                    text: format!("line {i}"),
                }],
            );
        }
        let buffer_len = state.buffers.get(&id).map(|b| b.len()).unwrap_or(0);
        assert_eq!(
            buffer_len, MAX_BUFFER_ENTRIES,
            "buffer should be capped at MAX_BUFFER_ENTRIES, got {buffer_len}"
        );
    }

    /// Verifies that push_banner trims the buffer when it exceeds MAX_BUFFER_ENTRIES.
    #[test]
    fn test_buffer_trim_on_push_banner() {
        let mut state = Tab2State::new();
        let id = task_id();
        for i in 0..MAX_BUFFER_ENTRIES + 5 {
            state.push_banner(&id, format!("banner {i}"));
        }
        let buffer_len = state.buffers.get(&id).map(|b| b.len()).unwrap_or(0);
        assert_eq!(
            buffer_len, MAX_BUFFER_ENTRIES,
            "buffer should be capped at MAX_BUFFER_ENTRIES, got {buffer_len}"
        );
    }

    /// Verifies that the most recent entries survive the trim (oldest are removed).
    #[test]
    fn test_buffer_trim_preserves_recent() {
        let mut state = Tab2State::new();
        let id = task_id();
        // Push MAX_BUFFER_ENTRIES + 1 banners.
        for i in 0..MAX_BUFFER_ENTRIES + 1 {
            state.push_banner(&id, format!("banner {i}"));
        }
        // The first banner (index 0) should have been trimmed; the last one survives.
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), MAX_BUFFER_ENTRIES);
        // The first surviving entry should be banner 1 (banner 0 was trimmed).
        assert!(
            matches!(&lines[0], ActivityLine::AgentBanner { message } if message == "banner 1"),
            "oldest entry should be trimmed; first remaining: {:?}",
            lines[0]
        );
        // The last entry should be the most recently pushed banner.
        let last_msg = format!("banner {}", MAX_BUFFER_ENTRIES);
        assert!(
            matches!(lines.last(), Some(ActivityLine::AgentBanner { message }) if message == &last_msg),
            "newest entry should be preserved; last: {:?}",
            lines.last()
        );
    }

    /// Verifies follow-tail semantics: set_displayed_task enables follow_tail,
    /// scroll_up disables it, scroll_down can re-enable it.
    #[test]
    fn test_scroll_bounds() {
        let mut state = Tab2State::new();
        let id = task_id();

        // After set_displayed_task, follow_tail is enabled.
        state.set_displayed_task(Some(&id));
        assert!(
            state.follow_tail,
            "set_displayed_task should enable follow_tail"
        );

        // scroll_up when follow_tail=true disables follow_tail and snaps offset.
        state.scroll_up();
        assert!(!state.follow_tail, "scroll_up should disable follow_tail");

        // scroll_down re-enables follow_tail when offset reaches last_max_scroll (0).
        state.scroll_down();
        assert!(
            state.follow_tail,
            "scroll_down past max should re-enable follow_tail"
        );

        // Push 5 lines with distinct message IDs; push_streaming calls scroll_to_bottom
        // which sets follow_tail = true.
        for i in 0..5 {
            state.push_streaming(
                &id,
                &format!("msg-{i}"),
                &[MessagePart::Text {
                    text: format!("line {i}"),
                }],
            );
        }
        assert!(
            state.follow_tail,
            "push_streaming should keep follow_tail enabled"
        );

        // scroll_down when follow_tail=true is a no-op.
        state.scroll_down();
        assert!(
            state.follow_tail,
            "scroll_down in follow_tail mode should be a no-op"
        );

        // scroll_up disables follow_tail and saves offset based on last_max_scroll.
        // last_max_scroll is 0 since render has never been called in this test.
        state.scroll_up();
        assert!(
            !state.follow_tail,
            "scroll_up should disable follow_tail again"
        );
        // With last_max_scroll=0, sat_sub(1)=0.
        assert_eq!(state.scroll_offset, 0);
    }

    /// Verifies that push_streaming with a new task_id enables follow_tail.
    #[test]
    fn test_follow_tail_enabled_on_push() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_displayed_task(Some(&id));
        // Manually disable follow_tail.
        state.follow_tail = false;

        state.push_streaming(
            &id,
            "msg-1",
            &[MessagePart::Text {
                text: "hello".to_string(),
            }],
        );
        assert!(
            state.follow_tail,
            "push_streaming should re-enable follow_tail for the current task"
        );

        // Same for push_banner.
        state.follow_tail = false;
        state.push_banner(&id, "banner".to_string());
        assert!(
            state.follow_tail,
            "push_banner should re-enable follow_tail for the current task"
        );
    }

    /// Verifies that scroll_up disables follow_tail and records the offset.
    #[test]
    fn test_scroll_up_disables_follow_tail() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_displayed_task(Some(&id));
        // Simulate a prior render having set last_max_scroll to 10.
        state.last_max_scroll.set(10);

        assert!(state.follow_tail);
        state.scroll_up();
        assert!(!state.follow_tail, "scroll_up should disable follow_tail");
        // Offset should be last_max_scroll - 1.
        assert_eq!(
            state.scroll_offset, 9,
            "scroll_up snaps to last_max_scroll - 1"
        );
    }

    /// Verifies that scroll_down re-enables follow_tail when reaching last_max_scroll.
    #[test]
    fn test_scroll_down_reenables_follow_tail() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_displayed_task(Some(&id));
        state.last_max_scroll.set(3);

        // Disable follow_tail and position at offset 1.
        state.scroll_up(); // follow_tail=false, offset=2
        state.scroll_up(); // follow_tail=false, offset=1

        assert!(!state.follow_tail);
        assert_eq!(state.scroll_offset, 1);

        state.scroll_down(); // offset=2
        assert!(!state.follow_tail, "not yet at max");
        assert_eq!(state.scroll_offset, 2);

        state.scroll_down(); // offset=3, 3>=3 -> follow_tail=true
        assert!(
            state.follow_tail,
            "scroll_down past max should re-enable follow_tail"
        );
    }

    /// Verifies that `**bold**` markdown produces a span with the BOLD modifier.
    #[test]
    fn test_markdown_bold_produces_bold_span() {
        let lines = markdown_to_lines("**bold**");
        let has_bold = lines.iter().any(|line| {
            line.spans.iter().any(|span| {
                span.style
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
            })
        });
        assert!(
            has_bold,
            "**bold** should produce a span with BOLD modifier; lines: {lines:?}"
        );
    }

    /// Verifies that two lines separated by a newline produce at least 2 non-empty Lines.
    #[test]
    fn test_markdown_multiline_splits_lines() {
        let lines = markdown_to_lines("line1\nline2");
        let non_empty: Vec<_> = lines.iter().filter(|l| l.width() > 0).collect();
        assert!(
            non_empty.len() >= 2,
            "should produce at least 2 non-empty lines from 'line1\\nline2'; got {} total: {lines:?}",
            lines.len()
        );
    }

    /// Verifies that plain text passes through without losing content.
    #[test]
    fn test_markdown_plain_text_passthrough() {
        let lines = markdown_to_lines("hello world");
        assert!(!lines.is_empty(), "should produce at least one line");
        let found = lines
            .iter()
            .any(|l| l.spans.iter().any(|s| s.content.contains("hello world")));
        assert!(
            found,
            "plain text 'hello world' should appear in output; lines: {lines:?}"
        );
    }

    /// Verifies that `any_thinking_status` returns None when no task is awaiting.
    #[test]
    fn test_any_thinking_status_none_by_default() {
        let state = Tab2State::new();
        assert!(
            state.any_thinking_status().is_none(),
            "any_thinking_status should be None when no task is awaiting"
        );
    }

    /// Verifies that `any_thinking_status` returns a formatted string after set_awaiting_response.
    #[test]
    fn test_any_thinking_status_returns_agent_after_set_awaiting() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_awaiting_response(&id, "Intake Agent".to_string());
        let status = state.any_thinking_status();
        assert!(
            status.is_some(),
            "should have a thinking status after set_awaiting_response"
        );
        let status = status.unwrap();
        assert!(
            status.contains("Intake Agent"),
            "status should contain the agent name: {status}"
        );
        assert!(
            status.contains("is thinking..."),
            "status should contain 'is thinking...': {status}"
        );
    }

    /// Verifies that `any_thinking_status` persists after clear_awaiting (agent still working).
    #[test]
    fn test_any_thinking_status_persists_after_clear_awaiting() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_awaiting_response(&id, "Intake Agent".to_string());
        state.clear_awaiting(&id);
        assert!(
            state.any_thinking_status().is_some(),
            "any_thinking_status should persist after clear_awaiting (agent still streaming)"
        );
    }

    /// Verifies that `any_thinking_status` returns None after clear_thinking.
    #[test]
    fn test_any_thinking_status_clears_after_clear_thinking() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_awaiting_response(&id, "Intake Agent".to_string());
        state.clear_thinking(&id);
        assert!(
            state.any_thinking_status().is_none(),
            "any_thinking_status should be None after clear_thinking"
        );
    }

    /// Verifies that is_agent_active returns true when an agent is active for the task.
    #[test]
    fn test_tab2_steering_textarea_visible_when_active() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.set_awaiting_response(&id, "Intake Agent".to_string());
        assert!(
            state.is_agent_active(&id),
            "is_agent_active should return true when thinking_tasks has an entry"
        );
    }

    /// Verifies that is_agent_active returns false when no agent is active for the task.
    #[test]
    fn test_tab2_steering_textarea_hidden_when_idle() {
        let state = Tab2State::new();
        let id = task_id();
        assert!(
            !state.is_agent_active(&id),
            "is_agent_active should return false with no thinking_tasks entry"
        );
    }

    /// Verifies that push_permission stores the pending permission and adds an activity line.
    #[test]
    fn test_push_permission_adds_activity_line() {
        use crate::opencode::types::PermissionRequest;

        let mut state = Tab2State::new();
        let id = task_id();
        let request = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-abc".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["cargo build".to_string()],
            always: vec![],
        };
        state.push_permission(id.clone(), request.clone());

        // The pending permission should be stored.
        assert!(
            state.pending_permission.is_some(),
            "pending_permission should be set after push_permission"
        );
        let (stored_task_id, stored_req) = state.pending_permission.as_ref().unwrap();
        assert_eq!(*stored_task_id, id);
        assert_eq!(stored_req.id, "perm-1");

        // An activity line should have been added.
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1, "one activity line should be added");
        assert!(
            matches!(
                &lines[0],
                ActivityLine::PermissionRequest { id, permission, resolved, .. }
                    if id == "perm-1" && permission == "bash" && !resolved
            ),
            "activity line should be an unresolved PermissionRequest, got: {:?}",
            lines[0]
        );
    }

    /// Verifies that resolve_permission clears the pending permission and marks the line resolved.
    #[test]
    fn test_resolve_permission_marks_resolved() {
        use crate::opencode::types::PermissionRequest;

        let mut state = Tab2State::new();
        let id = task_id();
        let request = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-abc".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["cargo build".to_string()],
            always: vec![],
        };
        state.push_permission(id.clone(), request);
        assert!(state.pending_permission.is_some());

        state.resolve_permission(&id);

        // Pending permission should be cleared.
        assert!(
            state.pending_permission.is_none(),
            "pending_permission should be None after resolve_permission"
        );

        // The activity line should be marked as resolved.
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1);
        assert!(
            matches!(
                &lines[0],
                ActivityLine::PermissionRequest { resolved, .. } if *resolved
            ),
            "activity line should be resolved, got: {:?}",
            lines[0]
        );
    }

    /// Verifies that visual_line_count correctly counts wrapped lines.
    #[test]
    fn test_visual_line_count_wrapping() {
        // A line of width 10 in a 5-wide viewport wraps to 2 visual rows.
        let lines = vec![
            Line::from("abcdefghij"), // width 10
            Line::from("ab"),         // width 2
            Line::from(""),           // empty -> 1 row
        ];
        // At width=5: ceil(10/5)=2 + ceil(2/5)=1 + 1(empty) = 4
        assert_eq!(visual_line_count(&lines, 5), 4);

        // At width=10: ceil(10/10)=1 + 1 + 1 = 3
        assert_eq!(visual_line_count(&lines, 10), 3);
    }
}
