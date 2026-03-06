//! Research tab: global AI chat scratchpad.
//!
//! Provides a persistent chat interface backed by a lazily-created backend session.
//! The session is created on the first prompt submission and reused for all follow-ups.
//! State is global -- it does not change when the user navigates between tasks.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_textarea::TextArea;

use crate::tui::markdown::markdown_to_lines;

/// The role of a chat message in the research conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatRole {
    /// A message submitted by the user.
    User,
    /// A response from the AI assistant.
    Assistant,
    /// A system status or error banner.
    System,
}

/// A single message in the research chat history.
#[derive(Debug, Clone)]
pub struct ChatMessage {
    /// The role of the sender.
    pub role: ChatRole,
    /// Message content text.
    pub content: String,
    /// Backend message ID used to upsert streaming responses.
    ///
    /// Only set for `Assistant` messages; `None` for `User` and `System`.
    pub message_id: Option<String>,
}

/// State for the Research tab.
///
/// Holds the full conversation history, backend session handle, and input textarea.
/// Persists for the lifetime of the application regardless of task navigation.
pub struct ResearchTabState {
    /// Accumulated chat messages (user, assistant, system).
    pub messages: Vec<ChatMessage>,
    /// The backend session ID once created; `None` before first prompt.
    pub session_id: Option<String>,
    /// Guards against double-creation: `true` while `create_session` is in flight.
    pub session_creating: bool,
    /// `true` while waiting for a streaming response; drives the "Thinking..." indicator.
    pub awaiting_response: bool,
    /// Prompt input textarea (unfocused by default).
    pub prompt_input: TextArea<'static>,
    /// Whether the prompt input has keyboard focus.
    pub prompt_focused: bool,
    /// Number of lines scrolled up from the bottom of the chat history.
    pub scroll_offset: usize,
    /// When `true`, the view auto-scrolls to the latest message.
    pub follow_tail: bool,
}

impl ResearchTabState {
    /// Creates a new empty `ResearchTabState`.
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
            session_id: None,
            session_creating: false,
            awaiting_response: false,
            prompt_input: TextArea::default(),
            prompt_focused: false,
            scroll_offset: 0,
            follow_tail: true,
        }
    }

    /// Appends a user message to the chat history.
    pub fn push_user_message(&mut self, text: String) {
        self.messages.push(ChatMessage {
            role: ChatRole::User,
            content: text,
            message_id: None,
        });
        if self.follow_tail {
            self.scroll_offset = 0;
        }
    }

    /// Appends a system status or error banner to the chat history.
    pub fn push_system_message(&mut self, text: String) {
        self.messages.push(ChatMessage {
            role: ChatRole::System,
            content: text,
            message_id: None,
        });
        if self.follow_tail {
            self.scroll_offset = 0;
        }
    }

    /// Upserts a streaming assistant message by `message_id`.
    ///
    /// If a message with the same `message_id` already exists, its content is replaced.
    /// Otherwise a new assistant message is appended.
    pub fn push_streaming(&mut self, message_id: &str, content: String) {
        if let Some(msg) = self
            .messages
            .iter_mut()
            .rev()
            .find(|m| m.message_id.as_deref() == Some(message_id))
        {
            msg.content = content;
        } else {
            self.messages.push(ChatMessage {
                role: ChatRole::Assistant,
                content,
                message_id: Some(message_id.to_string()),
            });
        }
        if self.follow_tail {
            self.scroll_offset = 0;
        }
    }

    /// Clears the `awaiting_response` flag after the session completes.
    pub fn finalize_response(&mut self) {
        self.awaiting_response = false;
    }

    /// Appends a system error message and clears `awaiting_response`.
    pub fn push_error(&mut self, error: String) {
        self.awaiting_response = false;
        self.messages.push(ChatMessage {
            role: ChatRole::System,
            content: format!("Error: {}", error),
            message_id: None,
        });
        if self.follow_tail {
            self.scroll_offset = 0;
        }
    }

    /// Scrolls the chat history up by one line, disabling auto-scroll.
    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_add(1);
        self.follow_tail = false;
    }

    /// Scrolls the chat history down by one line.
    ///
    /// When `scroll_offset` reaches 0, re-enables auto-scroll.
    pub fn scroll_down(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
        if self.scroll_offset == 0 {
            self.follow_tail = true;
        }
    }
}

