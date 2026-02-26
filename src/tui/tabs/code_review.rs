//! Tab 4: Code Review -- unified diff view with inline comments and line-range selection.
//!
//! Renders file diffs with syntax-highlighted hunks (green Added / red Removed / dim Context
//! lines prefixed +/-/space). Supports GitHub-style line-specific review comments:
//!
//! - Press `r` to focus the review pane.
//! - Up/Down move the cursor line; PageUp/PageDown navigate between files.
//! - Space marks the start of a selection; press Space again to mark the end and enter
//!   comment-input mode. Type a comment and press Enter to attach it inline in the diff.
//! - Press Esc at any point to cancel the current operation and exit review mode.
//! - Press `a` to approve. Press `R` (Shift+R) to emit HumanRequestedRevisions with all
//!   accumulated inline comments formatted as `path:start-end: text`.

use std::collections::HashMap;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use tui_textarea::TextArea;

use crate::opencode::types::{DiffLineKind, DiffStatus, FileDiff};
use crate::tasks::models::TaskId;

/// A review comment attached to a specific line range in a file diff.
///
/// Created when the user selects a range (Space start, Space end) and types a comment.
/// Rendered inline in the diff view after the line corresponding to `end_flat`.
#[derive(Debug, Clone)]
pub struct InlineComment {
    /// Index of the file in the diff list this comment belongs to.
    pub file_idx: usize,
    /// File path (for comment formatting).
    pub path: String,
    /// Index into `flatten_file_diff()` output where the selection ends.
    ///
    /// Used to position the comment block inline in the rendered diff.
    pub end_flat: usize,
    /// Git diff coordinate at the selection start (new-file line or old-file line).
    pub start_coord: u32,
    /// Git diff coordinate at the selection end (new-file line or old-file line).
    pub end_coord: u32,
    /// The review comment text.
    pub text: String,
}

impl InlineComment {
    /// Returns the header string `path:start-end` (or `path:line` for single-line).
    pub fn formatted_header(&self) -> String {
        if self.start_coord == self.end_coord || self.end_coord == 0 {
            format!("{}:{}", self.path, self.start_coord)
        } else {
            format!("{}:{}-{}", self.path, self.start_coord, self.end_coord)
        }
    }

    /// Formats this comment as `path:start-end: text` for use in revision requests.
    pub fn formatted(&self) -> String {
        format!("{}: {}", self.formatted_header(), self.text)
    }
}

/// The kind of a flattened diff display line.
#[derive(Debug, Clone, PartialEq)]
pub enum FlatDiffLineKind {
    /// A `@@ -old +new @@` hunk header.
    HunkHeader,
    /// A line added in the new version (prefix `+`).
    Added,
    /// A line removed from the old version (prefix `-`).
    Removed,
    /// An unchanged context line (prefix ` `).
    Context,
}

/// A single rendered line in the diff view with precomputed git line coordinates.
#[derive(Debug, Clone)]
pub struct FlatDiffLine {
    /// Display text including prefix (`+`, `-`, ` `, or the hunk header).
    pub display: String,
    /// Line kind for colour styling.
    pub kind: FlatDiffLineKind,
    /// New-file line number (set for Added and Context lines; `None` for Removed/HunkHeader).
    pub new_line: Option<u32>,
    /// Old-file line number (set for Removed and Context lines; `None` for Added/HunkHeader).
    pub old_line: Option<u32>,
}

impl FlatDiffLine {
    /// Returns the primary git coordinate for comment formatting.
    ///
    /// Prefers `new_line` (added/context lines), falls back to `old_line` (removed lines).
    pub fn coord(&self) -> Option<u32> {
        self.new_line.or(self.old_line)
    }
}

