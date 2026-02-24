//! Tab 4: Code Review -- unified diff view and comment input.
//!
//! Renders file diffs retrieved from the opencode diff endpoint with
//! syntax-highlighted hunks (green Added / red Removed / dim Context lines
//! prefixed with +/-/space). Provides a comment textarea for human review
//! feedback and action keys for approve / request-revisions.
//!
//! Navigation between files uses Left/Right arrows. The diff view is
//! scrollable with PgUp/PgDn. Press `c` to focus the comment textarea;
//! press Enter when not focused to append the current textarea content to
//! the accumulated comment list. Press `a` to approve; press `r` to emit
//! HumanRequestedRevisions with all accumulated comments.

use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_textarea::TextArea;

use crate::opencode::types::{DiffLineKind, DiffStatus, FileDiff};
use crate::tasks::models::TaskId;

/// UI state for Tab 4 (Code Review): per-task diff storage, file navigation,
/// diff scrolling, and comment accumulation.
pub struct Tab4State {
    /// Per-task list of file diffs fetched from the opencode diff endpoint.
    diffs: HashMap<TaskId, Vec<FileDiff>>,
    /// The task whose diffs are currently displayed, if any.
    current_task_id: Option<TaskId>,
    /// Index of the currently selected file within the diff list.
    pub selected_file: usize,
    /// Vertical scroll offset for the diff view.
    pub diff_scroll: u16,
    /// Textarea for entering a review comment.
    pub comment_input: TextArea<'static>,
    /// Accumulated review comments (appended via Enter, sent with `r`).
    pub comments: Vec<String>,
    /// Whether the comment textarea currently has keyboard focus.
    pub comment_focused: bool,
}

impl Tab4State {
    /// Creates a new `Tab4State` with empty diff storage and a blank comment input.
    pub fn new() -> Self {
        let mut comment_input = TextArea::default();
        comment_input.set_block(Self::unfocused_block("Add Comment"));
        Self {
            diffs: HashMap::new(),
            current_task_id: None,
            selected_file: 0,
            diff_scroll: 0,
            comment_input,
            comments: Vec::new(),
            comment_focused: false,
        }
    }

    /// Stores or replaces the diffs for the given task.
    pub fn set_diffs(&mut self, task_id: &TaskId, diffs: Vec<FileDiff>) {
        self.diffs.insert(task_id.clone(), diffs);
    }

