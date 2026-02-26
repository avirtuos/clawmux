//! Tab 6: Review discussion timeline.
//!
//! Renders a chronological feed of code-review events for the selected task:
//! agent summaries, user-posted inline comments, kickback notices, and lifecycle
//! banners. Distinct visual styles make each entry type immediately recognizable.

use std::cell::Cell;
use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::tasks::TaskId;
use crate::tui::markdown::{markdown_to_lines, visual_line_count};

/// A single entry in the review discussion timeline for a task.
#[derive(Debug, Clone)]
pub enum ReviewEntry {
    /// Full summary produced by a CodeReview agent on completion.
    AgentSummary {
        /// The display name of the agent that completed.
        agent: String,
        /// The full review summary text (may be markdown).
        summary: String,
    },
    /// A record of inline comments the user posted via the Code Diff tab.
    UserComments {
        /// The formatted comment strings (e.g. `"path:1-3: comment text"`).
        comments: Vec<String>,
    },
    /// A notice that an agent kicked back to another agent during review.
    Kickback {
        /// Display name of the agent that initiated the kickback.
        from: String,
        /// Display name of the agent that received the kickback.
        to: String,
        /// Human-readable reason for the kickback.
        reason: String,
    },
    /// A banner-style lifecycle event (e.g. "Review approved").
    Banner {
        /// The banner message text.
        message: String,
    },
}

/// UI state for Tab 6 (Review Discussion).
///
/// Maintains a per-task ordered list of [`ReviewEntry`] items and supports
/// scrollable rendering with follow-tail auto-scroll, mirroring the pattern
/// used by [`crate::tui::tabs::agent_activity::Tab2State`].
pub struct ReviewTabState {
    /// Per-task ordered list of review entries.
    buffers: HashMap<TaskId, Vec<ReviewEntry>>,
    /// The task currently displayed in the viewport.
    current_task_id: Option<TaskId>,
    /// Saved scroll offset used when `follow_tail` is false.
    scroll_offset: usize,
    /// When `true`, the viewport tracks the bottom of the buffer (auto-scroll).
    follow_tail: bool,
    /// Maximum scroll offset from the last render, used for scroll clamping.
    ///
    /// Uses `Cell` for interior mutability so `render()` can update it through `&self`.
    last_max_scroll: Cell<usize>,
}

impl ReviewTabState {
    /// Creates a new `ReviewTabState` with empty buffers and no task selected.
    pub fn new() -> Self {
        ReviewTabState {
            buffers: HashMap::new(),
            current_task_id: None,
            scroll_offset: 0,
            follow_tail: true,
            last_max_scroll: Cell::new(0),
        }
    }

    /// Appends an [`ReviewEntry::AgentSummary`] for `task_id`.
    ///
    /// Stores the full summary text so the Review tab can render it in its entirety,
    /// unlike the truncated banner shown in the Agent Activity tab.
    pub fn push_agent_summary(&mut self, task_id: &TaskId, agent: &str, summary: &str) {
        let buffer = self.buffers.entry(task_id.clone()).or_default();
        buffer.push(ReviewEntry::AgentSummary {
            agent: agent.to_string(),
            summary: summary.to_string(),
        });
        if self.current_task_id.as_ref() == Some(task_id) {
            self.follow_tail = true;
        }
    }

    /// Appends a [`ReviewEntry::UserComments`] for `task_id`.
    ///
    /// Records the formatted inline comments the user posted, preserving them in
    /// the review timeline for reference after the revision request is sent.
    pub fn push_user_comments(&mut self, task_id: &TaskId, comments: &[String]) {
        let buffer = self.buffers.entry(task_id.clone()).or_default();
        buffer.push(ReviewEntry::UserComments {
            comments: comments.to_vec(),
        });
        if self.current_task_id.as_ref() == Some(task_id) {
            self.follow_tail = true;
        }
    }

