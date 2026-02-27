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

use crate::opencode::types::{DiffLineKind, DiffStatus, FileDiff, MessagePart, PermissionRequest};
use crate::tasks::TaskId;
use crate::tui::markdown::markdown_to_lines;

/// Maximum number of buffer entries per task before old entries are trimmed.
const MAX_BUFFER_ENTRIES: usize = 500;
/// Maximum number of hunk lines shown in an inline diff preview.
const MAX_DIFF_PREVIEW_LINES: usize = 8;

/// A condensed representation of a single file's diff for inline display.
#[derive(Debug, Clone)]
pub struct DiffFileSummary {
    /// Relative path to the changed file.
    pub path: String,
    /// Whether the file was added, modified, or deleted.
    pub status: DiffStatus,
    /// Number of lines added across all hunks.
    pub lines_added: usize,
    /// Number of lines removed across all hunks.
    pub lines_removed: usize,
    /// Total hunk lines (all kinds) for truncation detection.
    pub total_hunk_lines: usize,
    /// First N hunk lines for compact inline preview.
    pub preview_lines: Vec<(DiffLineKind, String)>,
}

/// A single line of activity in the agent activity tab.
#[derive(Debug, Clone)]
pub enum ActivityLine {
    /// A text segment from streaming output (Text, Reasoning, or File parts).
    Text { content: String },
    /// A tool invocation status update.
    ToolActivity {
        tool: String,
        status: String,
        /// Human-readable summary of the tool's input (file path, command, etc.).
        detail: Option<String>,
    },
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
    /// A compact inline diff summary showing files changed and their hunks.
    DiffSummary {
        /// Per-file summaries with stats and preview lines.
        files: Vec<DiffFileSummary>,
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
    /// An inline diff summary; always appended, never replaced.
    Diff(ActivityLine),
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
    /// Per-task queued steering prompt (max 1).
    ///
    /// Set when the user submits a steering prompt while no session is active.
    /// Drained at the end of each session turn (`SessionCompleted`) and sent to
    /// the just-finished session so the agent processes it before the workflow advances.
    queued_steering_prompts: HashMap<TaskId, String>,
    /// Cumulative token counts per task: `(input_tokens, output_tokens)`.
    ///
    /// Updated whenever a `message.updated` SSE event carries token usage data.
    task_tokens: HashMap<TaskId, (u64, u64)>,
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
            queued_steering_prompts: HashMap::new(),
            task_tokens: HashMap::new(),
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
                    detail: None,
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

    /// Upserts a `ToolActivity` line for `task_id`.
    ///
    /// Lifecycle transitions (`pending` → `running`/`executing` → `completed`) for the
    /// same tool name are collapsed into a single line updated in place, so the activity
    /// view shows one entry per tool call rather than three. The backward scan finds the
    /// most recent entry for the same tool that is still in an earlier stage:
    ///
    /// - `running`/`executing`: updates the last `pending` entry for this tool.
    /// - `completed`: updates the last `pending`/`running`/`executing` entry for this tool.
    /// - All other statuses (including `pending`): always append a new entry.
    ///
    /// When updating in place, the `detail` field is replaced only if the new value is
    /// `Some` (so a detail that arrives with the `running` event overwrites the empty
    /// `pending` detail, but a `completed` event with no detail keeps the running detail).
    ///
    /// Automatically scrolls to the bottom if the task is currently displayed.
    pub fn push_tool(
        &mut self,
        task_id: &TaskId,
        tool: String,
        status: String,
        detail: Option<String>,
    ) {
        let buffer = self.buffers.entry(task_id.clone()).or_default();

        // Determine which earlier statuses this transition can collapse.
        let earlier: &[&str] = match status.as_str() {
            "running" | "executing" => &["pending"],
            "completed" => &["pending", "running", "executing"],
            _ => &[],
        };

        let mut updated = false;
        if !earlier.is_empty() {
            for entry in buffer.iter_mut().rev() {
                if let BufferEntry::Tool(ActivityLine::ToolActivity {
                    tool: ref t,
                    status: ref mut s,
                    detail: ref mut d,
                }) = entry
                {
                    if t == &tool && earlier.contains(&s.as_str()) {
                        *s = status.clone();
                        if detail.is_some() {
                            *d = detail.clone();
                        }
                        updated = true;
                        break;
                    }
                }
            }
        }

        if !updated {
            buffer.push(BufferEntry::Tool(ActivityLine::ToolActivity {
                tool,
                status,
                detail,
            }));
        }

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

    /// Appends an inline diff summary for `task_id`.
    ///
    /// Converts `&[FileDiff]` into a compact `DiffSummary` with per-file stats
    /// and truncated hunk previews. Always appended, never deduplicated.
    /// Automatically scrolls to the bottom if the task is currently displayed.
    pub fn push_diff(&mut self, task_id: &TaskId, diffs: &[FileDiff]) {
        let files: Vec<DiffFileSummary> = diffs
            .iter()
            .map(|fd| {
                let mut lines_added = 0usize;
                let mut lines_removed = 0usize;
                let mut preview_lines = Vec::new();
                let total_hunk_lines: usize = fd.hunks.iter().map(|h| h.lines.len()).sum();

                for hunk in &fd.hunks {
                    for line in &hunk.lines {
                        match line.kind {
                            DiffLineKind::Added => lines_added += 1,
                            DiffLineKind::Removed => lines_removed += 1,
                            DiffLineKind::Context => {}
                        }
                        if preview_lines.len() < MAX_DIFF_PREVIEW_LINES {
                            preview_lines.push((line.kind.clone(), line.content.clone()));
                        }
                    }
                }

                DiffFileSummary {
                    path: fd.path.clone(),
                    status: fd.status.clone(),
                    lines_added,
                    lines_removed,
                    total_hunk_lines,
                    preview_lines,
                }
            })
            .collect();

        let buffer = self.buffers.entry(task_id.clone()).or_default();
        buffer.push(BufferEntry::Diff(ActivityLine::DiffSummary { files }));
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
                        BufferEntry::Tool(line)
                        | BufferEntry::Banner(line)
                        | BufferEntry::Diff(line) => vec![line.clone()],
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
    #[allow(dead_code)]
    pub fn is_agent_active(&self, task_id: &TaskId) -> bool {
        self.thinking_tasks.contains_key(task_id)
    }

    /// Updates the cumulative token counts for a task.
    ///
    /// Replaces the stored `(input, output)` pair with the latest values reported
    /// by the OpenCode `message.updated` SSE event.
    pub fn update_tokens(&mut self, task_id: &TaskId, input_tokens: u64, output_tokens: u64) {
        self.task_tokens
            .insert(task_id.clone(), (input_tokens, output_tokens));
    }

    /// Returns the cumulative `(input_tokens, output_tokens)` for a task, if any have been reported.
    pub fn get_tokens(&self, task_id: &TaskId) -> Option<(u64, u64)> {
        self.task_tokens.get(task_id).copied()
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

    /// Stores `text` as the pending steering prompt for `task_id`, replacing any
    /// existing queued prompt (queue size is capped at 1).
    pub fn queue_prompt(&mut self, task_id: TaskId, text: String) {
        self.queued_steering_prompts.insert(task_id, text);
    }

    /// Removes and returns the queued steering prompt for `task_id`, if any.
    pub fn take_queued_prompt(&mut self, task_id: &TaskId) -> Option<String> {
        self.queued_steering_prompts.remove(task_id)
    }

    /// Returns `true` if there is a queued steering prompt for `task_id`.
    #[allow(dead_code)]
    pub fn has_queued_prompt(&self, task_id: &TaskId) -> bool {
        self.queued_steering_prompts.contains_key(task_id)
    }
}

impl Default for Tab2State {
    fn default() -> Self {
        Tab2State::new()
    }
}

/// Unescapes common JSON string escape sequences so streaming agent text is readable.
///
/// Agent responses are formatted as JSON, so their streaming text contains literal
/// escape sequences (`\n`, `\t`, `\"`, `\\`) instead of the actual characters.
/// Replacing these before rendering produces proper line breaks and readable output.
fn unescape_streaming_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.peek() {
                Some(&'n') => {
                    chars.next();
                    out.push('\n');
                }
                Some(&'t') => {
                    chars.next();
                    out.push('\t');
                }
                Some(&'"') => {
                    chars.next();
                    out.push('"');
                }
                Some(&'\\') => {
                    chars.next();
                    out.push('\\');
                }
                _ => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
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
            ActivityLine::Text { content } => markdown_to_lines(&unescape_streaming_text(&content)),
            ActivityLine::ToolActivity {
                tool,
                status,
                detail,
            } => {
                let status_style = match status.as_str() {
                    "pending" => Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                    "executing" | "running" => Style::default().fg(Color::Cyan),
                    "completed" => Style::default().fg(Color::DarkGray),
                    _ => Style::default().fg(Color::Yellow),
                };
                let mut spans = vec![Span::styled(
                    format!("[{tool}]"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )];
                if let Some(d) = detail {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(d, Style::default().fg(Color::DarkGray)));
                }
                spans.push(Span::raw("  "));
                spans.push(Span::styled(status, status_style));
                vec![Line::from(spans)]
            }
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
            ActivityLine::DiffSummary { files } => {
                let mut result: Vec<Line<'static>> = Vec::new();

                // Section header
                result.push(Line::from(Span::styled(
                    "--- Diff ---",
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                )));

                for file in files {
                    let status_label = match file.status {
                        DiffStatus::Added => "[+added]",
                        DiffStatus::Modified => "[modified]",
                        DiffStatus::Deleted => "[-deleted]",
                    };
                    result.push(Line::from(vec![
                        Span::styled(
                            format!("  {} ", file.path),
                            Style::default()
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("{} ", status_label),
                            Style::default().fg(Color::Cyan),
                        ),
                        Span::styled(
                            format!("+{}", file.lines_added),
                            Style::default().fg(Color::Green),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            format!("-{}", file.lines_removed),
                            Style::default().fg(Color::Red),
                        ),
                    ]));

                    for (kind, content) in &file.preview_lines {
                        let (prefix, color) = match kind {
                            DiffLineKind::Added => ("+", Color::Green),
                            DiffLineKind::Removed => ("-", Color::Red),
                            DiffLineKind::Context => (" ", Color::DarkGray),
                        };
                        result.push(Line::from(Span::styled(
                            format!("    {}{}", prefix, content),
                            Style::default().fg(color),
                        )));
                    }

                    if file.preview_lines.len() < file.total_hunk_lines {
                        result.push(Line::from(Span::styled(
                            format!(
                                "    ... ({} more lines)",
                                file.total_hunk_lines - file.preview_lines.len()
                            ),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                }

                result.push(Line::from(""));
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

    // Only allocate space for the steering textarea when it is focused.
    let (activity_area, steering_area) = if state.steering_focused {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(6)])
            .split(area);
        (chunks[0], Some(chunks[1]))
    } else {
        (area, None)
    };

    // line_count() adjusts for vertical borders but NOT horizontal ones, so we
    // call it on a blockless paragraph with the inner content width to match
    // what the renderer actually uses for word-wrapping.
    let inner_width = activity_area.width.saturating_sub(2);
    let viewport_height = activity_area.height.saturating_sub(2) as usize;
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let total_visual = paragraph.line_count(inner_width);
    let paragraph = paragraph.block(block);
    let max_scroll = total_visual.saturating_sub(viewport_height);
    state.last_max_scroll.set(max_scroll);

    let effective_scroll = if state.follow_tail {
        max_scroll
    } else {
        state.scroll_offset.min(max_scroll)
    };

    let paragraph = paragraph.scroll((effective_scroll as u16, 0));
    frame.render_widget(paragraph, activity_area);
    if let Some(sa) = steering_area {
        frame.render_widget(&state.steering_input, sa);
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
        state.push_tool(&id, "bash".to_string(), "running".to_string(), None);
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1);
        assert!(
            matches!(&lines[0], ActivityLine::ToolActivity { tool, status, .. } if tool == "bash" && status == "running")
        );
    }

    /// pending then running for the same tool collapses to a single entry.
    #[test]
    fn test_push_tool_collapses_pending_to_running() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_tool(&id, "bash".to_string(), "pending".to_string(), None);
        state.push_tool(
            &id,
            "bash".to_string(),
            "running".to_string(),
            Some("cargo build".to_string()),
        );
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1, "pending+running should collapse to 1 entry");
        assert!(
            matches!(&lines[0], ActivityLine::ToolActivity { tool, status, detail }
                if tool == "bash" && status == "running" && detail.as_deref() == Some("cargo build")),
            "entry should show running status with detail; got: {:?}",
            lines[0]
        );
    }

    /// pending -> running -> completed collapses to a single entry.
    #[test]
    fn test_push_tool_collapses_full_lifecycle() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_tool(&id, "write".to_string(), "pending".to_string(), None);
        state.push_tool(
            &id,
            "write".to_string(),
            "running".to_string(),
            Some("src/main.rs".to_string()),
        );
        state.push_tool(&id, "write".to_string(), "completed".to_string(), None);
        let lines = state.lines_for(&id);
        assert_eq!(
            lines.len(),
            1,
            "pending+running+completed should collapse to 1 entry"
        );
        assert!(
            matches!(&lines[0], ActivityLine::ToolActivity { tool, status, detail }
                if tool == "write" && status == "completed" && detail.as_deref() == Some("src/main.rs")),
            "entry should be completed with detail from running stage; got: {:?}",
            lines[0]
        );
    }

    /// completed with no detail preserves the detail set during running.
    #[test]
    fn test_push_tool_preserves_detail_when_completed_has_none() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_tool(
            &id,
            "read".to_string(),
            "running".to_string(),
            Some("src/lib.rs".to_string()),
        );
        state.push_tool(&id, "read".to_string(), "completed".to_string(), None);
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1);
        assert!(
            matches!(&lines[0], ActivityLine::ToolActivity { status, detail, .. }
                if status == "completed" && detail.as_deref() == Some("src/lib.rs")),
            "detail from running stage should be preserved when completed has None; got: {:?}",
            lines[0]
        );
    }

    /// Different tools each get their own entry; they are not cross-collapsed.
    #[test]
    fn test_push_tool_separate_tools_not_collapsed() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.push_tool(&id, "bash".to_string(), "pending".to_string(), None);
        state.push_tool(&id, "write".to_string(), "pending".to_string(), None);
        state.push_tool(&id, "bash".to_string(), "completed".to_string(), None);
        state.push_tool(&id, "write".to_string(), "completed".to_string(), None);
        let lines = state.lines_for(&id);
        assert_eq!(
            lines.len(),
            2,
            "two distinct tools should produce 2 collapsed entries"
        );
        assert!(
            matches!(&lines[0], ActivityLine::ToolActivity { tool, status, .. }
                if tool == "bash" && status == "completed"),
            "first entry should be completed bash; got: {:?}",
            lines[0]
        );
        assert!(
            matches!(&lines[1], ActivityLine::ToolActivity { tool, status, .. }
                if tool == "write" && status == "completed"),
            "second entry should be completed write; got: {:?}",
            lines[1]
        );
    }