impl Default for ResearchTabState {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders all chat messages into styled ratatui lines.
///
/// `[You]` prefix is yellow, `[Assistant]` is cyan, `[System]` is green.
/// Assistant content is rendered via `markdown_to_lines` for rich formatting.
fn render_messages(messages: &[ChatMessage], awaiting: bool) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    for msg in messages {
        match msg.role {
            ChatRole::User => {
                lines.push(Line::from(vec![
                    Span::styled(
                        "[You] ",
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(msg.content.clone()),
                ]));
                lines.push(Line::from(""));
            }
            ChatRole::System => {
                lines.push(Line::from(vec![Span::styled(
                    msg.content.clone(),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::ITALIC),
                )]));
                lines.push(Line::from(""));
            }
            ChatRole::Assistant => {
                lines.push(Line::from(vec![Span::styled(
                    "[Assistant] ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )]));
                let md_lines = markdown_to_lines(&msg.content);
                lines.extend(md_lines);
                lines.push(Line::from(""));
            }
        }
    }

    if awaiting {
        lines.push(Line::from(vec![Span::styled(
            "Thinking...",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )]));
    }

    lines
}

/// Renders the Research tab into `area`.
///
/// Layout: vertical split between chat history (fills remaining space) and
/// a prompt input (3 rows unfocused, 6 rows focused) at the bottom.
pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, state: &ResearchTabState) {
    let prompt_height: u16 = if state.prompt_focused { 6 } else { 3 };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(prompt_height)])
        .split(area);

    let chat_area = chunks[0];
    let input_area = chunks[1];

    // Render chat history.
    let lines = render_messages(&state.messages, state.awaiting_response);
    let total_lines = lines.len() as u16;
    let visible_rows = chat_area.height.saturating_sub(2); // subtract borders

    // Compute scroll: auto-tail pins to bottom; manual scroll uses offset.
    let max_scroll = total_lines.saturating_sub(visible_rows);
    let scroll = if state.follow_tail {
        max_scroll
    } else {
        max_scroll.saturating_sub(state.scroll_offset as u16)
    };

    let chat_block = Block::default()
        .borders(Borders::ALL)
        .title(" Research Chat ");
    let chat_para = Paragraph::new(lines).block(chat_block).scroll((scroll, 0));
    frame.render_widget(chat_para, chat_area);

    // Render prompt input.
    let prompt_title = if state.prompt_focused {
        " Prompt (Enter to send, Esc to exit) "
    } else {
        " Prompt [p to focus] "
    };
    let border_style = if state.prompt_focused {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default()
    };
    let mut prompt_input = state.prompt_input.clone();
    prompt_input.set_block(
        Block::default()
            .borders(Borders::ALL)
            .title(prompt_title)
            .border_style(border_style),
    );
    frame.render_widget(&prompt_input, input_area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_state_defaults() {
        let state = ResearchTabState::new();
        assert!(state.messages.is_empty());
        assert!(state.session_id.is_none());
        assert!(!state.session_creating);
        assert!(!state.awaiting_response);
        assert!(!state.prompt_focused);
        assert_eq!(state.scroll_offset, 0);
        assert!(state.follow_tail);
    }

    #[test]
    fn test_push_user_message() {
        let mut state = ResearchTabState::new();
        state.push_user_message("Hello".to_string());
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].role, ChatRole::User);
        assert_eq!(state.messages[0].content, "Hello");
        assert!(state.messages[0].message_id.is_none());
    }

    #[test]
    fn test_push_streaming_creates_new() {
        let mut state = ResearchTabState::new();
        state.push_streaming("msg-1", "Hello world".to_string());
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].role, ChatRole::Assistant);
        assert_eq!(state.messages[0].content, "Hello world");
        assert_eq!(state.messages[0].message_id, Some("msg-1".to_string()));
    }

    #[test]
    fn test_push_streaming_upserts_same_id() {
        let mut state = ResearchTabState::new();
        state.push_streaming("msg-1", "Part 1".to_string());
        state.push_streaming("msg-1", "Part 1 updated".to_string());
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].content, "Part 1 updated");
    }

    #[test]
    fn test_push_streaming_different_id_appends() {
        let mut state = ResearchTabState::new();
        state.push_streaming("msg-1", "First".to_string());
        state.push_streaming("msg-2", "Second".to_string());
        assert_eq!(state.messages.len(), 2);
        assert_eq!(state.messages[0].content, "First");
        assert_eq!(state.messages[1].content, "Second");
    }

    #[test]
    fn test_finalize_response() {
        let mut state = ResearchTabState::new();
        state.awaiting_response = true;
        state.finalize_response();
        assert!(!state.awaiting_response);
    }

    #[test]
    fn test_push_error() {
        let mut state = ResearchTabState::new();
        state.awaiting_response = true;
        state.push_error("connection refused".to_string());
        assert!(!state.awaiting_response);
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.messages[0].role, ChatRole::System);
        assert!(state.messages[0].content.contains("connection refused"));
    }

    #[test]
    fn test_scroll_clamps() {
        let mut state = ResearchTabState::new();
        assert_eq!(state.scroll_offset, 0);
        // Scroll down from 0 should stay at 0.
        state.scroll_down();
        assert_eq!(state.scroll_offset, 0);
        // Scroll up increases offset and disables follow_tail.
        state.scroll_up();
        assert_eq!(state.scroll_offset, 1);
        assert!(!state.follow_tail);
        // Scroll back down re-enables follow_tail.
        state.scroll_down();
        assert_eq!(state.scroll_offset, 0);
        assert!(state.follow_tail);
    }
}