    /// Returns the diffs for the given task, or an empty slice if none are stored.
    pub fn diffs_for(&self, task_id: &TaskId) -> &[FileDiff] {
        self.diffs.get(task_id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Sets the task whose diffs should be displayed.
    pub fn set_displayed_task(&mut self, task_id: Option<&TaskId>) {
        self.current_task_id = task_id.cloned();
    }

    /// Resets per-diff navigation state (file index, scroll, comment input and focus)
    /// when a new set of diffs is loaded.
    ///
    /// Accumulated `comments` are intentionally preserved across diff loads so that
    /// a reviewer can accumulate feedback across multiple agent iterations.
    pub fn reset_for_diffs(&mut self) {
        self.selected_file = 0;
        self.diff_scroll = 0;
        let mut ta = TextArea::default();
        ta.set_block(Self::unfocused_block("Add Comment"));
        self.comment_input = ta;
        self.comment_focused = false;
    }

    /// Decrements `selected_file`, clamped at 0. Resets scroll.
    pub fn select_prev_file(&mut self) {
        self.selected_file = self.selected_file.saturating_sub(1);
        self.diff_scroll = 0;
    }

    /// Increments `selected_file`, clamped at `count - 1`. Resets scroll.
    ///
    /// If `count` is 0 this is a no-op.
    pub fn select_next_file(&mut self, count: usize) {
        if count > 0 {
            self.selected_file = (self.selected_file + 1).min(count - 1);
            self.diff_scroll = 0;
        }
    }

    /// Scrolls the diff view up by 3 lines.
    pub fn scroll_up(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_sub(3);
    }

    /// Scrolls the diff view down by 3 lines.
    pub fn scroll_down(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_add(3);
    }

    /// Switches the comment textarea to the focused (yellow border) style.
    pub fn set_comment_focused(&mut self) {
        self.comment_focused = true;
        self.comment_input
            .set_block(Self::focused_block("Add Comment"));
    }

    /// Switches the comment textarea to the unfocused (default border) style.
    pub fn set_comment_unfocused(&mut self) {
        self.comment_focused = false;
        self.comment_input
            .set_block(Self::unfocused_block("Add Comment"));
    }

    /// Appends the current textarea content to `comments` and clears the textarea.
    ///
    /// Returns the appended comment text if it was non-empty, or `None` if the
    /// textarea was blank (in which case nothing is appended).
    pub fn submit_comment(&mut self) -> Option<String> {
        let text = self.comment_input.lines().join("\n");
        let trimmed = text.trim().to_string();
        if trimmed.is_empty() {
            return None;
        }
        self.comments.push(trimmed.clone());
        let mut ta = TextArea::default();
        ta.set_block(Self::unfocused_block("Add Comment"));
        self.comment_input = ta;
        self.comment_focused = false;
        Some(trimmed)
    }

    /// Takes all accumulated comments and resets the list to empty.
    pub fn take_comments(&mut self) -> Vec<String> {
        std::mem::take(&mut self.comments)
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

impl Default for Tab4State {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders the Code Review tab into `area`.
///
/// When no task is selected (`task_id` is `None`), displays a placeholder.
/// When the task has no diffs stored, displays a "No diffs available." message.
/// Otherwise renders:
/// - A block title: `Review  < N/M: path/to/file [status] >`
/// - A scrollable diff view (70% of inner height) with colored +/-/space prefixed lines
/// - A comment textarea (4 rows)
/// - A hint bar at the bottom
pub fn render(frame: &mut Frame, area: Rect, task_id: Option<&TaskId>, state: &Tab4State) {
    let Some(task_id) = task_id else {
        let placeholder = Paragraph::new("Select a task from the list")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().borders(Borders::ALL));
        frame.render_widget(placeholder, area);
        return;
    };

    let diffs = state.diffs_for(task_id);
    if diffs.is_empty() {
        let no_diffs = Paragraph::new("No diffs available.")
            .style(Style::default().fg(Color::DarkGray))
            .block(Block::default().title("Review").borders(Borders::ALL));
        frame.render_widget(no_diffs, area);
        return;
    }

    let file_count = diffs.len();
    let sel = state.selected_file.min(file_count - 1);
    let file_diff = &diffs[sel];

    let status_label = match file_diff.status {
        DiffStatus::Added => "[+added]",
        DiffStatus::Modified => "[modified]",
        DiffStatus::Deleted => "[-deleted]",
    };
    let block_title = format!(
        "Review  < {}/{}: {} {} >",
        sel + 1,
        file_count,
        file_diff.path,
        status_label
    );

    let block = Block::default().title(block_title).borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split inner area: diff view (70%), comment textarea (4 rows), hint (1 line).
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(70),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(inner);

    let diff_area = sections[0];
    let comment_area = sections[1];
    let hint_area = sections[2];

    // Build diff lines from all hunks.
    let mut diff_lines: Vec<Line> = Vec::new();
    for hunk in &file_diff.hunks {
        diff_lines.push(Line::from(Span::styled(
            format!("@@ -{} +{} @@", hunk.old_start, hunk.new_start),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        for line in &hunk.lines {
            let (prefix, style) = match line.kind {
                DiffLineKind::Added => ("+", Style::default().fg(Color::Green)),
                DiffLineKind::Removed => ("-", Style::default().fg(Color::Red)),
                DiffLineKind::Context => (" ", Style::default().fg(Color::DarkGray)),
            };
            diff_lines.push(Line::from(Span::styled(
                format!("{}{}", prefix, line.content),
                style,
            )));
        }
    }

    let diff_para = Paragraph::new(diff_lines).scroll((state.diff_scroll, 0));
    frame.render_widget(diff_para, diff_area);

    // Comment textarea.
    frame.render_widget(&state.comment_input, comment_area);

    // Hint bar.
    let comment_count = state.comments.len();
    let count_suffix = if comment_count > 0 {
        format!(
            " ({} comment{})",
            comment_count,
            if comment_count == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };
    let hint = Paragraph::new(format!(
        "[</> arrows] file | [PgUp/PgDn] scroll | [c] edit comment | [Enter] add comment | [a] approve | [r] revisions{}",
        count_suffix
    ))
    .style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hint, hint_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::opencode::types::{DiffHunk, DiffLine, DiffLineKind, DiffStatus, FileDiff};

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn make_task_id() -> TaskId {
        TaskId::from_path("tasks/1.1.md")
    }

    fn make_added_diff() -> FileDiff {
        FileDiff {
            path: "src/foo.rs".to_string(),
            status: DiffStatus::Added,
            hunks: vec![DiffHunk {
                old_start: 0,
                new_start: 1,
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Added,
                        content: "fn hello() {}".to_string(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Context,
                        content: "// context".to_string(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Removed,
                        content: "fn old() {}".to_string(),
                    },
                ],
            }],
        }
    }

    fn render_to_string(state: &Tab4State, task_id: Option<&TaskId>) -> String {
        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                render(frame, frame.area(), task_id, state);
            })
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect()
    }

    #[test]
    fn test_tab4_no_diffs_renders_placeholder() {
        let state = Tab4State::new();
        let task_id = make_task_id();
        let content = render_to_string(&state, Some(&task_id));
        assert!(
            content.contains("No diffs available"),
            "should show placeholder when no diffs; got: {content:?}"
        );
    }

    #[test]
    fn test_tab4_no_task_renders_placeholder() {
        let state = Tab4State::new();
        let content = render_to_string(&state, None);
        assert!(
            content.contains("Select a task"),
            "should show placeholder when no task; got: {content:?}"
        );
    }

    #[test]
    fn test_tab4_diff_render_added_line() {
        let mut state = Tab4State::new();
        let task_id = make_task_id();
        state.set_diffs(&task_id, vec![make_added_diff()]);
        let content = render_to_string(&state, Some(&task_id));
        assert!(
            content.contains("+fn hello()"),
            "should render added line with '+' prefix; got: {content:?}"
        );
    }

    #[test]
    fn test_tab4_diff_render_removed_line() {
        let mut state = Tab4State::new();
        let task_id = make_task_id();
        state.set_diffs(&task_id, vec![make_added_diff()]);
        let content = render_to_string(&state, Some(&task_id));
        assert!(
            content.contains("-fn old()"),
            "should render removed line with '-' prefix; got: {content:?}"
        );
    }

    #[test]
    fn test_tab4_diff_render_context_line() {
        let mut state = Tab4State::new();
        let task_id = make_task_id();
        state.set_diffs(&task_id, vec![make_added_diff()]);
        let content = render_to_string(&state, Some(&task_id));
        assert!(
            content.contains(" // context"),
            "should render context line with ' ' prefix; got: {content:?}"
        );
    }

    #[test]
    fn test_tab4_file_navigation() {
        let mut state = Tab4State::new();
        assert_eq!(state.selected_file, 0);

        let file2 = FileDiff {
            path: "src/bar.rs".to_string(),
            status: DiffStatus::Modified,
            hunks: vec![],
        };
        let task_id = make_task_id();
        state.set_diffs(&task_id, vec![make_added_diff(), file2]);

        state.select_next_file(2);
        assert_eq!(state.selected_file, 1, "should advance to file 2");
        assert_eq!(state.diff_scroll, 0, "scroll should reset on file change");

        state.select_next_file(2);
        assert_eq!(
            state.selected_file, 1,
            "select_next_file should clamp at count-1"
        );

        state.select_prev_file();
        assert_eq!(state.selected_file, 0, "should go back to file 1");

        state.select_prev_file();
        assert_eq!(state.selected_file, 0, "select_prev_file should clamp at 0");
    }

    #[test]
    fn test_tab4_file_header_shows_file_name() {
        let mut state = Tab4State::new();
        let task_id = make_task_id();
        state.set_diffs(&task_id, vec![make_added_diff()]);
        let content = render_to_string(&state, Some(&task_id));
        assert!(
            content.contains("src/foo.rs"),
            "file header should show file path; got: {content:?}"
        );
    }

    #[test]
    fn test_tab4_add_comment() {
        let mut state = Tab4State::new();
        assert!(state.comments.is_empty());

        // Simulate user typing in the textarea then calling submit_comment.
        state.comment_input.insert_str("This needs refactoring");
        let result = state.submit_comment();
        assert_eq!(result, Some("This needs refactoring".to_string()));
        assert_eq!(state.comments.len(), 1);
        assert_eq!(state.comments[0], "This needs refactoring");
        // Textarea should be cleared.
        assert_eq!(state.comment_input.lines().join(""), "");
    }

    #[test]
    fn test_tab4_add_comment_empty_is_noop() {
        let mut state = Tab4State::new();
        let result = state.submit_comment();
        assert!(
            result.is_none(),
            "submitting empty comment should return None"
        );
        assert!(state.comments.is_empty());
    }

    #[test]
    fn test_tab4_take_comments_clears_list() {
        let mut state = Tab4State::new();
        state.comment_input.insert_str("comment one");
        state.submit_comment();
        state.comment_input.insert_str("comment two");
        state.submit_comment();
        assert_eq!(state.comments.len(), 2);

        let taken = state.take_comments();
        assert_eq!(taken.len(), 2);
        assert!(
            state.comments.is_empty(),
            "comments should be cleared after take"
        );
    }

    #[test]
    fn test_tab4_reset_for_diffs_preserves_comments() {
        let mut state = Tab4State::new();
        state.selected_file = 2;
        state.diff_scroll = 10;
        state.comments.push("old comment".to_string());

        state.reset_for_diffs();
        assert_eq!(state.selected_file, 0, "selected_file should reset");
        assert_eq!(state.diff_scroll, 0, "diff_scroll should reset");
        assert_eq!(
            state.comments.len(),
            1,
            "comments should be preserved across reset"
        );
    }

    #[test]
    fn test_tab4_scroll_up_down() {
        let mut state = Tab4State::new();
        state.scroll_down();
        assert_eq!(state.diff_scroll, 3);
        state.scroll_up();
        assert_eq!(state.diff_scroll, 0);
        // Scroll up at 0 should clamp.
        state.scroll_up();
        assert_eq!(state.diff_scroll, 0, "scroll_up should clamp at 0");
    }

    #[test]
    fn test_tab4_comment_focus_style() {
        let mut state = Tab4State::new();
        assert!(!state.comment_focused);
        state.set_comment_focused();
        assert!(state.comment_focused);
        state.set_comment_unfocused();
        assert!(!state.comment_focused);
    }
}