    /// Two sequential calls to the same tool produce two separate entries.
    #[test]
    fn test_push_tool_sequential_same_tool_produces_two_entries() {
        let mut state = Tab2State::new();
        let id = task_id();
        // First bash call: full lifecycle.
        state.push_tool(&id, "bash".to_string(), "pending".to_string(), None);
        state.push_tool(
            &id,
            "bash".to_string(),
            "running".to_string(),
            Some("cargo fmt".to_string()),
        );
        state.push_tool(&id, "bash".to_string(), "completed".to_string(), None);
        // Second bash call: pending only (still in progress).
        state.push_tool(
            &id,
            "bash".to_string(),
            "pending".to_string(),
            Some("cargo build".to_string()),
        );
        let lines = state.lines_for(&id);
        assert_eq!(
            lines.len(),
            2,
            "two sequential bash calls should produce 2 entries"
        );
        assert!(
            matches!(&lines[0], ActivityLine::ToolActivity { tool, status, .. }
                if tool == "bash" && status == "completed"),
            "first entry should be the completed bash call; got: {:?}",
            lines[0]
        );
        assert!(
            matches!(&lines[1], ActivityLine::ToolActivity { tool, status, detail }
                if tool == "bash" && status == "pending" && detail.as_deref() == Some("cargo build")),
            "second entry should be the new pending bash call; got: {:?}",
            lines[1]
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
    fn test_is_agent_active_true_when_thinking() {
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
    fn test_is_agent_active_false_when_idle() {
        let state = Tab2State::new();
        let id = task_id();
        assert!(
            !state.is_agent_active(&id),
            "is_agent_active should return false with no thinking_tasks entry"
        );
    }

    /// Verifies that queue_prompt stores a prompt and take_queued_prompt drains it.
    #[test]
    fn test_queue_prompt_stores_and_dequeues() {
        let mut state = Tab2State::new();
        let id = task_id();
        assert!(!state.has_queued_prompt(&id), "no prompt queued initially");
        state.queue_prompt(id.clone(), "steer me".to_string());
        assert!(state.has_queued_prompt(&id), "prompt should be queued");
        let dequeued = state.take_queued_prompt(&id);
        assert_eq!(dequeued, Some("steer me".to_string()));
        assert!(
            !state.has_queued_prompt(&id),
            "queue should be empty after take"
        );
    }

    /// Verifies that a second queue_prompt replaces the first (max queue size of 1).
    #[test]
    fn test_queue_prompt_replaces_existing() {
        let mut state = Tab2State::new();
        let id = task_id();
        state.queue_prompt(id.clone(), "first prompt".to_string());
        state.queue_prompt(id.clone(), "second prompt".to_string());
        let dequeued = state.take_queued_prompt(&id);
        assert_eq!(
            dequeued,
            Some("second prompt".to_string()),
            "second queue_prompt should replace the first"
        );
    }

    /// Verifies that take_queued_prompt returns None when nothing is queued.
    #[test]
    fn test_take_queued_prompt_empty() {
        let mut state = Tab2State::new();
        let id = task_id();
        assert_eq!(state.take_queued_prompt(&id), None);
    }

    /// Verifies that queued prompts are isolated per task.
    #[test]
    fn test_queue_prompt_isolated_per_task() {
        let mut state = Tab2State::new();
        let id1 = TaskId::from_path("tasks/1.1.md");
        let id2 = TaskId::from_path("tasks/1.2.md");
        state.queue_prompt(id1.clone(), "for task 1".to_string());
        assert!(
            !state.has_queued_prompt(&id2),
            "task 2 should have no queue"
        );
        assert_eq!(
            state.take_queued_prompt(&id2),
            None,
            "take on task 2 should return None"
        );
        assert_eq!(
            state.take_queued_prompt(&id1),
            Some("for task 1".to_string())
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

    // --- push_diff tests ---

    fn make_file_diff(
        path: &str,
        status: DiffStatus,
        lines: Vec<(DiffLineKind, &str)>,
    ) -> FileDiff {
        use crate::opencode::types::{DiffHunk, DiffLine};
        let hunk_lines = lines
            .into_iter()
            .map(|(kind, content)| DiffLine {
                kind,
                content: content.to_string(),
            })
            .collect();
        FileDiff {
            path: path.to_string(),
            status,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                lines: hunk_lines,
            }],
        }
    }

    /// Verifies that push_diff creates a buffer entry and lines_for returns one DiffSummary.
    #[test]
    fn test_push_diff_creates_buffer_entry() {
        use crate::opencode::types::DiffStatus;

        let mut state = Tab2State::new();
        let id = task_id();
        let diff = make_file_diff(
            "src/main.rs",
            DiffStatus::Modified,
            vec![(DiffLineKind::Added, "fn main() {}")],
        );
        state.push_diff(&id, &[diff]);
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1, "should have exactly one DiffSummary line");
        assert!(
            matches!(&lines[0], ActivityLine::DiffSummary { files } if files.len() == 1),
            "DiffSummary should contain 1 file; got: {:?}",
            lines[0]
        );
    }

    /// Verifies that push_diff correctly computes stats for added/removed lines.
    #[test]
    fn test_push_diff_summary_stats() {
        use crate::opencode::types::DiffStatus;

        let mut state = Tab2State::new();
        let id = task_id();
        let diff = make_file_diff(
            "src/lib.rs",
            DiffStatus::Modified,
            vec![
                (DiffLineKind::Context, "context line"),
                (DiffLineKind::Added, "added line 1"),
                (DiffLineKind::Added, "added line 2"),
                (DiffLineKind::Removed, "removed line"),
            ],
        );
        state.push_diff(&id, &[diff]);
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1);
        if let ActivityLine::DiffSummary { files } = &lines[0] {
            let f = &files[0];
            assert_eq!(f.path, "src/lib.rs");
            assert!(matches!(f.status, DiffStatus::Modified));
            assert_eq!(f.lines_added, 2, "lines_added should be 2");
            assert_eq!(f.lines_removed, 1, "lines_removed should be 1");
            assert_eq!(f.total_hunk_lines, 4, "total_hunk_lines should be 4");
        } else {
            panic!("expected DiffSummary, got: {:?}", lines[0]);
        }
    }

    /// Verifies that push_diff truncates preview_lines to MAX_DIFF_PREVIEW_LINES.
    #[test]
    fn test_push_diff_truncates_preview() {
        use crate::opencode::types::DiffStatus;

        let mut state = Tab2State::new();
        let id = task_id();
        // 20 added lines -- well above MAX_DIFF_PREVIEW_LINES (8).
        let hunk_lines: Vec<(DiffLineKind, &str)> =
            (0..20).map(|_| (DiffLineKind::Added, "line")).collect();
        let diff = make_file_diff("src/big.rs", DiffStatus::Modified, hunk_lines);
        state.push_diff(&id, &[diff]);
        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 1);
        if let ActivityLine::DiffSummary { files } = &lines[0] {
            let f = &files[0];
            assert_eq!(
                f.preview_lines.len(),
                MAX_DIFF_PREVIEW_LINES,
                "preview should be capped at MAX_DIFF_PREVIEW_LINES"
            );
            assert_eq!(f.total_hunk_lines, 20, "total_hunk_lines should be 20");
        } else {
            panic!("expected DiffSummary, got: {:?}", lines[0]);
        }
    }