    /// Appends a [`ReviewEntry::Kickback`] for `task_id`.
    ///
    /// Records that the review agent kicked back to another agent, including the reason.
    pub fn push_kickback(&mut self, task_id: &TaskId, from: &str, to: &str, reason: &str) {
        let buffer = self.buffers.entry(task_id.clone()).or_default();
        buffer.push(ReviewEntry::Kickback {
            from: from.to_string(),
            to: to.to_string(),
            reason: reason.to_string(),
        });
        if self.current_task_id.as_ref() == Some(task_id) {
            self.follow_tail = true;
        }
    }

    /// Appends a [`ReviewEntry::Banner`] for `task_id`.
    ///
    /// Banners are used for lifecycle events such as "Review approved".
    pub fn push_banner(&mut self, task_id: &TaskId, message: String) {
        let buffer = self.buffers.entry(task_id.clone()).or_default();
        buffer.push(ReviewEntry::Banner { message });
        if self.current_task_id.as_ref() == Some(task_id) {
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

    /// Returns the ordered slice of [`ReviewEntry`] items for `task_id`.
    ///
    /// Returns an empty slice if no entries exist for the task.
    pub fn entries_for(&self, task_id: &TaskId) -> &[ReviewEntry] {
        self.buffers
            .get(task_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
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
}

impl Default for ReviewTabState {
    fn default() -> Self {
        ReviewTabState::new()
    }
}

/// Renders the Review tab into `area`.
///
/// Displays a placeholder when no task is selected or when there are no review
/// entries yet. When entries exist, renders them in chronological order with
/// distinct styles per variant. Supports scrolling with follow-tail auto-scroll.
///
/// Updates `state.last_max_scroll` via interior mutability so that `scroll_up`
/// and `scroll_down` can reference the correct visual line count.
pub fn render(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    task_id: Option<&TaskId>,
    state: &ReviewTabState,
) {
    let block = Block::default().title("Review").borders(Borders::ALL);

    if task_id.is_none() {
        let placeholder = Paragraph::new("Select a task to view review discussion")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(placeholder, area);
        return;
    }

    let task_id = task_id.unwrap();
    let entries = state.entries_for(task_id);

    if entries.is_empty() {
        let placeholder = Paragraph::new("No review activity yet.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(placeholder, area);
        return;
    }

    // Build display lines from all entries in order.
    let mut lines: Vec<Line> = Vec::new();
    for entry in entries {
        match entry {
            ReviewEntry::AgentSummary { agent, summary } => {
                lines.push(Line::from(Span::styled(
                    format!("--- Code Review Summary ({}) ---", agent),
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.extend(markdown_to_lines(summary));
                lines.push(Line::from(""));
            }
            ReviewEntry::UserComments { comments } => {
                lines.push(Line::from(Span::styled(
                    "--- Your Review Comments ---",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                for comment in comments {
                    lines.push(Line::from(vec![
                        Span::raw("  - "),
                        Span::styled(comment.clone(), Style::default().fg(Color::White)),
                    ]));
                }
                lines.push(Line::from(""));
            }
            ReviewEntry::Kickback { from, to, reason } => {
                lines.push(Line::from(Span::styled(
                    format!("--- Kickback: {} -> {} ---", from, to),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(reason.clone(), Style::default().fg(Color::White)),
                ]));
                lines.push(Line::from(""));
            }
            ReviewEntry::Banner { message } => {
                lines.push(Line::from(Span::styled(
                    message.clone(),
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                )));
            }
        }
    }

    // Compute effective scroll using visual (wrapped) line counts.
    let content_width = area.width.saturating_sub(2);
    let viewport_height = area.height.saturating_sub(2) as usize;
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

    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task_id() -> TaskId {
        TaskId::from_path("tasks/1.1.md")
    }

    fn other_task_id() -> TaskId {
        TaskId::from_path("tasks/1.2.md")
    }

    /// Verifies that push_agent_summary creates an AgentSummary entry.
    #[test]
    fn test_push_agent_summary_creates_entry() {
        let mut state = ReviewTabState::new();
        let id = task_id();
        state.push_agent_summary(&id, "Code Review Agent", "Looks good overall.");
        let entries = state.entries_for(&id);
        assert_eq!(entries.len(), 1, "should have exactly one entry");
        assert!(
            matches!(
                &entries[0],
                ReviewEntry::AgentSummary { agent, summary }
                    if agent == "Code Review Agent" && summary == "Looks good overall."
            ),
            "entry should be AgentSummary with correct fields; got: {:?}",
            entries[0]
        );
    }

    /// Verifies that push_user_comments creates a UserComments entry.
    #[test]
    fn test_push_user_comments_creates_entry() {
        let mut state = ReviewTabState::new();
        let id = task_id();
        let comments = vec!["src/main.rs:1-3: Rename this variable".to_string()];
        state.push_user_comments(&id, &comments);
        let entries = state.entries_for(&id);
        assert_eq!(entries.len(), 1, "should have exactly one entry");
        assert!(
            matches!(
                &entries[0],
                ReviewEntry::UserComments { comments: c } if c == &comments
            ),
            "entry should be UserComments; got: {:?}",
            entries[0]
        );
    }

    /// Verifies that push_kickback creates a Kickback entry.
    #[test]
    fn test_push_kickback_creates_entry() {
        let mut state = ReviewTabState::new();
        let id = task_id();
        state.push_kickback(
            &id,
            "Code Review Agent",
            "Developer Agent",
            "Missing tests.",
        );
        let entries = state.entries_for(&id);
        assert_eq!(entries.len(), 1, "should have exactly one entry");
        assert!(
            matches!(
                &entries[0],
                ReviewEntry::Kickback { from, to, reason }
                    if from == "Code Review Agent"
                        && to == "Developer Agent"
                        && reason == "Missing tests."
            ),
            "entry should be Kickback with correct fields; got: {:?}",
            entries[0]
        );
    }

    /// Verifies that entries are stored in chronological order.
    #[test]
    fn test_entries_interleave_chronologically() {
        let mut state = ReviewTabState::new();
        let id = task_id();
        state.push_agent_summary(&id, "Code Review Agent", "Initial review.");
        state.push_user_comments(&id, &["src/foo.rs:5: Rename var".to_string()]);
        state.push_kickback(&id, "Code Review Agent", "Developer Agent", "Fix tests.");

        let entries = state.entries_for(&id);
        assert_eq!(entries.len(), 3, "should have 3 entries in order");
        assert!(
            matches!(&entries[0], ReviewEntry::AgentSummary { .. }),
            "first entry should be AgentSummary"
        );
        assert!(
            matches!(&entries[1], ReviewEntry::UserComments { .. }),
            "second entry should be UserComments"
        );
        assert!(
            matches!(&entries[2], ReviewEntry::Kickback { .. }),
            "third entry should be Kickback"
        );
    }

    /// Verifies that set_displayed_task resets scroll when the task changes.
    #[test]
    fn test_set_displayed_task_resets_scroll() {
        let mut state = ReviewTabState::new();
        let id = task_id();
        let id2 = other_task_id();

        state.set_displayed_task(Some(&id));
        // Simulate user scrolling up.
        state.last_max_scroll.set(5);
        state.scroll_up(); // disables follow_tail, sets offset to 4
        assert!(
            !state.follow_tail,
            "follow_tail should be false after scroll_up"
        );
        assert_eq!(state.scroll_offset, 4);

        // Switch to a different task.
        state.set_displayed_task(Some(&id2));
        assert!(
            state.follow_tail,
            "follow_tail should be re-enabled after switching task"
        );
        assert_eq!(
            state.scroll_offset, 0,
            "scroll_offset should reset to 0 after switching task"
        );
    }
}