/// Flattens a [`FileDiff`] into a sequential list of display lines with git coordinates.
///
/// Hunk headers are included as separator lines with no coordinates. Added and Context lines
/// receive new-file coordinates; Removed and Context lines receive old-file coordinates.
pub fn flatten_file_diff(diff: &FileDiff) -> Vec<FlatDiffLine> {
    let mut lines = Vec::new();
    for hunk in &diff.hunks {
        lines.push(FlatDiffLine {
            display: format!("@@ -{} +{} @@", hunk.old_start, hunk.new_start),
            kind: FlatDiffLineKind::HunkHeader,
            new_line: None,
            old_line: None,
        });
        let mut new_line = hunk.new_start;
        let mut old_line = hunk.old_start;
        for line in &hunk.lines {
            match line.kind {
                DiffLineKind::Added => {
                    lines.push(FlatDiffLine {
                        display: format!("+{}", line.content),
                        kind: FlatDiffLineKind::Added,
                        new_line: Some(new_line),
                        old_line: None,
                    });
                    new_line += 1;
                }
                DiffLineKind::Removed => {
                    lines.push(FlatDiffLine {
                        display: format!("-{}", line.content),
                        kind: FlatDiffLineKind::Removed,
                        new_line: None,
                        old_line: Some(old_line),
                    });
                    old_line += 1;
                }
                DiffLineKind::Context => {
                    lines.push(FlatDiffLine {
                        display: format!(" {}", line.content),
                        kind: FlatDiffLineKind::Context,
                        new_line: Some(new_line),
                        old_line: Some(old_line),
                    });
                    new_line += 1;
                    old_line += 1;
                }
            }
        }
    }
    lines
}

/// UI state for Tab 4 (Code Review): per-task diff storage, file navigation,
/// cursor-based line selection, and inline comment accumulation.
pub struct Tab4State {
    /// Per-task list of file diffs fetched from the opencode diff endpoint.
    diffs: HashMap<TaskId, Vec<FileDiff>>,
    /// The task whose diffs are currently displayed, if any.
    current_task_id: Option<TaskId>,
    /// Index of the currently selected file within the diff list.
    pub selected_file: usize,
    /// Vertical scroll offset for the diff view.
    pub diff_scroll: u16,
    /// Whether the review pane currently has keyboard focus.
    ///
    /// When `true`, Up/Down move `cursor_line` and Space starts/ends selections.
    pub review_focused: bool,
    /// Cursor position within the current file's flat diff line list.
    pub cursor_line: usize,
    /// Flat-line index where the current selection started (first Space press).
    selection_start: Option<usize>,
    /// Completed selection range `(start_flat, end_flat)`, set after the second Space press.
    pending_range: Option<(usize, usize)>,
    /// Git coordinate of the pending selection start (extracted from flat_lines).
    pending_start_coord: u32,
    /// Git coordinate of the pending selection end (extracted from flat_lines).
    pending_end_coord: u32,
    /// Whether the comment draft textarea is active (user is typing a comment).
    pub comment_mode: bool,
    /// Textarea for drafting a line-range comment.
    pub comment_draft: TextArea<'static>,
    /// All submitted inline review comments for the current diff session.
    pub inline_comments: Vec<InlineComment>,
}