    /// Verifies that push_diff interleaves correctly with other entry types.
    #[test]
    fn test_push_diff_interleaves_with_other_entries() {
        use crate::opencode::types::DiffStatus;

        let mut state = Tab2State::new();
        let id = task_id();
        state.push_banner(&id, "session started".to_string());
        state.push_streaming(
            &id,
            "msg-1",
            &[MessagePart::Text {
                text: "working...".to_string(),
            }],
        );
        let diff = make_file_diff(
            "src/foo.rs",
            DiffStatus::Added,
            vec![(DiffLineKind::Added, "new file content")],
        );
        state.push_diff(&id, &[diff]);
        state.push_tool(&id, "bash".to_string(), "done".to_string(), None);

        let lines = state.lines_for(&id);
        assert_eq!(lines.len(), 4, "should have 4 lines in order");
        assert!(
            matches!(&lines[0], ActivityLine::AgentBanner { .. }),
            "first should be banner"
        );
        assert!(
            matches!(&lines[1], ActivityLine::Text { .. }),
            "second should be text"
        );
        assert!(
            matches!(&lines[2], ActivityLine::DiffSummary { .. }),
            "third should be diff summary"
        );
        assert!(
            matches!(&lines[3], ActivityLine::ToolActivity { .. }),
            "fourth should be tool activity"
        );
    }

    /// Verifies that update_tokens stores values and get_tokens retrieves them.
    #[test]
    fn test_update_and_get_tokens() {
        let mut state = Tab2State::new();
        let task_id = TaskId::from_path("tasks/1.1.md");

        assert!(
            state.get_tokens(&task_id).is_none(),
            "tokens should be None before any update"
        );

        state.update_tokens(&task_id, 1000, 250);
        assert_eq!(
            state.get_tokens(&task_id),
            Some((1000, 250)),
            "tokens should match after update"
        );

        // Subsequent update replaces previous values.
        state.update_tokens(&task_id, 2000, 500);
        assert_eq!(
            state.get_tokens(&task_id),
            Some((2000, 500)),
            "tokens should be replaced by second update"
        );
    }

    /// Verifies that token counts are per-task and do not bleed between tasks.
    #[test]
    fn test_tokens_are_per_task() {
        let mut state = Tab2State::new();
        let task1 = TaskId::from_path("tasks/1.1.md");
        let task2 = TaskId::from_path("tasks/1.2.md");

        state.update_tokens(&task1, 100, 50);
        assert_eq!(state.get_tokens(&task1), Some((100, 50)));
        assert!(
            state.get_tokens(&task2).is_none(),
            "task2 should have no tokens"
        );
    }