impl Tab4State {
    /// Creates a new `Tab4State` with empty diff storage and default state.
    pub fn new() -> Self {
        let mut comment_draft = TextArea::default();
        comment_draft.set_block(Self::comment_draft_block());
        Self {
            diffs: HashMap::new(),
            current_task_id: None,
            selected_file: 0,
            diff_scroll: 0,
            review_focused: false,
            cursor_line: 0,
            selection_start: None,
            pending_range: None,
            pending_start_coord: 0,
            pending_end_coord: 0,
            comment_mode: false,
            comment_draft,
            inline_comments: Vec::new(),
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

    /// Returns the diffs for the currently displayed task.
    pub fn current_diffs(&self) -> &[FileDiff] {
        self.current_task_id
            .as_ref()
            .and_then(|id| self.diffs.get(id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Sets the task whose diffs should be displayed.
    pub fn set_displayed_task(&mut self, task_id: Option<&TaskId>) {
        self.current_task_id = task_id.cloned();
    }

    /// Resets per-diff navigation state when a new set of diffs is loaded.
    ///
    /// Accumulated `inline_comments` are intentionally preserved across diff loads
    /// so that a reviewer can accumulate feedback across multiple agent iterations.
    pub fn reset_for_diffs(&mut self) {
        self.selected_file = 0;
        self.diff_scroll = 0;
        self.cursor_line = 0;
        self.selection_start = None;
        self.pending_range = None;
        self.pending_start_coord = 0;
        self.pending_end_coord = 0;
        self.comment_mode = false;
        self.review_focused = false;
        let mut ta = TextArea::default();
        ta.set_block(Self::comment_draft_block());
        self.comment_draft = ta;
    }

    /// Focuses the review pane, enabling cursor navigation and line selection.
    pub fn focus_review(&mut self) {
        self.review_focused = true;
    }

    /// Cancels any active selection or comment draft and removes focus from the review pane.
    pub fn cancel_review(&mut self) {
        self.review_focused = false;
        self.comment_mode = false;
        self.selection_start = None;
        self.pending_range = None;
        self.pending_start_coord = 0;
        self.pending_end_coord = 0;
        let mut ta = TextArea::default();
        ta.set_block(Self::comment_draft_block());
        self.comment_draft = ta;
    }

    /// Moves the cursor up one line, adjusting the scroll offset to keep it visible.
    pub fn move_cursor_up(&mut self) {
        self.cursor_line = self.cursor_line.saturating_sub(1);
        if self.cursor_line < self.diff_scroll as usize {
            self.diff_scroll = self.cursor_line as u16;
        }
    }

    /// Moves the cursor down one line (clamped to `max - 1`), adjusting scroll to keep it visible.
    ///
    /// Uses a conservative estimate of 15 visible rows for scroll adjustment.
    pub fn move_cursor_down(&mut self, max: usize) {
        if max > 0 && self.cursor_line + 1 < max {
            self.cursor_line += 1;
        }
        const VISIBLE_ESTIMATE: usize = 15;
        let scroll = self.diff_scroll as usize;
        if self.cursor_line >= scroll + VISIBLE_ESTIMATE {
            self.diff_scroll = (self.cursor_line + 1).saturating_sub(VISIBLE_ESTIMATE) as u16;
        }
    }

    /// Decrements `selected_file`, clamped at 0. Resets scroll, cursor, and selection.
    pub fn select_prev_file(&mut self) {
        self.selected_file = self.selected_file.saturating_sub(1);
        self.diff_scroll = 0;
        self.cursor_line = 0;
        self.selection_start = None;
    }

    /// Increments `selected_file`, clamped at `count - 1`. Resets scroll, cursor, and selection.
    ///
    /// If `count` is 0 this is a no-op.
    pub fn select_next_file(&mut self, count: usize) {
        if count > 0 {
            self.selected_file = (self.selected_file + 1).min(count - 1);
            self.diff_scroll = 0;
            self.cursor_line = 0;
            self.selection_start = None;
        }
    }

    /// Scrolls the diff view up by 3 lines (available when review pane is not focused).
    pub fn scroll_up(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_sub(3);
    }

    /// Scrolls the diff view down by 3 lines (available when review pane is not focused).
    pub fn scroll_down(&mut self) {
        self.diff_scroll = self.diff_scroll.saturating_add(3);
    }

    /// Handles a Space key press during review mode.
    ///
    /// If no selection is active, marks the cursor position as the selection start.
    /// If a selection start is already set, completes the selection and enters comment mode,
    /// extracting git coordinates from `flat_lines`.
    ///
    /// Returns `true` if comment mode was just entered (second Space press).
    pub fn press_space(&mut self, flat_lines: &[FlatDiffLine]) -> bool {
        match self.selection_start {
            None => {
                self.selection_start = Some(self.cursor_line);
                false
            }
            Some(start) => {
                let lo = start.min(self.cursor_line);
                let hi = start.max(self.cursor_line);
                self.pending_range = Some((lo, hi));
                self.selection_start = None;
                self.pending_start_coord = flat_lines.get(lo).and_then(|l| l.coord()).unwrap_or(0);
                self.pending_end_coord = flat_lines
                    .get(hi)
                    .and_then(|l| l.coord())
                    .unwrap_or(self.pending_start_coord);
                self.comment_mode = true;
                true
            }
        }
    }

    /// Submits the current comment draft as an inline comment for the given file.
    ///
    /// Does nothing if no selection range is pending. If the draft is empty, the
    /// pending range is still cleared and comment mode is exited.
    /// After submission, comment mode is cleared and review focus is restored for browsing.
    pub fn submit_draft_comment(&mut self, file_idx: usize, path: &str) {
        if let Some((_lo, hi)) = self.pending_range {
            let text = self.comment_draft.lines().join("\n");
            let trimmed = text.trim().to_string();
            if !trimmed.is_empty() {
                self.inline_comments.push(InlineComment {
                    file_idx,
                    path: path.to_string(),
                    end_flat: hi,
                    start_coord: self.pending_start_coord,
                    end_coord: self.pending_end_coord,
                    text: trimmed,
                });
            }
        }
        self.pending_range = None;
        self.pending_start_coord = 0;
        self.pending_end_coord = 0;
        self.comment_mode = false;
        let mut ta = TextArea::default();
        ta.set_block(Self::comment_draft_block());
        self.comment_draft = ta;
    }

    /// Takes all accumulated inline comments, formats them as `path:start-end: text` strings,
    /// and clears the list.
    pub fn take_comments(&mut self) -> Vec<String> {
        self.inline_comments
            .drain(..)
            .map(|c| c.formatted())
            .collect()
    }

    /// Returns the active selection range `(lo, hi)` as flat-line indices for rendering.
    ///
    /// Returns `None` if neither a live selection nor a pending range exists.
    pub fn selection_bounds(&self) -> Option<(usize, usize)> {
        if let Some((lo, hi)) = self.pending_range {
            return Some((lo, hi));
        }
        if let Some(start) = self.selection_start {
            let lo = start.min(self.cursor_line);
            let hi = start.max(self.cursor_line);
            return Some((lo, hi));
        }
        None
    }

    fn comment_draft_block() -> Block<'static> {
        Block::default()
            .title("Add Comment  [Enter] save  [Esc] cancel")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow))
    }
}

impl Default for Tab4State {
    fn default() -> Self {
        Self::new()
    }
}

/// Renders the Code Review tab into `area`.
///
/// When no task is selected or the task has no diffs, displays a placeholder message.
/// Otherwise renders:
/// - A block title with `N/M: path/to/file [status]` and `[REVIEW]` indicator when focused.
/// - A scrollable diff view with cursor (blue) and selection (yellow) highlighting.
/// - Inline comments displayed after their anchor line.
/// - A comment draft textarea (when in comment mode).
/// - A context-sensitive hint bar at the bottom.
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
            .block(Block::default().title("Code Diff").borders(Borders::ALL));
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
    let focus_indicator = if state.review_focused {
        " [REVIEW]"
    } else {
        ""
    };
    let block_title = format!(
        "Code Diff {}/{}: {} {}{}",
        sel + 1,
        file_count,
        file_diff.path,
        status_label,
        focus_indicator
    );

    let block = Block::default().title(block_title).borders(Borders::ALL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Split layout: diff area + optional comment draft + hint bar.
    let (diff_area, comment_draft_area, hint_area) = if state.comment_mode {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(4),
                Constraint::Length(1),
            ])
            .split(inner);
        (sections[0], Some(sections[1]), sections[2])
    } else {
        let sections = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(inner);
        (sections[0], None, sections[1])
    };