    // --- unescape_streaming_text tests ---

    #[test]
    fn test_unescape_newline() {
        assert_eq!(unescape_streaming_text("line1\\nline2"), "line1\nline2");
    }

    #[test]
    fn test_unescape_tab() {
        assert_eq!(unescape_streaming_text("a\\tb"), "a\tb");
    }

    #[test]
    fn test_unescape_quote() {
        assert_eq!(unescape_streaming_text("say \\\"hi\\\""), "say \"hi\"");
    }

    #[test]
    fn test_unescape_backslash() {
        assert_eq!(unescape_streaming_text("a\\\\b"), "a\\b");
    }

    #[test]
    fn test_unescape_plain_text_unchanged() {
        let s = "hello world, no escapes here.";
        assert_eq!(unescape_streaming_text(s), s);
    }

    #[test]
    fn test_unescape_json_response_fragment() {
        // Raw string: \n here is literally backslash + n (the JSON escape sequence).
        let input = r#"{"summary":"done\nstep two","updates":null}"#;
        let result = unescape_streaming_text(input);
        // After unescaping, the two-char \n should be a real newline.
        assert!(
            result.contains("done\nstep two"),
            "\\n should become newline"
        );
        // The literal backslash-n sequence should no longer appear.
        assert!(
            !result.contains("\\n"),
            "literal \\n should be gone after unescaping"
        );
    }

    /// Verifies that render() does not panic when steering is unfocused and area is small.
    ///
    /// When `steering_focused` is false the activity area should consume the full rect,
    /// so no 6-row steering section is subtracted even on a very constrained terminal.
    #[test]
    fn test_render_unfocused_steering_uses_full_area() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut state = Tab2State::new();
        state.steering_focused = false;
        let id = task_id();
        state.set_displayed_task(Some(&id));
        state.push_streaming(
            &id,
            "msg-1",
            &[MessagePart::Text {
                text: "hello".to_string(),
            }],
        );

        // A small area (8 rows) that would be completely consumed by the 6-row steering
        // block if it were always rendered -- this must not panic.
        let backend = TestBackend::new(40, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), Some(&id), &state);
            })
            .unwrap();
    }

    /// Verifies that render() does not panic when steering is focused and area is adequate.
    ///
    /// When `steering_focused` is true the layout splits into activity + 6-row steering.
    #[test]
    fn test_render_focused_steering_splits_area() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut state = Tab2State::new();
        state.steering_focused = true;
        state.set_steering_focused_style();
        let id = task_id();
        state.set_displayed_task(Some(&id));
        state.push_streaming(
            &id,
            "msg-1",
            &[MessagePart::Text {
                text: "hello".to_string(),
            }],
        );

        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), Some(&id), &state);
            })
            .unwrap();
    }

    /// Verifies that line_count called with inner_width (no block) gives a higher visual
    /// line count than the old approach of calling line_count with outer_width on a paragraph
    /// that has a bordered block attached.
    ///
    /// Four lines each `inner_width + 1` chars long wrap to 2 lines at inner_width but fit
    /// without wrapping at outer_width. The new count (8) exceeds the old buggy count (6),
    /// which means max_scroll is correctly estimated rather than underestimated.
    #[test]
    fn test_scroll_line_count_uses_inner_width() {
        let outer_width = 20u16;
        let inner_width = outer_width.saturating_sub(2);
        let viewport_height = 5usize;

        // Each line is exactly inner_width + 1 chars: fits in outer_width, wraps at inner_width.
        let content: String = "a".repeat((inner_width + 1) as usize);
        let lines: Vec<Line> = (0..4).map(|_| Line::from(content.clone())).collect();

        // New (correct) approach: blockless paragraph with inner_width.
        let paragraph_no_block = Paragraph::new(lines.clone()).wrap(Wrap { trim: false });
        let total_visual_new = paragraph_no_block.line_count(inner_width);

        // Old (buggy) approach: paragraph with block at outer_width.
        let block = Block::default().borders(Borders::ALL);
        let paragraph_with_block = Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false });
        let total_visual_old = paragraph_with_block.line_count(outer_width);

        // New: 4 lines x 2 visual rows each = 8.
        assert_eq!(
            total_visual_new, 8,
            "each line should wrap to 2 at inner_width"
        );
        // Old: 4 lines (no wrap at outer_width) + 2 border rows = 6.
        assert_eq!(
            total_visual_old, 6,
            "old count: no wrapping + 2 border rows"
        );

        // max_scroll from new code allows scrolling further than old code.
        let max_scroll_new = total_visual_new.saturating_sub(viewport_height);
        let max_scroll_old = total_visual_old.saturating_sub(viewport_height);
        assert!(
            max_scroll_new > max_scroll_old,
            "new max_scroll ({}) should exceed old max_scroll ({})",
            max_scroll_new,
            max_scroll_old
        );
    }
}