    // Compute the flat line list and selection bounds for the current file.
    let flat_lines = flatten_file_diff(file_diff);
    let sel_bounds = state.selection_bounds();

    // Build diff display lines with cursor/selection highlighting.
    let mut diff_lines: Vec<Line> = Vec::new();
    for (i, fl) in flat_lines.iter().enumerate() {
        let is_cursor = state.review_focused && !state.comment_mode && i == state.cursor_line;
        let in_selection = sel_bounds
            .map(|(lo, hi)| i >= lo && i <= hi)
            .unwrap_or(false);

        let base_style = match fl.kind {
            FlatDiffLineKind::HunkHeader => Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            FlatDiffLineKind::Added => Style::default().fg(Color::Green),
            FlatDiffLineKind::Removed => Style::default().fg(Color::Red),
            FlatDiffLineKind::Context => Style::default().fg(Color::DarkGray),
        };

        let style = if is_cursor {
            // Cursor always shows as blue, whether or not it overlaps a selection.
            Style::default().bg(Color::Blue).fg(Color::White)
        } else if in_selection {
            Style::default().bg(Color::Yellow).fg(Color::Black)
        } else {
            base_style
        };

        diff_lines.push(Line::from(Span::styled(fl.display.clone(), style)));

        // Render any inline comments anchored at this flat-line position.
        for comment in &state.inline_comments {
            if comment.file_idx == sel && comment.end_flat == i {
                diff_lines.push(Line::from(vec![
                    Span::styled(
                        format!("  >> [{}] ", comment.formatted_header()),
                        Style::default()
                            .fg(Color::Magenta)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(comment.text.clone(), Style::default().fg(Color::White)),
                ]));
            }
        }
    }

    let diff_para = Paragraph::new(diff_lines).scroll((state.diff_scroll, 0));
    frame.render_widget(diff_para, diff_area);

    // Comment draft textarea (visible only when comment mode is active).
    if let Some(ca) = comment_draft_area {
        frame.render_widget(&state.comment_draft, ca);
    }

    // Context-sensitive hint bar.
    let comment_count = state.inline_comments.len();
    let count_suffix = if comment_count > 0 {
        format!(
            " ({} comment{})",
            comment_count,
            if comment_count == 1 { "" } else { "s" }
        )
    } else {
        String::new()
    };

    let hint_text = if state.comment_mode {
        "[Enter] save | [Esc] cancel | Typing comment...".to_string()
    } else if state.review_focused && state.selection_start.is_some() {
        format!(
            "[Space] end selection | [Up/Down] extend | [Esc] cancel{}",
            count_suffix
        )
    } else if state.review_focused {
        format!(
            "[Up/Down] cursor | [PgUp/PgDn] files | [Space] select | [a] approve | [Esc] exit{}",
            count_suffix
        )
    } else {
        format!(
            "[r] review | [a] approve | [R] revisions | [PgUp/PgDn] scroll | [Tab] next tab | [q] quit{}",
            count_suffix
        )
    };

    let hint_para = Paragraph::new(hint_text).style(Style::default().fg(Color::DarkGray));
    frame.render_widget(hint_para, hint_area);
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

    fn make_hunk() -> DiffHunk {
        DiffHunk {
            old_start: 10,
            new_start: 12,
            lines: vec![
                DiffLine {
                    kind: DiffLineKind::Context,
                    content: "// context".to_string(),
                },
                DiffLine {
                    kind: DiffLineKind::Removed,
                    content: "fn old() {}".to_string(),
                },
                DiffLine {
                    kind: DiffLineKind::Added,
                    content: "fn hello() {}".to_string(),
                },
            ],
        }
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

    // ---- flatten_file_diff ----

    #[test]
    fn test_flatten_file_diff_coordinates() {
        let diff = FileDiff {
            path: "x.rs".to_string(),
            status: DiffStatus::Modified,
            hunks: vec![make_hunk()],
        };
        let flat = flatten_file_diff(&diff);
        // [0] HunkHeader, [1] Context, [2] Removed, [3] Added
        assert_eq!(flat.len(), 4);
        assert_eq!(flat[0].kind, FlatDiffLineKind::HunkHeader);
        assert_eq!(flat[0].new_line, None);
        assert_eq!(flat[0].old_line, None);

        // Context line: new_start=12, old_start=10 → new=12, old=10
        assert_eq!(flat[1].kind, FlatDiffLineKind::Context);
        assert_eq!(flat[1].new_line, Some(12));
        assert_eq!(flat[1].old_line, Some(10));

        // Removed line: old=11 (incremented from context), no new_line
        assert_eq!(flat[2].kind, FlatDiffLineKind::Removed);
        assert_eq!(flat[2].new_line, None);
        assert_eq!(flat[2].old_line, Some(11));

        // Added line: new=13 (incremented from context), no old_line
        assert_eq!(flat[3].kind, FlatDiffLineKind::Added);
        assert_eq!(flat[3].new_line, Some(13));
        assert_eq!(flat[3].old_line, None);
    }

    #[test]
    fn test_flatten_file_diff_display_prefixes() {
        let flat = flatten_file_diff(&make_added_diff());
        // HunkHeader, Added, Context, Removed
        assert!(
            flat[0].display.starts_with("@@"),
            "hunk header: {}",
            flat[0].display
        );
        assert!(
            flat[1].display.starts_with('+'),
            "added: {}",
            flat[1].display
        );
        assert!(
            flat[2].display.starts_with(' '),
            "context: {}",
            flat[2].display
        );
        assert!(
            flat[3].display.starts_with('-'),
            "removed: {}",
            flat[3].display
        );
    }

    // ---- render placeholders ----

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

    // ---- diff line rendering ----

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

    // ---- file navigation ----

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
        assert_eq!(state.cursor_line, 0, "cursor should reset on file change");

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

    // ---- cursor navigation ----

    #[test]
    fn test_tab4_cursor_navigation() {
        let mut state = Tab4State::new();
        assert_eq!(state.cursor_line, 0);

        // Move down (max=5 lines).
        state.move_cursor_down(5);
        assert_eq!(state.cursor_line, 1);
        state.move_cursor_down(5);
        assert_eq!(state.cursor_line, 2);

        // Move up.
        state.move_cursor_up();
        assert_eq!(state.cursor_line, 1);
        state.move_cursor_up();
        assert_eq!(state.cursor_line, 0);

        // Clamp at 0.
        state.move_cursor_up();
        assert_eq!(state.cursor_line, 0, "move_cursor_up should clamp at 0");
    }

    #[test]
    fn test_tab4_cursor_clamps_at_max() {
        let mut state = Tab4State::new();
        // With max=3, cursor can be at 0, 1, 2.
        for _ in 0..10 {
            state.move_cursor_down(3);
        }
        assert_eq!(state.cursor_line, 2, "cursor should clamp at max-1");
    }

    // ---- selection and comment ----

    #[test]
    fn test_tab4_press_space_starts_selection() {
        let mut state = Tab4State::new();
        state.review_focused = true;
        state.cursor_line = 2;

        let flat = flatten_file_diff(&make_added_diff());
        let entered_comment = state.press_space(&flat);

        assert!(
            !entered_comment,
            "first Space should not enter comment mode"
        );
        assert_eq!(
            state.selection_start,
            Some(2),
            "selection_start should be set to cursor_line"
        );
        assert!(!state.comment_mode);
    }

    #[test]
    fn test_tab4_press_space_ends_selection_enters_comment_mode() {
        let diff = make_added_diff();
        let flat = flatten_file_diff(&diff);
        // flat: [0] HunkHeader, [1] Added(new=1), [2] Context(new=2), [3] Removed(old=1)

        let mut state = Tab4State::new();
        state.review_focused = true;
        state.cursor_line = 1;
        state.press_space(&flat); // first Space at line 1
        state.cursor_line = 3;
        let entered_comment = state.press_space(&flat); // second Space at line 3

        assert!(entered_comment, "second Space should enter comment mode");
        assert!(state.comment_mode);
        assert_eq!(
            state.selection_start, None,
            "selection_start should be cleared"
        );
        assert_eq!(
            state.pending_range,
            Some((1, 3)),
            "pending_range should be (lo=1, hi=3)"
        );
        // pending_start_coord: flat[1].new_line = Some(1)
        assert_eq!(state.pending_start_coord, 1);
        // pending_end_coord: flat[3].old_line = Some(1) (Removed)
        assert_eq!(state.pending_end_coord, 1);
    }

    #[test]
    fn test_tab4_press_space_inverted_range() {
        // User moves cursor UP after pressing Space (selection_start > cursor_line).
        let diff = make_added_diff();
        let flat = flatten_file_diff(&diff);

        let mut state = Tab4State::new();
        state.review_focused = true;
        state.cursor_line = 3;
        state.press_space(&flat); // first Space at line 3
        state.cursor_line = 1;
        state.press_space(&flat); // second Space at line 1

        // Should normalize to (lo=1, hi=3).
        assert_eq!(state.pending_range, Some((1, 3)));
    }

    #[test]
    fn test_tab4_add_comment() {
        let diff = make_added_diff();
        let flat = flatten_file_diff(&diff);

        let mut state = Tab4State::new();
        state.review_focused = true;
        state.cursor_line = 1;
        state.press_space(&flat);
        state.cursor_line = 2;
        state.press_space(&flat);
        // Now in comment mode.
        assert!(state.comment_mode);

        state.comment_draft.insert_str("This needs refactoring");
        state.submit_draft_comment(0, "src/foo.rs");

        assert!(!state.comment_mode, "comment mode should exit after submit");
        assert_eq!(state.inline_comments.len(), 1);
        assert_eq!(state.inline_comments[0].text, "This needs refactoring");
        assert_eq!(state.inline_comments[0].file_idx, 0);
        assert_eq!(state.inline_comments[0].path, "src/foo.rs");
        assert_eq!(
            state.comment_draft.lines().join(""),
            "",
            "draft should be cleared"
        );
    }

    #[test]
    fn test_tab4_submit_comment_empty_is_noop() {
        let diff = make_added_diff();
        let flat = flatten_file_diff(&diff);

        let mut state = Tab4State::new();
        state.review_focused = true;
        state.cursor_line = 1;
        state.press_space(&flat);
        state.cursor_line = 2;
        state.press_space(&flat);

        // Submit with empty draft.
        state.submit_draft_comment(0, "src/foo.rs");

        assert!(
            !state.comment_mode,
            "comment mode should exit even on empty submit"
        );
        assert!(
            state.inline_comments.is_empty(),
            "no comment should be added for empty input"
        );
    }

    #[test]
    fn test_tab4_take_comments_formats_and_clears() {
        let diff = make_added_diff();
        let flat = flatten_file_diff(&diff);

        let mut state = Tab4State::new();
        // First comment.
        state.review_focused = true;
        state.cursor_line = 1;
        state.press_space(&flat);
        state.cursor_line = 2;
        state.press_space(&flat);
        state.comment_draft.insert_str("first comment");
        state.submit_draft_comment(0, "src/foo.rs");

        // Second comment.
        state.review_focused = true;
        state.cursor_line = 3;
        state.press_space(&flat);
        state.cursor_line = 3;
        state.press_space(&flat);
        state.comment_draft.insert_str("second comment");
        state.submit_draft_comment(0, "src/foo.rs");

        assert_eq!(state.inline_comments.len(), 2);
        let taken = state.take_comments();
        assert_eq!(taken.len(), 2);
        assert!(
            state.inline_comments.is_empty(),
            "inline_comments should be cleared after take"
        );
        // Each entry should contain the path and text.
        assert!(taken[0].contains("src/foo.rs"), "got: {}", taken[0]);
        assert!(taken[0].contains("first comment"), "got: {}", taken[0]);
        assert!(taken[1].contains("second comment"), "got: {}", taken[1]);
    }

    #[test]
    fn test_tab4_inline_comment_formatted() {
        let c = InlineComment {
            file_idx: 0,
            path: "src/foo.rs".to_string(),
            end_flat: 2,
            start_coord: 10,
            end_coord: 15,
            text: "needs fix".to_string(),
        };
        assert_eq!(c.formatted(), "src/foo.rs:10-15: needs fix");
        assert_eq!(c.formatted_header(), "src/foo.rs:10-15");

        let single = InlineComment {
            end_coord: 10,
            start_coord: 10,
            ..c
        };
        assert_eq!(single.formatted(), "src/foo.rs:10: needs fix");
    }

    #[test]
    fn test_tab4_reset_for_diffs_preserves_inline_comments() {
        let diff = make_added_diff();
        let flat = flatten_file_diff(&diff);

        let mut state = Tab4State::new();
        state.review_focused = true;
        state.cursor_line = 1;
        state.press_space(&flat);
        state.cursor_line = 2;
        state.press_space(&flat);
        state.comment_draft.insert_str("old comment");
        state.submit_draft_comment(0, "src/foo.rs");

        state.selected_file = 2;
        state.diff_scroll = 10;
        state.reset_for_diffs();

        assert_eq!(state.selected_file, 0, "selected_file should reset");
        assert_eq!(state.diff_scroll, 0, "diff_scroll should reset");
        assert_eq!(state.cursor_line, 0, "cursor_line should reset");
        assert!(!state.review_focused, "review_focused should reset");
        assert!(!state.comment_mode, "comment_mode should reset");
        assert_eq!(
            state.inline_comments.len(),
            1,
            "inline_comments should be preserved across reset"
        );
    }

    #[test]
    fn test_tab4_cancel_review_clears_everything() {
        let mut state = Tab4State::new();
        state.review_focused = true;
        state.cursor_line = 3;
        state.selection_start = Some(1);
        state.comment_mode = true;

        state.cancel_review();

        assert!(!state.review_focused);
        assert!(!state.comment_mode);
        assert_eq!(state.selection_start, None);
        assert_eq!(state.pending_range, None);
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
    fn test_tab4_selection_bounds_live() {
        let mut state = Tab4State::new();
        state.cursor_line = 3;
        state.selection_start = Some(5);
        // Live selection: lo = min(5, 3) = 3, hi = max(5, 3) = 5.
        assert_eq!(state.selection_bounds(), Some((3, 5)));
    }

    #[test]
    fn test_tab4_selection_bounds_pending() {
        let mut state = Tab4State::new();
        state.pending_range = Some((2, 4));
        state.selection_start = None;
        assert_eq!(state.selection_bounds(), Some((2, 4)));
    }

    #[test]
    fn test_tab4_selection_bounds_none() {
        let state = Tab4State::new();
        assert_eq!(state.selection_bounds(), None);
    }

    #[test]
    fn test_tab4_inline_comment_rendered_in_diff() {
        let mut state = Tab4State::new();
        let task_id = make_task_id();
        state.set_diffs(&task_id, vec![make_added_diff()]);

        // Manually add an inline comment at flat index 1 (Added line).
        state.inline_comments.push(InlineComment {
            file_idx: 0,
            path: "src/foo.rs".to_string(),
            end_flat: 1,
            start_coord: 1,
            end_coord: 1,
            text: "great addition".to_string(),
        });

        let content = render_to_string(&state, Some(&task_id));
        assert!(
            content.contains("great addition"),
            "inline comment text should appear in render; got: {content:?}"
        );
        assert!(
            content.contains("src/foo.rs"),
            "inline comment header should appear in render; got: {content:?}"
        );
    }
}
