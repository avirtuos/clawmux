//! Top-level TUI draw and input handling.
//!
//! Coordinates ratatui rendering across the layout, task list widget, and the
//! 9-tab right pane. Dispatches keyboard events to the focused widget.

use crossterm::event::{Event, KeyCode, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use tui_textarea::Input;

use crate::app::{App, CommitDialogState};
use crate::messages::AppMessage;
use crate::opencode::types::{DiffStatus, PermissionRequest};
use crate::tasks::models::{status_to_index, Task, TaskStatus, ALL_STATUSES};
use crate::tasks::TaskId;
use crate::workflow::agents::AgentKind;
use crate::workflow::transitions::WorkflowPhase;

pub mod layout;
pub mod markdown;
pub mod tabs;
pub mod task_list;

/// Inner width of the permission dialog (dialog_width - 2 border columns).
/// Used when computing wrapped line counts for scroll bounds.
const PERMISSION_DIALOG_INNER_WIDTH: u16 = 58;

/// Draws a full-screen loading status indicator.
///
/// Shows the app name centered above a status message. Used during
/// startup before the main `App` state is available.
pub fn draw_loading_screen(frame: &mut Frame, status: &str) {
    let area = frame.area();
    let text = Text::from(vec![
        Line::from(concat!("ClawMux v", env!("CARGO_PKG_VERSION"))).centered(),
        Line::from(""),
        Line::from(status).centered(),
    ]);
    let y = area.y + area.height.saturating_sub(3) / 2;
    let content_area = Rect::new(area.x, y, area.width, 3);
    frame.render_widget(Paragraph::new(text), content_area);
}

/// Syncs all tab states when the selected task changes after navigation.
///
/// Compares the newly selected task against `tab1_state.current_task_id`.
/// If different, calls [`Tab1State::reset_for_task`] to clear per-task focus
/// state. Also syncs `questions_state`, resets design/plan scroll, and updates
/// the displayed task for Tabs 4, 5, and 6.
fn sync_tabs_on_nav(app: &mut App) {
    let new_id = app.task_list_state.selected_task_id().cloned();

    if new_id != app.tab1_state.current_task_id {
        // Reset design and plan scroll offsets on task change.
        app.design_state.scroll = 0;
        app.plan_state.scroll = 0;
        match new_id {
            Some(ref id) => {
                if let Some(task) = app.task_store.get(id) {
                    // Clone to release the immutable borrow before mutating tab1_state.
                    let task = task.clone();
                    app.tab1_state.reset_for_task(&task);
                } else {
                    app.tab1_state.current_task_id = new_id.clone();
                }
            }
            None => {
                app.tab1_state.current_task_id = None;
            }
        }
    }

    if new_id != app.questions_state.current_task_id {
        match new_id {
            Some(ref id) => {
                if let Some(task) = app.task_store.get(id) {
                    let task = task.clone();
                    app.questions_state.reset_for_task(&task);
                }
            }
            None => {
                app.questions_state.current_task_id = None;
            }
        }
    }

    app.tab2_state
        .set_displayed_task(app.task_list_state.selected_task_id());
    app.tab3_state
        .set_displayed_task(app.task_list_state.selected_task_id());
    app.tab4_state
        .set_displayed_task(app.task_list_state.selected_task_id());
    app.review_state
        .set_displayed_task(app.task_list_state.selected_task_id());
}

/// The focused input context for [`footer_hint_text`].
///
/// When a textarea is active or a review pane is focused, the footer shows
/// context-specific hints instead of the normal per-tab shortcuts.
pub enum FocusedInput {
    /// No textarea or pane is focused.
    None,
    /// The supplemental-prompt textarea on Tab 0 is focused.
    Prompt,
    /// An answer textarea on the Questions tab (Tab 1) is focused.
    Answer,
    /// The review pane on Tab 7 (Code Diff) is focused for cursor browsing and line selection.
    Review,
    /// The comment draft textarea on the Code Diff tab (Tab 7) is active.
    Comment,
    /// The steering textarea on Tab 2 (Agent Activity) is focused.
    Steering,
    /// The rejection response textarea in the permission dialog is focused.
    RejectionResponse,
    /// The general review comment textarea on Tab 6 or Tab 7 is focused.
    ReviewComment,
    /// The prompt textarea on the Research tab (Tab 8) is focused.
    ResearchPrompt,
}

/// Returns the footer hint string based on the current application state.
///
/// Priority (highest first):
/// - Quit-confirm dialog visible.
/// - Status picker visible.
/// - A textarea or review pane is focused (editing/browsing mode).
/// - Tab 0 specific states (malformed task, startable task, normal).
/// - Tab 1 (Questions): answer and navigation bindings.
/// - Tabs 2-3 (Design/Plan): scroll bindings.
/// - Tab 4 (Agent Activity): permission pending, steer and scroll bindings.
/// - Tab 5 (Team Status): approval gate, scroll bindings.
/// - Tab 6 (Review): review discussion scroll and action bindings.
/// - Tab 7 (Code Diff): review mode, approve, and revision bindings.
/// - On other tabs: shows minimal bindings.
#[allow(clippy::too_many_arguments)]
pub fn footer_hint_text(
    show_quit_confirm: bool,
    show_status_picker: bool,
    active_tab: usize,
    focused_input: FocusedInput,
    is_malformed_task: bool,
    is_startable_task: bool,
    is_resumable_task: bool,
    pending_permission: bool,
    awaiting_approval: bool,
) -> &'static str {
    if show_quit_confirm {
        "[y/Enter] confirm quit | [n/Esc] cancel"
    } else if show_status_picker {
        "[1-5] select | [Up/Down] navigate | [Enter] confirm | [Esc] cancel"
    } else if matches!(focused_input, FocusedInput::ReviewComment) {
        "[Esc] exit | [Enter] submit | Editing review comment"
    } else if matches!(focused_input, FocusedInput::Prompt) {
        "[Esc] exit | [Enter] exit | Editing prompt"
    } else if matches!(focused_input, FocusedInput::Answer) {
        "[Esc] exit | [Tab] next answer | [Enter] submit | Editing answer"
    } else if matches!(focused_input, FocusedInput::Review) {
        "[Esc] exit | [Up/Down] cursor | [PgUp/PgDn] files | [Space] select | [a] approve"
    } else if matches!(focused_input, FocusedInput::Comment) {
        "[Esc] cancel | [Enter] save | Editing comment"
    } else if matches!(focused_input, FocusedInput::Steering) {
        "[Esc] exit | [Enter] send | Editing steering prompt"
    } else if matches!(focused_input, FocusedInput::RejectionResponse) {
        "[Enter] submit | [Esc] cancel | Editing rejection response"
    } else if matches!(focused_input, FocusedInput::ResearchPrompt) {
        "[Esc] exit | [Enter] send | Editing research prompt"
    } else if active_tab == 0 && is_malformed_task {
        "[f] request fix | [Enter] apply fix | [PgUp/PgDn] switch tasks | [Tab] next tab | [q] quit"
    } else if active_tab == 0 && is_startable_task {
        "[Enter] start | [p] prompt | [s] status | [PgUp/PgDn] switch tasks | [Tab] next tab | [q] quit"
    } else if active_tab == 0 && is_resumable_task {
        "[Enter] resume | [s] status | [PgUp/PgDn] switch tasks | [Tab] next tab | [q] quit"
    } else if active_tab == 0 {
        "[p] prompt | [s] status | [PgUp/PgDn] switch tasks | [Tab] next tab | [q] quit"
    } else if active_tab == 1 {
        "[p] answer | [Enter] submit | [Up/Down] navigate | [Tab] next tab | [q] quit"
    } else if active_tab == 2 || active_tab == 3 {
        "[Up/Down] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 4 && pending_permission {
        "[y] approve | [a] always | [n] reject | [r] reject with response | [Up/Down] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 4 {
        "[p] steer | [Enter] send | [Up/Down] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 5 && awaiting_approval {
        "[Ctrl+N] next agent | [Up/Down] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 5 {
        "[Up/Down] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 6 {
        "[a] approve | [p] comment | [R] revisions | [Up/Down] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 7 {
        "[r] review | [a] approve | [p] comment | [R] revisions | [Up/Down] scroll | [Tab] next tab | [q] quit"
    } else if active_tab == 8 {
        "[p] prompt | [Up/Down] scroll | [Tab] next tab | [q] quit"
    } else {
        "[Tab] next tab | [q] quit"
    }
}

/// Returns a short status string describing the selected task's state and active agent.
///
/// Shown in the footer right section at all times so the user always knows the current
/// task state without having to look at the task list. Returns `"No Task Selected"` when
/// no task is focused. For tasks assigned to `Human` (i.e. no automated agent active),
/// returns `"No Active Agent"` in the agent position.
fn team_status_text(task: Option<&Task>) -> String {
    let Some(task) = task else {
        return "No Task Selected".to_string();
    };
    let status_label = match task.status {
        TaskStatus::Open => "Open",
        TaskStatus::InProgress => "In Progress",
        TaskStatus::PendingReview => "Pending Review",
        TaskStatus::Completed => "Completed",
        TaskStatus::Abandoned => "Abandoned",
    };
    let agent_label = task
        .assigned_to
        .filter(|a| *a != AgentKind::Human)
        .map(|a| a.display_name())
        .unwrap_or("No Active Agent");
    format!("{} - {}", status_label, agent_label)
}

/// Draws the full TUI frame with layout and task list widget.
///
/// Renders left pane (task list), right pane, and footer using the computed layout regions.
pub fn draw(frame: &mut Frame, app: &App) {
    let areas = layout::render_layout(frame.area());

    task_list::render(
        frame,
        areas.left_pane,
        &app.task_list_state,
        &app.cached_stories,
    );

    tabs::render(frame, areas.right_pane, app);

    let selected_task = app.selected_task().and_then(|id| app.task_store.get(id));
    let is_malformed_task = selected_task.map(|t| t.is_malformed()).unwrap_or(false);
    let is_startable_task = selected_task
        .map(|t| !t.is_malformed() && t.status == TaskStatus::Open)
        .unwrap_or(false);
    let is_resumable_task = selected_task
        .map(|t| !t.is_malformed() && t.status == TaskStatus::InProgress)
        .unwrap_or(false);
    let focused_input = if app.tab1_state.prompt_focused {
        FocusedInput::Prompt
    } else if app.questions_state.focused_answer.is_some() {
        FocusedInput::Answer
    } else if app.tab4_state.review_comment_focused {
        FocusedInput::ReviewComment
    } else if app.tab4_state.comment_mode {
        FocusedInput::Comment
    } else if app.tab4_state.review_focused {
        FocusedInput::Review
    } else if app.tab2_state.rejection_response_focused {
        FocusedInput::RejectionResponse
    } else if app.tab2_state.steering_focused {
        FocusedInput::Steering
    } else if app.research_state.prompt_focused {
        FocusedInput::ResearchPrompt
    } else {
        FocusedInput::None
    };
    let pending_permission = app
        .selected_task()
        .map(|id| {
            app.tab2_state
                .pending_permissions
                .front()
                .map(|(tid, _)| tid == id)
                .unwrap_or(false)
        })
        .unwrap_or(false);
    let awaiting_agent = app
        .selected_task()
        .and_then(|id| app.workflow_engine.state(id))
        .and_then(|s| match &s.phase {
            WorkflowPhase::AwaitingApproval { next_agent, .. } => Some(*next_agent),
            _ => None,
        });
    let awaiting_approval = awaiting_agent.is_some();
    let hint = footer_hint_text(
        app.show_quit_confirm,
        app.show_status_picker.is_some(),
        app.active_tab,
        focused_input,
        is_malformed_task,
        is_startable_task,
        is_resumable_task,
        pending_permission,
        awaiting_approval,
    );
    let right_status: Option<String> = {
        let base = app.tab2_state.any_thinking_status().or_else(|| {
            awaiting_agent.map(|a| format!("pending approval for {}", a.display_name()))
        });
        let tokens = app
            .selected_task()
            .and_then(|id| app.tab2_state.get_tokens(id));
        match (base, tokens) {
            (Some(b), Some((inp, out))) => Some(format!(
                "{} | in:{} out:{}",
                b,
                format_tokens(inp),
                format_tokens(out)
            )),
            (Some(b), None) => Some(b),
            (None, Some((inp, out))) => Some(format!(
                "in:{} out:{}",
                format_tokens(inp),
                format_tokens(out)
            )),
            (None, None) => Some(team_status_text(selected_task)),
        }
    };
    let footer_block = Block::default().borders(Borders::TOP);
    let footer_inner = footer_block.inner(areas.footer);
    frame.render_widget(footer_block, areas.footer);

    const VERSION: &str = concat!("ClawMux v", env!("CARGO_PKG_VERSION"), " ");
    let version_width = VERSION.len() as u16;

    if let Some(ref status_text) = right_status {
        let status_width = status_text.len() as u16 + 1;
        let footer_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(version_width),
                Constraint::Min(0),
                Constraint::Length(status_width),
            ])
            .split(footer_inner);
        frame.render_widget(Paragraph::new(VERSION), footer_layout[0]);
        frame.render_widget(Paragraph::new(hint), footer_layout[1]);
        frame.render_widget(
            Paragraph::new(status_text.as_str()).style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::ITALIC),
            ),
            footer_layout[2],
        );
    } else {
        let footer_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(version_width), Constraint::Min(0)])
            .split(footer_inner);
        frame.render_widget(Paragraph::new(VERSION), footer_layout[0]);
        frame.render_widget(Paragraph::new(hint), footer_layout[1]);
    }

    if app.show_quit_confirm {
        render_quit_confirm_dialog(frame, frame.area());
    }

    if let Some(ref dialog) = app.commit_dialog {
        render_commit_dialog(frame, frame.area(), dialog);
    }

    if let Some((ref _tid, ref request)) = app.tab2_state.pending_permissions.front() {
        render_permission_dialog(
            frame,
            frame.area(),
            request,
            app.tab2_state.rejection_response_focused,
            &app.tab2_state.rejection_response,
            app.tab2_state.permission_scroll,
        );
    }

    if let Some(selected_idx) = app.show_status_picker {
        let current_status = app
            .selected_task()
            .and_then(|id| app.task_store.get(id))
            .map(|t| t.status.clone())
            .unwrap_or(TaskStatus::Open);
        render_status_picker_dialog(frame, frame.area(), selected_idx, &current_status);
    }
}

/// Formats a token count for compact display: `1234` -> `"1234"`, `12345` -> `"12.3k"`, etc.
fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format!("{}", n)
    }
}

/// Renders a centered modal quit confirmation dialog over the given area.
///
/// Blanks a 40x5 centered region using [`Clear`] then draws a bordered paragraph
/// with a yellow border asking the user to confirm or cancel quitting.
fn render_quit_confirm_dialog(frame: &mut Frame, area: Rect) {
    let dialog_width = 40u16;
    let dialog_height = 5u16;
    let x = area.x + area.width.saturating_sub(dialog_width) / 2;
    let y = area.y + area.height.saturating_sub(dialog_height) / 2;
    let dialog_area = Rect::new(
        x,
        y,
        dialog_width.min(area.width),
        dialog_height.min(area.height),
    );

    frame.render_widget(Clear, dialog_area);
    let block = Block::default()
        .title(" Quit ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let paragraph = Paragraph::new("Are you sure you want to quit?")
        .block(block)
        .alignment(ratatui::layout::Alignment::Center);
    frame.render_widget(paragraph, dialog_area);
}

/// Renders a centered modal commit confirmation dialog over the given area.
///
/// Shows the list of changed files (capped at 10 visible rows) and an editable
/// textarea pre-filled with the proposed commit message.
/// `[Ctrl+S]` confirms; `[Enter]` inserts a newline; `[Esc]` cancels.
fn render_commit_dialog(frame: &mut Frame, area: Rect, dialog: &CommitDialogState) {
    let dialog_width = 70u16;
    // Layout: 2 borders + file list (capped at 10) + 1 spacer + 6 editor rows + 1 hint = varies.
    let file_rows = (dialog.file_summary.len() as u16).min(10);
    let dialog_height = 2 + file_rows + 1 + 6 + 1;

    let x = area.x + area.width.saturating_sub(dialog_width) / 2;
    let y = area.y + area.height.saturating_sub(dialog_height) / 2;
    let dialog_area = Rect::new(
        x,
        y,
        dialog_width.min(area.width),
        dialog_height.min(area.height),
    );

    frame.render_widget(Clear, dialog_area);
    let block = Block::default()
        .title(" Commit Changes ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let [files_area, _spacer, editor_area, hint_area] = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(file_rows),
            Constraint::Length(1),
            Constraint::Length(6),
            Constraint::Length(1),
        ])
        .areas(inner);

    // Render file list with status prefix and color.
    let file_lines: Vec<Line> = dialog
        .file_summary
        .iter()
        .take(10)
        .map(|(path, status)| {
            let (prefix, color) = match status {
                DiffStatus::Added => ("[A]", Color::Green),
                DiffStatus::Modified => ("[M]", Color::Yellow),
                DiffStatus::Deleted => ("[D]", Color::Red),
            };
            Line::from(vec![
                ratatui::text::Span::styled(format!("{} ", prefix), Style::default().fg(color)),
                ratatui::text::Span::raw(path.clone()),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(file_lines), files_area);

    // Render editable commit message textarea.
    frame.render_widget(&dialog.editor, editor_area);

    // Render hint line.
    frame.render_widget(
        Paragraph::new(Line::from("[Ctrl+S] commit | [Esc] cancel")),
        hint_area,
    );
}

/// Renders a centered modal permission request dialog over the given area.
///
/// The dialog is fixed at 60 columns wide. Pattern lines are word-wrapped inside
/// a 4-row scrollable viewport; `permission_scroll` shifts the viewport so the
/// user can read long commands with Up/Down arrows.
///
/// When `rejection_focused` is `true`, the dialog expands by 4 rows and renders
/// the `rejection_response` textarea so the user can type a guidance note.
fn render_permission_dialog(
    frame: &mut Frame,
    area: Rect,
    request: &PermissionRequest,
    rejection_focused: bool,
    rejection_response: &tui_textarea::TextArea<'_>,
    permission_scroll: u16,
) {
    // Fixed layout: 2 borders + 1 type + 4 pattern viewport + 1 spacer + 2 hint = 10 rows.
    // Hint is 2 rows to accommodate the wrapped key-binding text.
    // Rejection mode adds 4 rows for the guidance textarea.
    let dialog_width = 60u16;
    let extra_rows = if rejection_focused { 4u16 } else { 0u16 };
    let dialog_height = 10u16 + extra_rows;
    let x = area.x + area.width.saturating_sub(dialog_width) / 2;
    let y = area.y + area.height.saturating_sub(dialog_height) / 2;
    let dialog_area = Rect::new(
        x,
        y,
        dialog_width.min(area.width),
        dialog_height.min(area.height),
    );

    frame.render_widget(Clear, dialog_area);
    let block = Block::default()
        .title(" Permission Request ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let type_line = Line::from(format!("Type: {}", request.permission));
    let pattern_lines: Vec<Line> = request
        .patterns
        .iter()
        .map(|p| Line::from(format!("  cmd: {}", p)))
        .collect();

    if rejection_focused {
        // Layout: type(1) | patterns(4) | rejection textarea(4) | hint(1)
        let [type_area, patterns_area, rejection_area, hint_area] =
            ratatui::layout::Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(1),
                    Constraint::Length(4),
                    Constraint::Length(4),
                    Constraint::Length(1),
                ])
                .areas(inner);
        frame.render_widget(Paragraph::new(type_line), type_area);
        frame.render_widget(
            Paragraph::new(pattern_lines)
                .wrap(Wrap { trim: false })
                .scroll((permission_scroll, 0)),
            patterns_area,
        );
        frame.render_widget(rejection_response, rejection_area);
        frame.render_widget(
            Paragraph::new(Line::from("[Enter] submit | [Esc] cancel")),
            hint_area,
        );
    } else {
        // Layout: type(1) | patterns(4) | spacer(1) | hint(2)
        let [type_area, patterns_area, _spacer, hint_area] = ratatui::layout::Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(4),
                Constraint::Length(1),
                Constraint::Length(2),
            ])
            .areas(inner);
        frame.render_widget(Paragraph::new(type_line), type_area);
        frame.render_widget(
            Paragraph::new(pattern_lines)
                .wrap(Wrap { trim: false })
                .scroll((permission_scroll, 0)),
            patterns_area,
        );
        frame.render_widget(
            Paragraph::new(Line::from(
                "[y] approve once | [a] always allow | [n] reject | [r] reject with response | [Up/Down] scroll",
            ))
            .wrap(Wrap { trim: false }),
            hint_area,
        );
    }
}

/// Renders a centered modal status picker dialog over the given area.
///
/// Shows all 5 task statuses, with the highlighted entry in yellow bold
/// and the current status marked with `*`.
fn render_status_picker_dialog(
    frame: &mut Frame,
    area: Rect,
    selected_idx: usize,
    current_status: &TaskStatus,
) {
    let dialog_width = 36u16;
    let dialog_height = 10u16;
    let x = area.x + area.width.saturating_sub(dialog_width) / 2;
    let y = area.y + area.height.saturating_sub(dialog_height) / 2;
    let dialog_area = Rect::new(
        x,
        y,
        dialog_width.min(area.width),
        dialog_height.min(area.height),
    );

    frame.render_widget(Clear, dialog_area);

    let current_idx = status_to_index(current_status);
    let lines: Vec<Line> = ALL_STATUSES
        .iter()
        .enumerate()
        .map(|(i, status)| {
            let marker = if i == current_idx { "*" } else { " " };
            let text = format!("{}{}.  {}", marker, i + 1, status);
            if i == selected_idx {
                Line::from(text).style(
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Line::from(text)
            }
        })
        .collect();

    let block = Block::default()
        .title(" Status ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, dialog_area);
}

/// Applies a status change by index, mutating the selected task in the store.
///
/// Returns `Some(TaskUpdated)` to trigger persistence, or `None` if the index is
/// out of range or no task is selected.
fn apply_status_change(app: &mut App, status_idx: usize) -> Option<AppMessage> {
    let new_status = ALL_STATUSES.get(status_idx)?.clone();
    let task_id = app.selected_task()?.clone();
    if let Some(task) = app.task_store.get_mut(&task_id) {
        task.status = new_status;
    }
    Some(AppMessage::TaskUpdated { task_id })
}

/// Attempts to submit the steering prompt textarea content as a mid-session prompt.
///
/// If the workflow is in the Running phase with an active session, sends immediately
/// via `SendPrompt`. Otherwise queues the prompt (max 1; replaces any existing queued
/// prompt) and pushes a `[You - queued]` banner so the user sees their input was
/// accepted. Returns `None` if the textarea is empty or no task is selected.
fn submit_steering_prompt(app: &mut App) -> Option<AppMessage> {
    let text: String = app.tab2_state.steering_input.lines().join("\n");
    if text.trim().is_empty() {
        return None;
    }
    let task_id = app.selected_task()?.clone();
    // Always clear and unfocus the textarea on submit.
    app.tab2_state.reset_steering();

    // If there is an active running session, send immediately.
    let active_session = app.workflow_engine.state(&task_id).and_then(|s| {
        if s.phase == WorkflowPhase::Running {
            s.session_id.clone()
        } else {
            None
        }
    });

    if let Some(session_id) = active_session {
        app.tab2_state
            .push_banner(&task_id, format!("[You] {}", text));
        return Some(AppMessage::SendPrompt {
            task_id,
            session_id,
            prompt: text,
        });
    }

    // No active session -- queue the prompt for dispatch at the end of the next turn.
    app.tab2_state
        .push_banner(&task_id, format!("[You - queued] {}", text));
    app.tab2_state.queue_prompt(task_id, text);
    None
}

/// Extracts the text from the Research tab prompt textarea, clears it, and returns
/// a [`AppMessage::ResearchPromptSubmitted`] if non-empty.
fn submit_research_prompt(app: &mut App) -> Option<AppMessage> {
    let text: String = app.research_state.prompt_input.lines().join("\n");
    if text.trim().is_empty() {
        return None;
    }
    app.research_state.prompt_input = tui_textarea::TextArea::default();
    app.research_state.set_prompt_unfocused_style();
    app.research_state.prompt_focused = false;
    Some(AppMessage::ResearchPromptSubmitted { prompt: text })
}

/// Maps an `answer_inputs` index to the corresponding `task.questions` index.
///
/// `answer_inputs` only covers unanswered questions in order. This function
/// finds the `task.questions` position for the N-th unanswered question.
fn map_answer_idx_to_question_idx(app: &App, task_id: &TaskId, answer_idx: usize) -> usize {
    app.task_store
        .get(task_id)
        .map(|t| {
            t.questions
                .iter()
                .enumerate()
                .filter(|(_, q)| q.answer.is_none())
                .nth(answer_idx)
                .map(|(i, _)| i)
                .unwrap_or(0)
        })
        .unwrap_or(0)
}

/// Finds the `answer_inputs` index for the currently selected question, if unanswered.
///
/// `selected_question` is a display index (0 = newest). This function converts it to
/// the underlying `task.questions` index before computing the `answer_inputs` position.
/// Returns `None` if the selected question is already answered or there is no task.
fn find_answer_idx_for_selected(app: &App) -> Option<usize> {
    let task = app.selected_task().and_then(|id| app.task_store.get(id))?;
    let display_idx = app.questions_state.selected_question;
    let total = task.questions.len();
    let sel = tabs::questions::display_to_question_idx(display_idx, total);
    let q = task.questions.get(sel)?;
    if q.answer.is_some() {
        return None;
    }
    // Count unanswered questions appearing before sel.
    Some(
        task.questions[..sel]
            .iter()
            .filter(|q| q.answer.is_none())
            .count(),
    )
}

/// Converts a crossterm event into an optional [`AppMessage`], mutating `app` for navigation.
///
/// - `PgUp` / `k` -> move task list selection up
/// - `PgDn` / `j` -> move task list selection down
/// - `Up` / `Down` -> scroll the active right pane
/// - `Enter` / `Space` -> toggle story expansion (no-op if a task is selected)
/// - `Tab` -> cycle `app.active_tab` (0-6)
/// - `q` (no modifiers) -> [`AppMessage::Shutdown`]
/// - `Ctrl-C` -> [`AppMessage::Shutdown`]
/// - Any other key -> `None`
pub fn handle_input(event: Event, app: &mut App) -> Option<AppMessage> {
    if let Event::Key(key) = event {
        // When the quit confirmation dialog is visible, intercept all input.
        if app.show_quit_confirm {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    return Some(AppMessage::Shutdown);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    app.dismiss_quit_confirm();
                    return None;
                }
                _ => return None,
            }
        }

        // When the status picker is open, intercept all input.
        if let Some(selected_idx) = app.show_status_picker {
            match key.code {
                KeyCode::Esc => {
                    app.dismiss_status_picker();
                    return None;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.show_status_picker = Some(selected_idx.saturating_sub(1));
                    return None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.show_status_picker = Some((selected_idx + 1).min(4));
                    return None;
                }
                KeyCode::Char(c @ '1'..='5') => {
                    let idx = (c as usize) - ('1' as usize);
                    app.show_status_picker = None;
                    return apply_status_change(app, idx);
                }
                KeyCode::Enter => {
                    app.show_status_picker = None;
                    return apply_status_change(app, selected_idx);
                }
                _ => return None, // swallow all other keys
            }
        }

        // When a permission dialog is visible, intercept y/a/n/r regardless of active tab.
        let has_pending = app
            .selected_task()
            .map(|id| {
                app.tab2_state
                    .pending_permissions
                    .front()
                    .map(|(tid, _)| tid == id)
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if has_pending {
            // If the rejection response textarea is focused, handle its input first.
            if app.tab2_state.rejection_response_focused {
                match key.code {
                    KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                        let text: String = app.tab2_state.rejection_response.lines().join("\n");
                        app.tab2_state.reset_rejection_response();
                        if let Some((task_id, request)) =
                            app.tab2_state.pending_permissions.front().cloned()
                        {
                            let explanation = if text.trim().is_empty() {
                                None
                            } else {
                                Some(format!("No, let's consider something else first. {}", text))
                            };
                            return Some(AppMessage::PermissionResolved {
                                task_id,
                                request,
                                response: "reject".to_string(),
                                explanation,
                            });
                        }
                        return None;
                    }
                    KeyCode::Esc => {
                        app.tab2_state.reset_rejection_response();
                        return None;
                    }
                    _ => {
                        app.tab2_state.rejection_response.input(Input::from(key));
                        return None;
                    }
                }
            }

            // Up/Down scroll the pattern viewport; intercept before tab-level scroll handling.
            match key.code {
                KeyCode::Up if key.modifiers == KeyModifiers::NONE => {
                    app.tab2_state.permission_scroll =
                        app.tab2_state.permission_scroll.saturating_sub(1);
                    return None;
                }
                KeyCode::Down if key.modifiers == KeyModifiers::NONE => {
                    let max_scroll =
                        if let Some((_, req)) = app.tab2_state.pending_permissions.front() {
                            let lines: Vec<Line> = req
                                .patterns
                                .iter()
                                .map(|p| Line::from(format!("  cmd: {}", p)))
                                .collect();
                            let total = Paragraph::new(lines)
                                .wrap(Wrap { trim: false })
                                .line_count(PERMISSION_DIALOG_INNER_WIDTH); // dialog_width - 2 borders
                            u16::try_from(total.saturating_sub(4)).unwrap_or(u16::MAX)
                        } else {
                            0
                        };
                    app.tab2_state.permission_scroll = app
                        .tab2_state
                        .permission_scroll
                        .saturating_add(1)
                        .min(max_scroll);
                    return None;
                }
                _ => {}
            }

            let response = match key.code {
                KeyCode::Char('y') if key.modifiers == KeyModifiers::NONE => {
                    Some("once".to_string())
                }
                KeyCode::Char('a') if key.modifiers == KeyModifiers::NONE => {
                    Some("always".to_string())
                }
                KeyCode::Char('n') if key.modifiers == KeyModifiers::NONE => {
                    Some("reject".to_string())
                }
                KeyCode::Char('r') if key.modifiers == KeyModifiers::NONE => {
                    app.tab2_state.focus_rejection_response();
                    return None;
                }
                _ => None,
            };
            if let Some(resp) = response {
                if let Some((task_id, request)) =
                    app.tab2_state.pending_permissions.front().cloned()
                {
                    return Some(AppMessage::PermissionResolved {
                        task_id,
                        request,
                        response: resp,
                        explanation: None,
                    });
                }
            }
            // Consume all other keys while permission dialog is active.
            return None;
        }

        // When the commit dialog is open, intercept all input.
        // This block runs AFTER the permission dialog check so that a permission
        // request arriving while the commit dialog is open is handled correctly.
        if app.commit_dialog.is_some() {
            match key.code {
                // Ctrl+S or Alt+Enter confirms the commit.
                // Bare Enter falls through to the textarea to insert a newline.
                KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    let message = app
                        .commit_dialog
                        .as_ref()
                        .unwrap()
                        .editor
                        .lines()
                        .join("\n");
                    if message.trim().is_empty() {
                        return None;
                    }
                    let dialog = app.commit_dialog.take().unwrap();
                    let file_paths: Vec<String> =
                        dialog.file_summary.iter().map(|(p, _)| p.clone()).collect();
                    return Some(AppMessage::HumanApprovedCommit {
                        task_id: dialog.task_id,
                        commit_message: message,
                        file_paths,
                    });
                }
                KeyCode::Enter
                    if key.modifiers.intersects(
                        KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::META,
                    ) =>
                {
                    // Check for empty message without consuming the dialog.
                    let message = app
                        .commit_dialog
                        .as_ref()
                        .unwrap()
                        .editor
                        .lines()
                        .join("\n");
                    if message.trim().is_empty() {
                        // Keep dialog open; user must provide a message.
                        return None;
                    }
                    let dialog = app.commit_dialog.take().unwrap();
                    let file_paths: Vec<String> =
                        dialog.file_summary.iter().map(|(p, _)| p.clone()).collect();
                    return Some(AppMessage::HumanApprovedCommit {
                        task_id: dialog.task_id,
                        commit_message: message,
                        file_paths,
                    });
                }
                KeyCode::Esc => {
                    app.commit_dialog = None;
                    return None;
                }
                _ => {
                    app.commit_dialog
                        .as_mut()
                        .unwrap()
                        .editor
                        .input(Input::from(key));
                    return None;
                }
            }
        }

        // Tab 0 (Details): textarea focus and task actions.
        if app.active_tab == 0 {
            // Check if a malformed task is selected.
            let selected_malformed_task_id = app
                .selected_task()
                .and_then(|id| app.task_store.get(id))
                .filter(|t| t.is_malformed())
                .map(|t| t.id.clone());

            if let Some(task_id) = selected_malformed_task_id {
                match key.code {
                    KeyCode::Char('f') if key.modifiers == KeyModifiers::NONE => {
                        return Some(AppMessage::RequestTaskFix { task_id });
                    }
                    KeyCode::Enter => {
                        // Only emit ApplyTaskFix if there is a suggestion ready.
                        let has_fix = app
                            .task_store
                            .get(&task_id)
                            .and_then(|t| t.parse_error.as_ref())
                            .map(|e| e.suggested_fix.is_some())
                            .unwrap_or(false);
                        if has_fix {
                            return Some(AppMessage::ApplyTaskFix { task_id });
                        }
                    }
                    KeyCode::Up => {
                        app.tab1_state.scroll_desc_up();
                        return None;
                    }
                    KeyCode::Down => {
                        app.tab1_state.scroll_desc_down();
                        return None;
                    }
                    _ => {}
                }
                // Fall through to shared navigation handling below.
            } else {
                if app.tab1_state.prompt_focused {
                    if key.code == KeyCode::Esc
                        || (key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE)
                    {
                        app.tab1_state.prompt_focused = false;
                        app.tab1_state.set_prompt_unfocused_style();
                    } else {
                        app.tab1_state.prompt_input.input(Input::from(key));
                    }
                    return None;
                }
                // Enter focus on the supplemental prompt with 'p'.
                if key.code == KeyCode::Char('p') && key.modifiers == KeyModifiers::NONE {
                    app.tab1_state.prompt_focused = true;
                    app.tab1_state.set_prompt_focused_style();
                    return None;
                }
                // Open the status picker with 's' when a task is selected.
                if key.code == KeyCode::Char('s')
                    && key.modifiers == KeyModifiers::NONE
                    && app.selected_task().is_some()
                {
                    app.open_status_picker();
                    return None;
                }
                // Start an OPEN task or resume an INPROGRESS task with Enter.
                if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE {
                    if let Some(task_id) = app.selected_task().cloned() {
                        let status = app.task_store.get(&task_id).map(|t| t.status.clone());
                        match status {
                            Some(TaskStatus::Open) => {
                                return Some(AppMessage::StartTask { task_id });
                            }
                            Some(TaskStatus::InProgress) => {
                                return Some(AppMessage::ResumeTask { task_id });
                            }
                            _ => {}
                        }
                    }
                }
                // Scroll the description paragraph with Up/Down (no textarea focused).
                match key.code {
                    KeyCode::Up => {
                        app.tab1_state.scroll_desc_up();
                        return None;
                    }
                    KeyCode::Down => {
                        app.tab1_state.scroll_desc_down();
                        return None;
                    }
                    _ => {}
                }
            }
        }

        // Tab 1 (Questions): answer textarea input and question navigation.
        if app.active_tab == 1 {
            if let Some(idx) = app.questions_state.focused_answer {
                // A textarea is focused: forward most keys to it.
                if key.code == KeyCode::Esc {
                    app.questions_state.set_answer_unfocused_style(idx);
                    app.questions_state.focused_answer = None;
                } else if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE {
                    // Submit the answer.
                    let answer_text: String =
                        app.questions_state.answer_inputs[idx].lines().join("\n");
                    if let Some(task_id) = app.selected_task().cloned() {
                        let question_index = map_answer_idx_to_question_idx(app, &task_id, idx);
                        app.questions_state.set_answer_unfocused_style(idx);
                        app.questions_state.focused_answer = None;
                        return Some(AppMessage::HumanAnswered {
                            task_id,
                            question_index,
                            answer: answer_text,
                        });
                    }
                } else if key.code == KeyCode::Tab && key.modifiers == KeyModifiers::NONE {
                    // Cycle to next answer textarea.
                    let len = app.questions_state.answer_inputs.len();
                    if len > 0 {
                        let new_idx = (idx + 1) % len;
                        app.questions_state.set_answer_unfocused_style(idx);
                        app.questions_state.focused_answer = Some(new_idx);
                        app.questions_state.set_answer_focused_style(new_idx);
                    }
                } else if let Some(ta) = app.questions_state.answer_inputs.get_mut(idx) {
                    ta.input(Input::from(key));
                }
                return None;
            }

            // No textarea focused: handle navigation and focus entry.
            match key.code {
                KeyCode::Up => {
                    app.questions_state.select_prev();
                    return None;
                }
                KeyCode::Down => {
                    let total = app
                        .selected_task()
                        .and_then(|id| app.task_store.get(id))
                        .map(|t| t.questions.len())
                        .unwrap_or(0);
                    app.questions_state.select_next(total);
                    return None;
                }
                KeyCode::Char('p') if key.modifiers == KeyModifiers::NONE => {
                    // Focus the answer textarea for the currently selected question.
                    if !app.questions_state.answer_inputs.is_empty() {
                        if let Some(answer_idx) = find_answer_idx_for_selected(app) {
                            app.questions_state.focused_answer = Some(answer_idx);
                            app.questions_state.set_answer_focused_style(answer_idx);
                        }
                    }
                    return None;
                }
                _ => {}
            }
        }

        // Tab 2 (Design): scroll.
        if app.active_tab == 2 {
            match key.code {
                KeyCode::Up => {
                    app.design_state.scroll_up();
                    return None;
                }
                KeyCode::Down => {
                    app.design_state.scroll_down();
                    return None;
                }
                _ => {}
            }
        }

        // Tab 3 (Plan): scroll.
        if app.active_tab == 3 {
            match key.code {
                KeyCode::Up => {
                    app.plan_state.scroll_up();
                    return None;
                }
                KeyCode::Down => {
                    app.plan_state.scroll_down();
                    return None;
                }
                _ => {}
            }
        }

        // Tab 4 (Agent Activity): steering prompt focus and scroll.
        if app.active_tab == 4 {
            if app.tab2_state.steering_focused {
                // Focused: all keys go to textarea except Esc and Enter (which submits).
                if key.code == KeyCode::Esc {
                    app.tab2_state.steering_focused = false;
                    app.tab2_state.set_steering_unfocused_style();
                    return None;
                }
                if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE {
                    return submit_steering_prompt(app);
                }
                // Forward all other keys to the textarea.
                app.tab2_state.steering_input.input(Input::from(key));
                return None;
            }
            // Unfocused mode.
            match key.code {
                KeyCode::Char('p') if key.modifiers == KeyModifiers::NONE => {
                    if app.selected_task().is_some() {
                        app.tab2_state.steering_focused = true;
                        app.tab2_state.set_steering_focused_style();
                    }
                    return None;
                }
                KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                    if let Some(msg) = submit_steering_prompt(app) {
                        return Some(msg);
                    }
                    return None;
                }
                KeyCode::Up => {
                    app.tab2_state.scroll_up();
                    return None;
                }
                KeyCode::Down => {
                    app.tab2_state.scroll_down();
                    return None;
                }
                _ => {}
            }
        }

        // Tab 5 (Team Status): scroll.
        if app.active_tab == 5 {
            match key.code {
                KeyCode::Up => {
                    app.tab3_state.scroll_up();
                    return None;
                }
                KeyCode::Down => {
                    let max = app
                        .selected_task()
                        .and_then(|id| app.task_store.get(id))
                        .map_or(0, |t| t.work_log.len());
                    app.tab3_state.scroll_down(max);
                    return None;
                }
                _ => {}
            }
        }

        // Tab 6 (Review Discussion): approve, request revisions, scroll.
        if app.active_tab == 6 {
            // General comment box input handling takes priority.
            if app.tab4_state.review_comment_focused {
                match key.code {
                    KeyCode::Esc => {
                        app.tab4_state.unfocus_review_comment();
                        return None;
                    }
                    KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                        app.tab4_state.submit_review_comment();
                        return None;
                    }
                    _ => {
                        app.tab4_state.review_comment.input(Input::from(key));
                        return None;
                    }
                }
            }

            match key.code {
                KeyCode::Up => {
                    app.review_state.scroll_up();
                    return None;
                }
                KeyCode::Down => {
                    app.review_state.scroll_down();
                    return None;
                }
                KeyCode::Char('a') if key.modifiers == KeyModifiers::NONE => {
                    if let Some(task_id) = app.selected_task().cloned() {
                        app.open_commit_dialog(&task_id);
                    }
                    return None;
                }
                KeyCode::Char('p') if key.modifiers == KeyModifiers::NONE => {
                    if app.selected_task().is_some() {
                        app.tab4_state.focus_review_comment();
                    }
                    return None;
                }
                // Shift+R: request revisions using accumulated inline and general comments.
                KeyCode::Char('R') => {
                    if let Some(task_id) = app.selected_task().cloned() {
                        let comments = app.tab4_state.take_comments();
                        return Some(AppMessage::HumanRequestedRevisions { task_id, comments });
                    }
                    return None;
                }
                _ => {}
            }
        }

        // Tab 7 (Code Diff): review pane focus, line cursor navigation, selection, comments.
        if app.active_tab == 7 {
            // Comment mode: user is typing a comment draft.
            if app.tab4_state.comment_mode {
                match key.code {
                    KeyCode::Esc => {
                        app.tab4_state.cancel_review();
                        return None;
                    }
                    KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                        let (file_idx, path) = {
                            let diffs = app.tab4_state.current_diffs();
                            let idx = app
                                .tab4_state
                                .selected_file
                                .min(diffs.len().saturating_sub(1));
                            let path = diffs.get(idx).map(|d| d.path.clone()).unwrap_or_default();
                            (idx, path)
                        };
                        app.tab4_state.submit_draft_comment(file_idx, &path);
                        return None;
                    }
                    _ => {
                        app.tab4_state.comment_draft.input(Input::from(key));
                        return None;
                    }
                }
            }

            // General comment box: user is typing a general review comment.
            if app.tab4_state.review_comment_focused {
                match key.code {
                    KeyCode::Esc => {
                        app.tab4_state.unfocus_review_comment();
                        return None;
                    }
                    KeyCode::Enter if key.modifiers == KeyModifiers::NONE => {
                        app.tab4_state.submit_review_comment();
                        return None;
                    }
                    _ => {
                        app.tab4_state.review_comment.input(Input::from(key));
                        return None;
                    }
                }
            }

            // Review focused: cursor navigation, line selection, file navigation.
            if app.tab4_state.review_focused {
                match key.code {
                    KeyCode::Up => {
                        app.tab4_state.move_cursor_up();
                        return None;
                    }
                    KeyCode::Down => {
                        let flat_count = app
                            .tab4_state
                            .current_diffs()
                            .get(app.tab4_state.selected_file)
                            .map(|d| {
                                use crate::tui::tabs::code_review::flatten_file_diff;
                                flatten_file_diff(d).len()
                            })
                            .unwrap_or(0);
                        app.tab4_state.move_cursor_down(flat_count);
                        return None;
                    }
                    KeyCode::PageUp => {
                        app.tab4_state.select_prev_file();
                        return None;
                    }
                    KeyCode::PageDown => {
                        let count = app.tab4_state.current_diffs().len();
                        app.tab4_state.select_next_file(count);
                        return None;
                    }
                    KeyCode::Char(' ') => {
                        let flat_lines = app
                            .tab4_state
                            .current_diffs()
                            .get(app.tab4_state.selected_file)
                            .map(|d| {
                                use crate::tui::tabs::code_review::flatten_file_diff;
                                flatten_file_diff(d)
                            })
                            .unwrap_or_default();
                        app.tab4_state.press_space(&flat_lines);
                        return None;
                    }
                    KeyCode::Esc => {
                        app.tab4_state.cancel_review();
                        return None;
                    }
                    KeyCode::Char('a') if key.modifiers == KeyModifiers::NONE => {
                        if let Some(task_id) = app.selected_task().cloned() {
                            app.open_commit_dialog(&task_id);
                        }
                        return None;
                    }
                    _ => {
                        // Consume all other keys while review pane is focused so they
                        // don't bleed through to global handlers (e.g. Up/Down task nav).
                        return None;
                    }
                }
            }

            // Review pane not focused: global Tab 7 shortcuts.
            match key.code {
                KeyCode::Char('r') if key.modifiers == KeyModifiers::NONE => {
                    app.tab4_state.focus_review();
                    return None;
                }
                KeyCode::Up => {
                    app.tab4_state.scroll_up();
                    return None;
                }
                KeyCode::Down => {
                    app.tab4_state.scroll_down();
                    return None;
                }
                KeyCode::Char('p') if key.modifiers == KeyModifiers::NONE => {
                    if app.selected_task().is_some() {
                        app.tab4_state.focus_review_comment();
                    }
                    return None;
                }
                KeyCode::Char('a') if key.modifiers == KeyModifiers::NONE => {
                    if let Some(task_id) = app.selected_task().cloned() {
                        app.open_commit_dialog(&task_id);
                    }
                    return None;
                }
                // Shift+R: request revisions using accumulated inline and general comments.
                KeyCode::Char('R') => {
                    if let Some(task_id) = app.selected_task().cloned() {
                        let comments = app.tab4_state.take_comments();
                        return Some(AppMessage::HumanRequestedRevisions { task_id, comments });
                    }
                    return None;
                }
                _ => {}
            }
        }

        // Tab 8 (Research): prompt focus, send, and scroll.
        if app.active_tab == 8 {
            if app.research_state.prompt_focused {
                if key.code == KeyCode::Esc {
                    app.research_state.prompt_focused = false;
                    app.research_state.set_prompt_unfocused_style();
                    return None;
                }
                if key.code == KeyCode::Enter && key.modifiers == KeyModifiers::NONE {
                    return submit_research_prompt(app);
                }
                app.research_state.prompt_input.input(Input::from(key));
                return None;
            }
            match key.code {
                KeyCode::Char('p') if key.modifiers == KeyModifiers::NONE => {
                    app.research_state.prompt_focused = true;
                    app.research_state.set_prompt_focused_style();
                    return None;
                }
                KeyCode::Up => {
                    app.research_state.scroll_up();
                    return None;
                }
                KeyCode::Down => {
                    app.research_state.scroll_down();
                    return None;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Char('q') if key.modifiers == KeyModifiers::NONE => {
                return Some(AppMessage::Shutdown);
            }
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                return Some(AppMessage::Shutdown);
            }
            KeyCode::Char('n') if key.modifiers == KeyModifiers::CONTROL => {
                if let Some(task_id) = app.selected_task().cloned() {
                    if app
                        .workflow_engine
                        .state(&task_id)
                        .is_some_and(|s| matches!(s.phase, WorkflowPhase::AwaitingApproval { .. }))
                    {
                        return Some(AppMessage::HumanApprovedTransition { task_id });
                    }
                }
                return None;
            }
            KeyCode::PageUp | KeyCode::Char('k') => {
                app.task_list_state.move_up();
                sync_tabs_on_nav(app);
            }
            KeyCode::PageDown | KeyCode::Char('j') => {
                app.task_list_state.move_down();
                sync_tabs_on_nav(app);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                if app.task_list_state.selected_task_id().is_none() {
                    let stories = app.cached_stories.clone();
                    app.task_list_state.toggle_story(&stories);
                }
            }
            KeyCode::Tab => {
                app.active_tab = (app.active_tab + 1) % 9;
            }
            KeyCode::BackTab => {
                app.active_tab = (app.active_tab + 8) % 9;
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyEvent, KeyEventKind, KeyEventState};

    use super::*;

    fn key_event(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn test_handle_input_q_quits() {
        let mut app = App::test_default();
        let event = key_event(KeyCode::Char('q'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_ctrl_c_quits() {
        let mut app = App::test_default();
        let event = key_event(KeyCode::Char('c'), KeyModifiers::CONTROL);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_other_key_none() {
        let mut app = App::test_default();
        let event = key_event(KeyCode::Char('a'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(result.is_none());
    }

    #[test]
    fn test_handle_input_up_moves() {
        let mut app = App::test_default();
        // Seed two story headers by directly setting up state.
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        app.task_list_state
            .expanded_stories
            .insert("2. Beta".to_string());
        let stories = vec![
            crate::tasks::Story {
                name: "1. Alpha".to_string(),
                tasks: vec![crate::tasks::models::Task {
                    id: crate::tasks::TaskId::from_path("tasks/1.1.md"),
                    story_name: "1. Alpha".to_string(),
                    name: "1.1".to_string(),
                    status: crate::tasks::TaskStatus::Open,
                    assigned_to: None,
                    description: String::new(),
                    starting_prompt: None,
                    questions: Vec::new(),
                    design: None,
                    implementation_plan: None,
                    work_log: Vec::new(),
                    file_path: std::path::PathBuf::from("tasks/1.1.md"),
                    extra_sections: Vec::new(),
                    parse_error: None,
                }],
            },
            crate::tasks::Story {
                name: "2. Beta".to_string(),
                tasks: vec![],
            },
        ];
        app.task_list_state.refresh(&stories);
        // items: [0] Story "1. Alpha", [1] Task "1.1", [2] Story "2. Beta"
        app.task_list_state.selected_index = 1;

        let event = key_event(KeyCode::PageUp, KeyModifiers::NONE);
        handle_input(event, &mut app);
        assert_eq!(app.task_list_state.selected_index, 0);
        // Now on a story — selected_task should be None.
        assert!(app.selected_task().is_none());
    }

    #[test]
    fn test_handle_input_down_moves() {
        let mut app = App::test_default();
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        let stories = vec![crate::tasks::Story {
            name: "1. Alpha".to_string(),
            tasks: vec![crate::tasks::models::Task {
                id: crate::tasks::TaskId::from_path("tasks/1.1.md"),
                story_name: "1. Alpha".to_string(),
                name: "1.1".to_string(),
                status: crate::tasks::TaskStatus::Open,
                assigned_to: None,
                description: String::new(),
                starting_prompt: None,
                questions: Vec::new(),
                design: None,
                implementation_plan: None,
                work_log: Vec::new(),
                file_path: std::path::PathBuf::from("tasks/1.1.md"),
                extra_sections: Vec::new(),
                parse_error: None,
            }],
        }];
        app.task_list_state.refresh(&stories);
        // items: [0] Story "1. Alpha", [1] Task "1.1"
        app.task_list_state.selected_index = 0;

        let event = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        handle_input(event, &mut app);
        assert_eq!(app.task_list_state.selected_index, 1);
        // Now on a task.
        assert_eq!(
            app.selected_task(),
            Some(&crate::tasks::TaskId::from_path("tasks/1.1.md"))
        );
    }

    #[test]
    fn test_handle_input_enter_toggles_story() {
        let mut app = App::test_default();
        // Insert a task so the store has a story to display.
        app.task_store.insert(crate::tasks::models::Task {
            id: crate::tasks::TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Alpha".to_string(),
            name: "1.1".to_string(),
            status: crate::tasks::TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        });
        // Refresh cached stories and task list from the store.
        app.refresh_stories();
        // Before toggle: story is collapsed, selected_index = 0.
        assert!(!app.task_list_state.expanded_stories.contains("1. Alpha"));

        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        handle_input(event, &mut app);
        // After pressing Enter on story header, it should be expanded.
        assert!(app.task_list_state.expanded_stories.contains("1. Alpha"));
    }

    #[test]
    fn test_handle_input_tab_cycles() {
        let mut app = App::test_default();
        assert_eq!(app.active_tab, 0);

        let tab = key_event(KeyCode::Tab, KeyModifiers::NONE);
        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 1);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 2);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 3);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 4);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 5);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 6);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 7);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 8);

        handle_input(tab.clone(), &mut app);
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn test_backtab_cycles_tabs_backward() {
        let mut app = App::test_default();
        app.active_tab = 2;

        let backtab = key_event(KeyCode::BackTab, KeyModifiers::SHIFT);
        handle_input(backtab.clone(), &mut app);
        assert_eq!(app.active_tab, 1);

        handle_input(backtab.clone(), &mut app);
        assert_eq!(app.active_tab, 0);
    }

    #[test]
    fn test_backtab_wraps_from_first_to_last() {
        let mut app = App::test_default();
        assert_eq!(app.active_tab, 0);

        let backtab = key_event(KeyCode::BackTab, KeyModifiers::SHIFT);
        handle_input(backtab, &mut app);
        assert_eq!(app.active_tab, 8);
    }

    /// Builds a minimal App with one story containing one task that has one unanswered question.
    fn app_with_task_and_question() -> App {
        use crate::tasks::models::{Question, Task, TaskId, TaskStatus};
        use crate::workflow::agents::AgentKind;

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Alpha".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: vec![Question {
                agent: AgentKind::Intake,
                text: "Clarify scope?".to_string(),
                answer: None,
                opencode_request_id: None,
            }],
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);
        app.refresh_stories();
        // Expand the story and navigate to the task row.
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // items: [0] Story "1. Alpha", [1] Task "1.1" — navigate down to select the task.
        let down = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        handle_input(down, &mut app);
        app
    }

    #[test]
    fn test_handle_input_i_focuses_prompt() {
        let mut app = App::test_default();
        assert_eq!(app.active_tab, 0);
        assert!(!app.tab1_state.prompt_focused);

        let event = key_event(KeyCode::Char('p'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(app.tab1_state.prompt_focused);
    }

    #[test]
    fn test_handle_input_a_focuses_answer() {
        let mut app = app_with_task_and_question();
        // Switch to Questions tab (tab 1) where answer focus is handled.
        app.active_tab = 1;
        // After navigating to the task, questions_state.answer_inputs should be populated.
        assert_eq!(app.questions_state.answer_inputs.len(), 1);
        assert!(app.questions_state.focused_answer.is_none());

        let event = key_event(KeyCode::Char('p'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert_eq!(app.questions_state.focused_answer, Some(0));
    }

    #[test]
    fn test_handle_input_ctrl_enter_submits_answer() {
        let mut app = app_with_task_and_question();
        app.active_tab = 1;
        // Focus the answer textarea.
        app.questions_state.focused_answer = Some(0);

        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(
            matches!(
                result,
                Some(AppMessage::HumanAnswered {
                    question_index: 0,
                    ..
                })
            ),
            "expected HumanAnswered with question_index=0, got: {result:?}"
        );
        assert!(
            app.questions_state.focused_answer.is_none(),
            "focused_answer should be cleared after submit"
        );
    }

    #[test]
    fn test_questions_pgup_pgdn_navigates() {
        let mut app = app_with_task_and_question();
        // Add a second question to make navigation meaningful.
        {
            use crate::tasks::models::Question;
            use crate::workflow::agents::AgentKind;
            let id = crate::tasks::TaskId::from_path("tasks/1.1.md");
            if let Some(task) = app.task_store.get_mut(&id) {
                task.questions.push(Question {
                    agent: AgentKind::Design,
                    text: "Architecture choice?".to_string(),
                    answer: None,
                    opencode_request_id: None,
                });
            }
        }
        // Reset questions_state to pick up the second question.
        {
            let id = crate::tasks::TaskId::from_path("tasks/1.1.md");
            if let Some(task) = app.task_store.get(&id) {
                let task = task.clone();
                app.questions_state.reset_for_task(&task);
            }
        }

        app.active_tab = 1;
        assert_eq!(app.questions_state.selected_question, 0);

        // Down advances.
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.questions_state.selected_question, 1);

        // Down at last clamps.
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.questions_state.selected_question, 1);

        // Up retreats.
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.questions_state.selected_question, 0);

        // Up at first clamps.
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.questions_state.selected_question, 0);
    }

    #[test]
    fn test_handle_input_pgdn_scrolls_description() {
        let mut app = App::test_default();
        assert_eq!(app.active_tab, 0);
        assert_eq!(app.tab1_state.desc_scroll, 0);

        let event = key_event(KeyCode::Down, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert_eq!(app.tab1_state.desc_scroll, 1);
    }

    #[test]
    fn test_handle_input_pgup_scrolls_description() {
        let mut app = App::test_default();
        app.tab1_state.desc_scroll = 2;

        let event = key_event(KeyCode::Up, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert_eq!(app.tab1_state.desc_scroll, 1);
    }

    #[test]
    fn test_handle_input_pgdn_no_scroll_when_prompt_focused() {
        let mut app = App::test_default();
        app.tab1_state.prompt_focused = true;

        let event = key_event(KeyCode::Down, KeyModifiers::NONE);
        handle_input(event, &mut app);

        // Down was forwarded to the textarea, not the scroll handler.
        assert_eq!(app.tab1_state.desc_scroll, 0);
    }

    #[test]
    fn test_handle_input_pgdn_no_scroll_on_other_tab() {
        // Tab 1 is now Questions; Down navigates questions, not description scroll.
        let mut app = App::test_default();
        app.active_tab = 1;

        let event = key_event(KeyCode::Down, KeyModifiers::NONE);
        handle_input(event, &mut app);

        assert_eq!(app.tab1_state.desc_scroll, 0);
    }

    #[test]
    fn test_review_tab_r_focuses_review_pane() {
        let mut app = App::test_default();
        app.active_tab = 7;
        assert!(!app.tab4_state.review_focused);

        // 'r' focuses the review pane.
        handle_input(key_event(KeyCode::Char('r'), KeyModifiers::NONE), &mut app);
        assert!(
            app.tab4_state.review_focused,
            "pressing 'r' should focus the review pane"
        );

        // Esc exits review mode.
        handle_input(key_event(KeyCode::Esc, KeyModifiers::NONE), &mut app);
        assert!(
            !app.tab4_state.review_focused,
            "Esc should exit review mode"
        );
    }

    #[test]
    fn test_review_tab_pgup_pgdn_scrolls_diff_when_unfocused() {
        let mut app = App::test_default();
        app.active_tab = 7;
        assert!(!app.tab4_state.review_focused);
        assert_eq!(app.tab4_state.diff_scroll, 0);

        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(
            app.tab4_state.diff_scroll, 3,
            "Down should scroll diff when not in review mode"
        );

        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(
            app.tab4_state.diff_scroll, 0,
            "Up should scroll diff when not in review mode"
        );
    }

    #[test]
    fn test_review_tab_pgup_pgdn_navigate_files_when_focused() {
        use crate::opencode::types::{DiffHunk, DiffLine, DiffLineKind, DiffStatus, FileDiff};

        let mut app = App::test_default();
        app.active_tab = 7;

        // Load two diffs via current_task_id path so current_diffs() works.
        let task_id = TaskId::from_path("tasks/1.1.md");
        let diffs = vec![
            FileDiff {
                path: "a.rs".to_string(),
                status: DiffStatus::Modified,
                hunks: vec![DiffHunk {
                    old_start: 1,
                    new_start: 1,
                    lines: vec![DiffLine {
                        kind: DiffLineKind::Added,
                        content: "x".to_string(),
                    }],
                }],
            },
            FileDiff {
                path: "b.rs".to_string(),
                status: DiffStatus::Added,
                hunks: vec![],
            },
        ];
        app.tab4_state.set_diffs(&task_id, diffs);
        app.tab4_state.set_displayed_task(Some(&task_id));
        app.tab4_state.focus_review();

        assert_eq!(app.tab4_state.selected_file, 0);

        // PgDn navigates to next file.
        handle_input(key_event(KeyCode::PageDown, KeyModifiers::NONE), &mut app);
        assert_eq!(
            app.tab4_state.selected_file, 1,
            "PgDn in review mode should go to next file"
        );

        // PgUp goes back.
        handle_input(key_event(KeyCode::PageUp, KeyModifiers::NONE), &mut app);
        assert_eq!(
            app.tab4_state.selected_file, 0,
            "PgUp in review mode should go to prev file"
        );
    }

    #[test]
    fn test_review_tab_up_down_moves_cursor_when_focused() {
        use crate::opencode::types::{DiffHunk, DiffLine, DiffLineKind, DiffStatus, FileDiff};

        let mut app = App::test_default();
        app.active_tab = 7;
        let task_id = TaskId::from_path("tasks/1.1.md");
        // Three-line diff → three flat lines (hunk header + 2 content lines).
        let diffs = vec![FileDiff {
            path: "x.rs".to_string(),
            status: DiffStatus::Modified,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Added,
                        content: "a".to_string(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Removed,
                        content: "b".to_string(),
                    },
                ],
            }],
        }];
        app.tab4_state.set_diffs(&task_id, diffs);
        app.tab4_state.set_displayed_task(Some(&task_id));
        app.tab4_state.focus_review();
        assert_eq!(app.tab4_state.cursor_line, 0);

        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.tab4_state.cursor_line, 1, "Down should advance cursor");

        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.tab4_state.cursor_line, 0, "Up should retreat cursor");
    }

    #[test]
    fn test_review_tab_a_emits_approved() {
        use crate::tasks::models::{Task, TaskStatus};
        use std::path::PathBuf;

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: vec![],
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("1. Story".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        app.task_list_state.selected_index = 1;
        app.active_tab = 6;

        let result = handle_input(key_event(KeyCode::Char('a'), KeyModifiers::NONE), &mut app);
        assert!(
            result.is_none(),
            "pressing 'a' on Tab 6 should return None (opens commit dialog)"
        );
        assert!(
            app.commit_dialog.is_some(),
            "pressing 'a' on Tab 6 should open the commit dialog"
        );
    }

    #[test]
    fn test_review_tab_enter_submits_inline_comment() {
        use crate::opencode::types::{DiffHunk, DiffLine, DiffLineKind, DiffStatus, FileDiff};
        use crate::tui::tabs::code_review::flatten_file_diff;

        let mut app = App::test_default();
        app.active_tab = 7;
        let task_id = TaskId::from_path("tasks/1.1.md");
        let diffs = vec![FileDiff {
            path: "x.rs".to_string(),
            status: DiffStatus::Modified,
            hunks: vec![DiffHunk {
                old_start: 1,
                new_start: 1,
                lines: vec![
                    DiffLine {
                        kind: DiffLineKind::Added,
                        content: "a".to_string(),
                    },
                    DiffLine {
                        kind: DiffLineKind::Added,
                        content: "b".to_string(),
                    },
                ],
            }],
        }];
        app.tab4_state.set_diffs(&task_id, diffs.clone());
        app.tab4_state.set_displayed_task(Some(&task_id));
        app.tab4_state.focus_review();

        // Select range: Space at line 0, move to line 1, Space again.
        let flat = flatten_file_diff(&diffs[0]);
        app.tab4_state.cursor_line = 0;
        app.tab4_state.press_space(&flat);
        app.tab4_state.cursor_line = 1;
        app.tab4_state.press_space(&flat);
        assert!(app.tab4_state.comment_mode);

        // Type and submit.
        app.tab4_state.comment_draft.insert_str("needs work");
        handle_input(key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert!(
            !app.tab4_state.comment_mode,
            "comment mode should exit after Enter"
        );
        assert_eq!(
            app.tab4_state.inline_comments.len(),
            1,
            "inline comment should be added"
        );
        assert_eq!(app.tab4_state.inline_comments[0].text, "needs work");
    }

    #[test]
    fn test_design_tab_pgdn_scrolls() {
        let mut app = App::test_default();
        app.active_tab = 2;
        assert_eq!(app.design_state.scroll, 0);

        let event = key_event(KeyCode::Down, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert_eq!(app.design_state.scroll, 1);
    }

    #[test]
    fn test_plan_tab_pgdn_scrolls() {
        let mut app = App::test_default();
        app.active_tab = 3;
        assert_eq!(app.plan_state.scroll, 0);

        let event = key_event(KeyCode::Down, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert_eq!(app.plan_state.scroll, 1);
    }

    #[test]
    fn test_footer_hints_normal_tab0() {
        let text = footer_hint_text(
            false,
            false,
            0,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[p] prompt"), "got: {text}");
        assert!(!text.contains("[a] answer"), "got: {text}"); // moved to Questions tab
        assert!(text.contains("[s] status"), "got: {text}");
        assert!(text.contains("PgUp/PgDn"), "got: {text}");
        assert!(text.contains("switch tasks"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_questions_tab() {
        let text = footer_hint_text(
            false,
            false,
            1,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[p] answer"), "got: {text}");
        assert!(text.contains("[Enter] submit"), "got: {text}");
        assert!(text.contains("Up/Down"), "got: {text}");
        assert!(text.contains("[Tab] next tab"), "got: {text}");
        assert!(text.contains("[q] quit"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_prompt_focused() {
        let text = footer_hint_text(
            false,
            false,
            0,
            FocusedInput::Prompt,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[Esc]"), "got: {text}");
        assert!(text.contains("Editing prompt"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_answer_focused() {
        let text = footer_hint_text(
            false,
            false,
            1,
            FocusedInput::Answer,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[Esc] exit"), "got: {text}");
        assert!(text.contains("Editing answer"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_review_focused() {
        let text = footer_hint_text(
            false,
            false,
            6,
            FocusedInput::Review,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[Esc] exit"), "got: {text}");
        assert!(text.contains("[Up/Down] cursor"), "got: {text}");
        assert!(text.contains("[Space] select"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_comment_focused() {
        let text = footer_hint_text(
            false,
            false,
            6,
            FocusedInput::Comment,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[Esc] cancel"), "got: {text}");
        assert!(text.contains("Editing comment"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_design_tab() {
        // Tab 2 is Design.
        let text = footer_hint_text(
            false,
            false,
            2,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[Up/Down] scroll"), "got: {text}");
        assert!(text.contains("[Tab] next tab"), "got: {text}");
        assert!(text.contains("[q] quit"), "got: {text}");
        assert!(!text.contains("[p] steer"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_plan_tab() {
        // Tab 3 is Plan.
        let text = footer_hint_text(
            false,
            false,
            3,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[Up/Down] scroll"), "got: {text}");
        assert!(text.contains("[Tab] next tab"), "got: {text}");
        assert!(text.contains("[q] quit"), "got: {text}");
        assert!(!text.contains("[p] steer"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_agent_activity_tab() {
        // Tab 4 is Agent Activity.
        let text = footer_hint_text(
            false,
            false,
            4,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[p] steer"), "got: {text}");
        assert!(text.contains("[Enter] send"), "got: {text}");
        assert!(text.contains("[Up/Down] scroll"), "got: {text}");
        assert!(text.contains("[Tab] next tab"), "got: {text}");
        assert!(text.contains("[q] quit"), "got: {text}");
        assert!(!text.contains("[i]"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_steering_focused() {
        let text = footer_hint_text(
            false,
            false,
            4,
            FocusedInput::Steering,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[Esc] exit"), "got: {text}");
        assert!(text.contains("[Enter] send"), "got: {text}");
        assert!(text.contains("Editing steering prompt"), "got: {text}");
    }

    #[test]
    fn test_tab2_steering_focus_p_key() {
        let mut app = app_with_normal_task();
        app.active_tab = 4;
        // 'p' should focus the steering textarea even when the agent is idle.
        assert!(!app.tab2_state.steering_focused);

        let event = key_event(KeyCode::Char('p'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(
            app.tab2_state.steering_focused,
            "'p' on Tab 4 should focus steering textarea even when agent is idle"
        );
    }

    /// Verifies that submitting while no active session queues the prompt instead of sending.
    #[test]
    fn test_submit_steering_prompt_queues_when_no_active_session() {
        use crate::tui::tabs::agent_activity::ActivityLine;

        let mut app = app_with_normal_task();
        app.active_tab = 4;
        let id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        // No workflow started -- no active session.

        app.tab2_state.steering_input.insert_str("queue me");

        // Submit via Enter.
        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        // Should return None (not a SendPrompt).
        assert!(
            result.is_none(),
            "submit without active session should return None, got: {result:?}"
        );

        // The prompt should be queued.
        assert_eq!(
            app.tab2_state.take_queued_prompt(&id),
            Some("queue me".to_string()),
            "prompt should be queued when no active session"
        );

        // A [You - queued] banner should appear in the activity buffer.
        let lines = app.tab2_state.lines_for(&id);
        assert!(
            lines.iter().any(|l| matches!(
                l,
                ActivityLine::AgentBanner { message }
                    if message.contains("[You - queued]") && message.contains("queue me")
            )),
            "expected [You - queued] banner; lines: {lines:?}"
        );

        // Textarea should be cleared.
        assert_eq!(
            app.tab2_state.steering_input.lines().join("\n"),
            "",
            "textarea should be cleared after submit"
        );
    }

    #[test]
    fn test_tab2_steering_esc_unfocuses() {
        let mut app = app_with_normal_task();
        app.active_tab = 4;
        let id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        app.tab2_state
            .set_awaiting_response(&id, "Intake Agent".to_string());
        app.tab2_state.steering_focused = true;
        app.tab2_state.set_steering_focused_style();

        let event = key_event(KeyCode::Esc, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(
            !app.tab2_state.steering_focused,
            "Esc should unfocus steering textarea"
        );
    }

    #[test]
    fn test_tab2_steering_submit_emits_message() {
        use crate::tui::tabs::agent_activity::ActivityLine;

        let mut app = app_with_normal_task();
        app.active_tab = 4;
        let id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        // Start task and create session so workflow engine has session_id.
        app.workflow_engine.process(AppMessage::StartTask {
            task_id: id.clone(),
        });
        app.workflow_engine.process(AppMessage::SessionCreated {
            task_id: id.clone(),
            session_id: "sess-1".to_string(),
        });
        app.tab2_state
            .set_awaiting_response(&id, "Intake Agent".to_string());

        // Type text into the steering textarea.
        app.tab2_state
            .steering_input
            .insert_str("focus on error handling");

        // Submit via Enter (unfocused mode).
        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(
            matches!(
                result,
                Some(AppMessage::SendPrompt {
                    ref prompt,
                    ref session_id,
                    ..
                }) if prompt == "focus on error handling" && session_id == "sess-1"
            ),
            "expected SendPrompt with steering text, got: {result:?}"
        );
        // Textarea should be cleared.
        assert_eq!(
            app.tab2_state.steering_input.lines().join("\n"),
            "",
            "steering textarea should be cleared after submit"
        );
        // [You] banner should appear in the activity log.
        let lines = app.tab2_state.lines_for(&id);
        assert!(
            lines.iter().any(|l| matches!(
                l,
                ActivityLine::AgentBanner { message }
                    if message.contains("[You]") && message.contains("focus on error handling")
            )),
            "expected [You] banner in activity log: {:?}",
            lines
        );
        // Should be unfocused.
        assert!(!app.tab2_state.steering_focused);
    }

    /// Verifies that plain Enter submits the steering prompt when the textarea is focused.
    #[test]
    fn test_tab2_steering_enter_submits_when_focused() {
        let mut app = app_with_normal_task();
        app.active_tab = 4;
        let id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        app.workflow_engine.process(AppMessage::StartTask {
            task_id: id.clone(),
        });
        app.workflow_engine.process(AppMessage::SessionCreated {
            task_id: id.clone(),
            session_id: "sess-focused".to_string(),
        });
        app.tab2_state
            .set_awaiting_response(&id, "Intake Agent".to_string());

        // Focus the textarea and type text.
        app.tab2_state.steering_focused = true;
        app.tab2_state.steering_input.insert_str("steer the agent");

        // Plain Enter should submit.
        let result = handle_input(key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);
        assert!(
            matches!(
                result,
                Some(AppMessage::SendPrompt { ref prompt, .. })
                    if prompt == "steer the agent"
            ),
            "focused Enter should submit prompt, got: {result:?}"
        );
        assert!(
            !app.tab2_state.steering_focused,
            "should be unfocused after submit"
        );
    }

    #[test]
    fn test_footer_hints_review_tab() {
        // Tab 6 is now the Review Discussion tab.
        let text = footer_hint_text(
            false,
            false,
            6,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[a] approve"), "got: {text}");
        assert!(text.contains("[R] revisions"), "got: {text}");
        assert!(text.contains("[Tab] next tab"), "got: {text}");
        // '[r] review' is only on the Code Diff tab (7).
        assert!(!text.contains("[r] review"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_code_diff_tab() {
        // Tab 7 is the Code Diff tab.
        let text = footer_hint_text(
            false,
            false,
            7,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[a] approve"), "got: {text}");
        assert!(text.contains("[r] review"), "got: {text}");
        assert!(text.contains("[R] revisions"), "got: {text}");
        assert!(text.contains("[Tab] next tab"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_permission_pending() {
        // When a permission is pending on Tab 4, show approve/reject hints.
        let text = footer_hint_text(
            false,
            false,
            4,
            FocusedInput::None,
            false,
            false,
            false,
            true,
            false,
        );
        assert!(text.contains("[y] approve"), "got: {text}");
        assert!(text.contains("[a] always"), "got: {text}");
        assert!(text.contains("[n] reject"), "got: {text}");
        assert!(text.contains("[Tab] next tab"), "got: {text}");
        // Should NOT show steer hints when permission is pending.
        assert!(!text.contains("[p] steer"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_quit_confirm() {
        let text = footer_hint_text(
            true,
            false,
            0,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[y/Enter]"), "got: {text}");
        assert!(text.contains("[n/Esc]"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_malformed_task() {
        let text = footer_hint_text(
            false,
            false,
            0,
            FocusedInput::None,
            true,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[f] request fix"), "got: {text}");
        assert!(text.contains("[Enter] apply fix"), "got: {text}");
        assert!(text.contains("PgUp/PgDn"), "got: {text}");
        assert!(text.contains("switch tasks"), "got: {text}");
        // Normal task hints should not appear for malformed tasks.
        assert!(!text.contains("[i] prompt"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_status_picker() {
        let text = footer_hint_text(
            false,
            true,
            0,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(text.contains("[1-5] select"), "got: {text}");
        assert!(text.contains("[Up/Down] navigate"), "got: {text}");
        assert!(text.contains("[Enter] confirm"), "got: {text}");
        assert!(text.contains("[Esc] cancel"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_startable_task() {
        let text = footer_hint_text(
            false,
            false,
            0,
            FocusedInput::None,
            false,
            true,
            false,
            false,
            false,
        );
        assert!(text.contains("[Enter] start"), "got: {text}");
        assert!(text.contains("[s] status"), "got: {text}");
        assert!(text.contains("PgUp/PgDn"), "got: {text}");
        assert!(text.contains("switch tasks"), "got: {text}");
        // Should not show [a] answer (moved to Questions tab).
        assert!(!text.contains("[a] answer"), "got: {text}");
    }

    #[test]
    fn test_handle_input_quit_confirm_y_confirms() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Char('y'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
        assert!(app.show_quit_confirm); // still true; app.rs handler will set should_quit
    }

    #[test]
    fn test_handle_input_quit_confirm_enter_confirms() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(matches!(result, Some(AppMessage::Shutdown)));
    }

    #[test]
    fn test_handle_input_quit_confirm_n_cancels() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Char('n'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(result.is_none());
        assert!(!app.show_quit_confirm);
    }

    #[test]
    fn test_handle_input_quit_confirm_esc_cancels() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Esc, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(result.is_none());
        assert!(!app.show_quit_confirm);
    }

    #[test]
    fn test_handle_input_quit_confirm_other_keys_ignored() {
        let mut app = App::test_default();
        app.show_quit_confirm = true;
        let event = key_event(KeyCode::Char('x'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(result.is_none());
        assert!(app.show_quit_confirm); // dialog remains
    }

    /// Builds an App with a malformed task selected in Tab 0.
    fn app_with_malformed_task() -> App {
        use crate::tasks::models::{ParseErrorInfo, Task, TaskId, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Alpha".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: Some(ParseErrorInfo {
                error_message: "missing Status".to_string(),
                raw_content: "bad content".to_string(),
                suggested_fix: None,
                fix_in_progress: false,
                fix_error: None,
            }),
        };
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // Navigate down to select the task row.
        let down = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        handle_input(down, &mut app);
        app
    }

    #[test]
    fn test_handle_input_f_on_malformed() {
        let mut app = app_with_malformed_task();
        let event = key_event(KeyCode::Char('f'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            matches!(result, Some(AppMessage::RequestTaskFix { .. })),
            "expected RequestTaskFix, got: {result:?}"
        );
    }

    #[test]
    fn test_handle_input_enter_on_malformed_without_fix() {
        // No suggested fix -- Enter should not emit ApplyTaskFix.
        let mut app = app_with_malformed_task();
        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            result.is_none(),
            "Enter without a fix should return None, got: {result:?}"
        );
    }

    #[test]
    fn test_handle_input_enter_on_malformed_with_fix() {
        use crate::tasks::models::{SuggestedFix, TaskId};

        let mut app = app_with_malformed_task();
        // Add a suggested fix to the task.
        let id = TaskId::from_path("tasks/1.1.md");
        if let Some(task) = app.task_store.get_mut(&id) {
            if let Some(ref mut err_info) = task.parse_error {
                err_info.suggested_fix = Some(SuggestedFix {
                    corrected_content: "fixed content".to_string(),
                    explanation: "Added Status line".to_string(),
                });
            }
        }
        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            matches!(result, Some(AppMessage::ApplyTaskFix { .. })),
            "expected ApplyTaskFix, got: {result:?}"
        );
    }

    /// Builds an App with a normal (non-malformed) task selected on Tab 0.
    fn app_with_normal_task() -> App {
        use crate::tasks::models::{Task, TaskId, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Alpha".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // Navigate down to select the task row.
        let down = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        handle_input(down, &mut app);
        app
    }

    #[test]
    fn test_handle_input_s_opens_status_picker() {
        let mut app = app_with_normal_task();
        assert!(app.show_status_picker.is_none());

        let event = key_event(KeyCode::Char('s'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        // Open status index 0 (OPEN) should be pre-selected.
        assert_eq!(app.show_status_picker, Some(0));
    }

    #[test]
    fn test_handle_input_s_ignored_no_task() {
        // No task selected (on a story row) -- 's' should not open the picker.
        let mut app = App::test_default();
        assert!(app.selected_task().is_none());

        let event = key_event(KeyCode::Char('s'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(app.show_status_picker.is_none());
    }

    #[test]
    fn test_handle_input_s_ignored_prompt_focused() {
        // When the prompt textarea is focused, 's' goes to the textarea, not the picker.
        let mut app = app_with_normal_task();
        app.tab1_state.prompt_focused = true;
        app.tab1_state.set_prompt_focused_style();

        let event = key_event(KeyCode::Char('s'), KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(app.show_status_picker.is_none());
    }

    #[test]
    fn test_status_picker_esc_dismisses() {
        let mut app = app_with_normal_task();
        app.show_status_picker = Some(2);

        let event = key_event(KeyCode::Esc, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(result.is_none());
        assert!(app.show_status_picker.is_none());
    }

    #[test]
    fn test_status_picker_up_down_navigation() {
        let mut app = app_with_normal_task();
        app.show_status_picker = Some(2);

        // Up moves to 1.
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(1));

        // Up again to 0.
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(0));

        // Clamped at 0.
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(0));

        // Down to 1.
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(1));

        // Down three more times to reach 4 (Abandoned).
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(4));

        // Clamped at 4.
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.show_status_picker, Some(4));
    }

    #[test]
    fn test_status_picker_number_key_selects() {
        let mut app = app_with_normal_task();
        app.show_status_picker = Some(0);

        // Press '3' to select PendingReview (index 2).
        let result = handle_input(key_event(KeyCode::Char('3'), KeyModifiers::NONE), &mut app);

        assert!(app.show_status_picker.is_none(), "picker should close");
        assert!(
            matches!(result, Some(AppMessage::TaskUpdated { .. })),
            "expected TaskUpdated, got: {result:?}"
        );
    }

    #[test]
    fn test_status_picker_enter_confirms() {
        let mut app = app_with_normal_task();
        // Pre-select index 3 (Completed).
        app.show_status_picker = Some(3);

        let result = handle_input(key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);

        assert!(app.show_status_picker.is_none(), "picker should close");
        assert!(
            matches!(result, Some(AppMessage::TaskUpdated { .. })),
            "expected TaskUpdated, got: {result:?}"
        );
    }

    #[test]
    fn test_status_picker_swallows_other_keys() {
        let mut app = app_with_normal_task();
        app.show_status_picker = Some(1);

        // 'q' should NOT quit when picker is open.
        let result = handle_input(key_event(KeyCode::Char('q'), KeyModifiers::NONE), &mut app);
        assert!(result.is_none(), "expected None, got: {result:?}");
        assert_eq!(app.show_status_picker, Some(1), "picker should remain open");

        // Tab should be swallowed.
        let result = handle_input(key_event(KeyCode::Tab, KeyModifiers::NONE), &mut app);
        assert!(result.is_none());
        assert_eq!(app.show_status_picker, Some(1));
    }

    #[test]
    fn test_status_picker_changes_task_status() {
        use crate::tasks::models::{TaskId, TaskStatus};

        let mut app = app_with_normal_task();
        // Task starts as Open (index 0).
        let id = TaskId::from_path("tasks/1.1.md");
        assert_eq!(app.task_store.get(&id).unwrap().status, TaskStatus::Open);

        // Open picker, navigate to Completed (index 3), press Enter.
        app.show_status_picker = Some(3);
        handle_input(key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);

        assert_eq!(
            app.task_store.get(&id).unwrap().status,
            TaskStatus::Completed,
            "task status should be Completed after picker selection"
        );
    }

    #[test]
    fn test_permission_y_emits_once() {
        use crate::opencode::types::PermissionRequest;

        let mut app = app_with_normal_task();
        app.active_tab = 4;
        let id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        let request = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-1".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["cargo build".to_string()],
            always: vec![],
        };
        app.tab2_state.push_permission(id.clone(), request);

        let result = handle_input(key_event(KeyCode::Char('y'), KeyModifiers::NONE), &mut app);
        assert!(
            matches!(
                result,
                Some(AppMessage::PermissionResolved { ref response, .. }) if response == "once"
            ),
            "expected PermissionResolved with 'once', got: {result:?}"
        );
    }

    #[test]
    fn test_permission_a_emits_always() {
        use crate::opencode::types::PermissionRequest;

        let mut app = app_with_normal_task();
        app.active_tab = 4;
        let id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        let request = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-1".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["cargo build".to_string()],
            always: vec![],
        };
        app.tab2_state.push_permission(id.clone(), request);

        let result = handle_input(key_event(KeyCode::Char('a'), KeyModifiers::NONE), &mut app);
        assert!(
            matches!(
                result,
                Some(AppMessage::PermissionResolved { ref response, .. }) if response == "always"
            ),
            "expected PermissionResolved with 'always', got: {result:?}"
        );
    }

    #[test]
    fn test_permission_n_emits_reject() {
        use crate::opencode::types::PermissionRequest;

        let mut app = app_with_normal_task();
        app.active_tab = 4;
        let id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        let request = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-1".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["cargo build".to_string()],
            always: vec![],
        };
        app.tab2_state.push_permission(id.clone(), request);

        let result = handle_input(key_event(KeyCode::Char('n'), KeyModifiers::NONE), &mut app);
        assert!(
            matches!(
                result,
                Some(AppMessage::PermissionResolved { ref response, .. }) if response == "reject"
            ),
            "expected PermissionResolved with 'reject', got: {result:?}"
        );
    }

    #[test]
    fn test_handle_input_enter_starts_open_task() {
        // An OPEN non-malformed task: Enter should emit StartTask.
        let mut app = app_with_normal_task();

        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            matches!(result, Some(AppMessage::StartTask { .. })),
            "expected StartTask for OPEN task, got: {result:?}"
        );
    }

    #[test]
    fn test_handle_input_enter_no_start_non_open_task() {
        use crate::tasks::models::{TaskId, TaskStatus};

        // Change the task status to Completed; Enter should not start or resume it.
        let mut app = app_with_normal_task();
        let id = TaskId::from_path("tasks/1.1.md");
        if let Some(task) = app.task_store.get_mut(&id) {
            task.status = TaskStatus::Completed;
        }

        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);
        assert!(
            result.is_none(),
            "Enter on a Completed task should return None, got: {result:?}"
        );
    }

    #[test]
    fn test_footer_hints_team_status_awaiting_approval() {
        // Tab 5 with awaiting_approval=true should show [Ctrl+N] next agent.
        let text = footer_hint_text(
            false,
            false,
            5,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            true,
        );
        assert!(text.contains("[Ctrl+N] next agent"), "got: {text}");
        assert!(text.contains("[Up/Down] scroll"), "got: {text}");
        assert!(text.contains("[Tab] next tab"), "got: {text}");
        assert!(text.contains("[q] quit"), "got: {text}");
    }

    #[test]
    fn test_footer_hints_team_status_not_awaiting() {
        // Tab 5 without awaiting_approval should NOT show [Ctrl+N] next agent.
        let text = footer_hint_text(
            false,
            false,
            5,
            FocusedInput::None,
            false,
            false,
            false,
            false,
            false,
        );
        assert!(!text.contains("[Ctrl+N] next agent"), "got: {text}");
        assert!(text.contains("[Up/Down] scroll"), "got: {text}");
    }

    #[test]
    fn test_ctrl_n_key_emits_approved_transition() {
        use crate::tasks::models::TaskId;
        use crate::workflow::agents::AgentKind;
        use crate::workflow::transitions::WorkflowPhase;

        let mut app = app_with_normal_task();
        app.active_tab = 5;
        let id = TaskId::from_path("tasks/1.1.md");

        // Manually inject AwaitingApproval state into the workflow engine.
        app.workflow_engine.process(AppMessage::StartTask {
            task_id: id.clone(),
        });
        app.workflow_engine.set_approval_gate(true);
        app.workflow_engine.process(AppMessage::AgentCompleted {
            task_id: id.clone(),
            agent: AgentKind::Intake,
            summary: "done".to_string(),
        });

        // Now the phase should be AwaitingApproval.
        let state = app.workflow_engine.state(&id).expect("state exists");
        assert!(
            matches!(state.phase, WorkflowPhase::AwaitingApproval { .. }),
            "phase should be AwaitingApproval before test"
        );

        // Press Ctrl+N on Tab 5 -- should emit HumanApprovedTransition.
        let result = handle_input(
            key_event(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &mut app,
        );
        assert!(
            matches!(result, Some(AppMessage::HumanApprovedTransition { .. })),
            "expected HumanApprovedTransition, got: {result:?}"
        );
    }

    #[test]
    fn test_ctrl_n_key_works_from_any_tab() {
        use crate::tasks::models::TaskId;
        use crate::workflow::agents::AgentKind;
        use crate::workflow::transitions::WorkflowPhase;

        let mut app = app_with_normal_task();
        let id = TaskId::from_path("tasks/1.1.md");

        app.workflow_engine.process(AppMessage::StartTask {
            task_id: id.clone(),
        });
        app.workflow_engine.set_approval_gate(true);
        app.workflow_engine.process(AppMessage::AgentCompleted {
            task_id: id.clone(),
            agent: AgentKind::Intake,
            summary: "done".to_string(),
        });

        let state = app.workflow_engine.state(&id).expect("state exists");
        assert!(
            matches!(state.phase, WorkflowPhase::AwaitingApproval { .. }),
            "phase should be AwaitingApproval before test"
        );

        // Ctrl+N should fire HumanApprovedTransition from tab 0 (not just tab 5).
        app.active_tab = 0;
        let result = handle_input(
            key_event(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &mut app,
        );
        assert!(
            matches!(result, Some(AppMessage::HumanApprovedTransition { .. })),
            "expected HumanApprovedTransition from tab 0, got: {result:?}"
        );
    }

    #[test]
    fn test_ctrl_n_key_noop_when_not_awaiting() {
        let mut app = app_with_normal_task();
        app.active_tab = 5;
        let id = crate::tasks::TaskId::from_path("tasks/1.1.md");

        // Start task but no gate -- workflow is Running, not AwaitingApproval.
        app.workflow_engine.process(AppMessage::StartTask {
            task_id: id.clone(),
        });

        let result = handle_input(
            key_event(KeyCode::Char('n'), KeyModifiers::CONTROL),
            &mut app,
        );
        assert!(
            result.is_none(),
            "Ctrl+N while Running should be a no-op, got: {result:?}"
        );
    }

    #[test]
    fn test_n_key_on_tab5_no_longer_triggers_transition() {
        use crate::tasks::models::TaskId;
        use crate::workflow::agents::AgentKind;
        use crate::workflow::transitions::WorkflowPhase;

        let mut app = app_with_normal_task();
        app.active_tab = 5;
        let id = TaskId::from_path("tasks/1.1.md");

        app.workflow_engine.process(AppMessage::StartTask {
            task_id: id.clone(),
        });
        app.workflow_engine.set_approval_gate(true);
        app.workflow_engine.process(AppMessage::AgentCompleted {
            task_id: id.clone(),
            agent: AgentKind::Intake,
            summary: "done".to_string(),
        });

        let state = app.workflow_engine.state(&id).expect("state exists");
        assert!(
            matches!(state.phase, WorkflowPhase::AwaitingApproval { .. }),
            "phase should be AwaitingApproval before test"
        );

        // Plain 'n' on tab 5 should no longer trigger transition.
        let result = handle_input(key_event(KeyCode::Char('n'), KeyModifiers::NONE), &mut app);
        assert!(
            result.is_none(),
            "plain 'n' on Tab 5 should no longer emit HumanApprovedTransition, got: {result:?}"
        );
    }

    /// Verifies that format_tokens renders small numbers as-is.
    #[test]
    fn test_format_tokens_small() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
    }

    /// Verifies that format_tokens renders thousands with one decimal and k suffix.
    #[test]
    fn test_format_tokens_thousands() {
        assert_eq!(format_tokens(1000), "1.0k");
        assert_eq!(format_tokens(12345), "12.3k");
        assert_eq!(format_tokens(999_999), "1000.0k");
    }

    /// Verifies that format_tokens renders millions with one decimal and M suffix.
    #[test]
    fn test_format_tokens_millions() {
        assert_eq!(format_tokens(1_000_000), "1.0M");
        assert_eq!(format_tokens(2_500_000), "2.5M");
    }

    // --- Resume task UI tests ---

    /// Helper: builds an App with an InProgress task and navigates to it.
    fn app_with_in_progress_task() -> (App, crate::tasks::TaskId) {
        use crate::tasks::models::{Task, TaskId, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: Some(crate::workflow::agents::AgentKind::Design),
            description: "desc".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("1. Story".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // items: [0] Story, [1] Task -- navigate to task.
        let down = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        handle_input(down, &mut app);
        (app, task_id)
    }

    /// Verifies that pressing Enter on an InProgress task emits ResumeTask.
    #[test]
    fn test_enter_on_in_progress_task_emits_resume() {
        let (mut app, task_id) = app_with_in_progress_task();
        // Tab 0 is default.
        assert_eq!(app.active_tab, 0);

        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(
            matches!(result, Some(AppMessage::ResumeTask { task_id: ref tid }) if *tid == task_id),
            "Enter on InProgress task should emit ResumeTask, got: {result:?}"
        );
    }

    /// Verifies that pressing Enter on an Open task still emits StartTask.
    #[test]
    fn test_enter_on_open_task_still_emits_start() {
        use crate::tasks::models::{Task, TaskId, TaskStatus};

        let mut app = App::test_default();
        let task = Task {
            id: TaskId::from_path("tasks/2.1.md"),
            story_name: "2. Story".to_string(),
            name: "2.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: "desc".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/2.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let task_id = task.id.clone();
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("2. Story".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        let down = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        handle_input(down, &mut app);

        let event = key_event(KeyCode::Enter, KeyModifiers::NONE);
        let result = handle_input(event, &mut app);

        assert!(
            matches!(result, Some(AppMessage::StartTask { task_id: ref tid }) if *tid == task_id),
            "Enter on Open task should emit StartTask, got: {result:?}"
        );
    }

    /// Verifies that footer shows resume hint for an InProgress task on Tab 0.
    #[test]
    fn test_footer_hints_resumable_task() {
        let hint = footer_hint_text(
            false,
            false,
            0,
            FocusedInput::None,
            false,
            false,
            true, // is_resumable_task
            false,
            false,
        );
        assert!(
            hint.contains("[Enter] resume"),
            "footer should show resume hint for InProgress task, got: {hint}"
        );
    }

    // --- team_status_text tests ---

    fn make_task(status: TaskStatus, assigned_to: Option<AgentKind>) -> Task {
        use crate::tasks::models::TaskId;
        Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status,
            assigned_to,
            description: "desc".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        }
    }

    /// No task selected returns "No Task Selected".
    #[test]
    fn test_team_status_text_no_task() {
        let text = team_status_text(None);
        assert_eq!(text, "No Task Selected");
    }

    /// Open task with no assignment shows "Open - No Active Agent".
    #[test]
    fn test_team_status_text_open_no_agent() {
        let task = make_task(TaskStatus::Open, None);
        let text = team_status_text(Some(&task));
        assert_eq!(text, "Open - No Active Agent");
    }

    /// InProgress task assigned to Implementation agent shows correct labels.
    #[test]
    fn test_team_status_text_in_progress_with_agent() {
        let task = make_task(TaskStatus::InProgress, Some(AgentKind::Implementation));
        let text = team_status_text(Some(&task));
        assert_eq!(text, "In Progress - Implementation Agent");
    }

    /// Completed task assigned to Human shows "No Active Agent" (Human is not a pipeline agent).
    #[test]
    fn test_team_status_text_completed_human_assigned() {
        let task = make_task(TaskStatus::Completed, Some(AgentKind::Human));
        let text = team_status_text(Some(&task));
        assert_eq!(text, "Completed - No Active Agent");
    }

    /// PendingReview task assigned to CodeReview agent shows correct labels.
    #[test]
    fn test_team_status_text_pending_review_with_agent() {
        let task = make_task(TaskStatus::PendingReview, Some(AgentKind::CodeReview));
        let text = team_status_text(Some(&task));
        assert_eq!(text, "Pending Review - Code Review Agent");
    }

    /// Builds an App with one task selected and a pending permission request stored.
    fn app_with_pending_permission() -> App {
        use crate::opencode::types::PermissionRequest;
        use crate::tasks::models::{Task, TaskStatus};

        let mut app = App::test_default();
        let task_id = TaskId::from_path("tasks/1.1.md");
        let task = Task {
            id: task_id.clone(),
            story_name: "1. Alpha".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("1. Alpha".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // Navigate to the task row (index 1 after expanding story).
        let down = key_event(KeyCode::PageDown, KeyModifiers::NONE);
        handle_input(down, &mut app);

        let request = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-1".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["cargo build".to_string()],
            always: vec![],
        };
        app.tab2_state.push_permission(task_id, request);
        app
    }

    /// Pressing 'y' with a pending permission resolves it with "once", regardless of active tab.
    #[test]
    fn test_permission_popup_y_resolves_from_any_tab() {
        let mut app = app_with_pending_permission();
        app.active_tab = 0;

        let result = handle_input(key_event(KeyCode::Char('y'), KeyModifiers::NONE), &mut app);

        assert!(
            matches!(
                result,
                Some(AppMessage::PermissionResolved {
                    ref response,
                    ..
                }) if response == "once"
            ),
            "expected PermissionResolved with response=once, got: {result:?}"
        );
    }

    /// While a permission dialog is pending, non-permission keys are consumed and
    /// do NOT pass through to tab-level handlers (fixes key-leak bug).
    #[test]
    fn test_permission_popup_non_permission_key_blocked() {
        let mut app = app_with_pending_permission();
        let initial_tab = app.active_tab;

        handle_input(key_event(KeyCode::Tab, KeyModifiers::NONE), &mut app);

        assert_eq!(
            app.active_tab, initial_tab,
            "Tab key should be consumed by permission dialog and must not cycle active_tab"
        );
    }

    /// Pressing `r` while a permission dialog is shown focuses the rejection response textarea
    /// without emitting any message.
    #[test]
    fn test_r_key_focuses_rejection_response() {
        let mut app = app_with_pending_permission();
        assert!(
            !app.tab2_state.rejection_response_focused,
            "rejection response should start unfocused"
        );

        let result = handle_input(key_event(KeyCode::Char('r'), KeyModifiers::NONE), &mut app);

        assert!(result.is_none(), "expected None from [r], got: {result:?}");
        assert!(
            app.tab2_state.rejection_response_focused,
            "rejection response should be focused after [r]"
        );
    }

    /// Pressing Esc while the rejection response is focused returns to the y/a/n/r dialog
    /// without emitting any message and without rejecting.
    #[test]
    fn test_esc_in_rejection_response_returns_to_dialog() {
        let mut app = app_with_pending_permission();
        app.tab2_state.focus_rejection_response();

        let result = handle_input(key_event(KeyCode::Esc, KeyModifiers::NONE), &mut app);

        assert!(result.is_none(), "expected None from Esc, got: {result:?}");
        assert!(
            !app.tab2_state.rejection_response_focused,
            "rejection response should be unfocused after Esc"
        );
        // Permission should still be pending.
        assert!(
            !app.tab2_state.pending_permissions.is_empty(),
            "permission should still be pending after Esc"
        );
    }

    /// Pressing Enter while the rejection response textarea is focused emits `PermissionResolved`
    /// with `response="reject"` and a prepended explanation string.
    #[test]
    fn test_enter_in_rejection_response_emits_resolved() {
        use tui_textarea::Input;
        let mut app = app_with_pending_permission();
        app.tab2_state.focus_rejection_response();

        // Type some text into the rejection response textarea.
        for ch in "try a different approach".chars() {
            app.tab2_state
                .rejection_response
                .input(Input::from(key_event(
                    KeyCode::Char(ch),
                    KeyModifiers::NONE,
                )));
        }

        let result = handle_input(key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);

        assert!(
            matches!(
                result,
                Some(AppMessage::PermissionResolved {
                    ref response,
                    explanation: Some(ref expl),
                    ..
                }) if response == "reject"
                    && expl.starts_with("No, let's consider something else first.")
                    && expl.contains("try a different approach")
            ),
            "expected PermissionResolved with reject + explanation, got: {result:?}"
        );
        // Textarea should be cleared and unfocused.
        assert!(
            !app.tab2_state.rejection_response_focused,
            "rejection response should be unfocused after submit"
        );
    }

    /// Up/Down arrows while a permission dialog is pending adjust `permission_scroll`
    /// and do NOT bubble up to the activity buffer scroll. Down is bounded by the
    /// wrapped line count of the pattern list (max_scroll = total_lines - 4 visible).
    #[test]
    fn test_up_down_adjust_permission_scroll() {
        use crate::opencode::types::PermissionRequest;
        let mut app = app_with_pending_permission();

        // Replace the fixture permission with a 6-pattern one so the pattern area
        // (4 visible rows) needs scrolling: max_scroll = 6 - 4 = 2.
        app.tab2_state.pending_permissions.clear();
        let task_id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        let request = PermissionRequest {
            id: "perm-scroll".to_string(),
            session_id: "sess-1".to_string(),
            permission: "bash".to_string(),
            patterns: (1..=6).map(|i| format!("cmd-{}", i)).collect(),
            always: vec![],
        };
        app.tab2_state.push_permission(task_id, request);
        assert_eq!(app.tab2_state.permission_scroll, 0);

        // Down increases scroll.
        let result = handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert!(result.is_none(), "Down should return None, got: {result:?}");
        assert_eq!(app.tab2_state.permission_scroll, 1);

        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(app.tab2_state.permission_scroll, 2);

        // Down clamps at max_scroll (2).
        handle_input(key_event(KeyCode::Down, KeyModifiers::NONE), &mut app);
        assert_eq!(
            app.tab2_state.permission_scroll, 2,
            "Down should clamp at max_scroll"
        );

        // Up decreases scroll.
        let result = handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert!(result.is_none(), "Up should return None, got: {result:?}");
        assert_eq!(app.tab2_state.permission_scroll, 1);

        // Up clamps at zero.
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        handle_input(key_event(KeyCode::Up, KeyModifiers::NONE), &mut app);
        assert_eq!(
            app.tab2_state.permission_scroll, 0,
            "scroll should clamp at 0"
        );
    }

    /// `push_permission` resets `permission_scroll` to 0 only on the empty-to-non-empty
    /// transition so the first dialog starts at the top. A second push while a dialog is
    /// already visible must not disturb the user's current scroll position.
    #[test]
    fn test_push_permission_resets_scroll() {
        use crate::opencode::types::PermissionRequest;

        let mut app = App::test_default();
        let id = crate::tasks::TaskId::from_path("tasks/1.1.md");

        // First push onto an empty queue: scroll should reset to 0.
        app.tab2_state.permission_scroll = 5;
        let req1 = PermissionRequest {
            id: "perm-1".to_string(),
            session_id: "sess-1".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["cargo build".to_string()],
            always: vec![],
        };
        app.tab2_state.push_permission(id.clone(), req1);
        assert_eq!(
            app.tab2_state.permission_scroll, 0,
            "first push should reset scroll to 0"
        );

        // Second push onto a non-empty queue: scroll must not be disturbed.
        app.tab2_state.permission_scroll = 3;
        let req2 = PermissionRequest {
            id: "perm-2".to_string(),
            session_id: "sess-1".to_string(),
            permission: "bash".to_string(),
            patterns: vec!["cargo test".to_string()],
            always: vec![],
        };
        app.tab2_state.push_permission(id, req2);
        assert_eq!(
            app.tab2_state.permission_scroll, 3,
            "second push must not reset scroll while a dialog is already showing"
        );
    }

    /// Helper to create a task and set up task list selection.
    fn setup_task_selected(app: &mut App, task_id: crate::tasks::TaskId) {
        use crate::tasks::models::{Task, TaskStatus};
        let task = Task {
            id: task_id.clone(),
            story_name: "1. Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::PendingReview,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: vec![],
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: std::path::PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        app.task_store.insert(task);
        app.refresh_stories();
        app.task_list_state
            .expanded_stories
            .insert("1. Story".to_string());
        app.task_list_state.refresh(&app.cached_stories);
        // Story header is at index 0, task at index 1.
        app.task_list_state.selected_index = 1;
    }

    /// Verifies that pressing `[a]` on Tab 6 opens the commit dialog instead of emitting
    /// `HumanApprovedReview`.
    #[test]
    fn test_a_key_opens_commit_dialog_on_review_tab() {
        let mut app = App::test_default();
        let task_id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        setup_task_selected(&mut app, task_id.clone());
        app.active_tab = 6;

        let result = handle_input(key_event(KeyCode::Char('a'), KeyModifiers::NONE), &mut app);

        assert!(
            result.is_none(),
            "pressing 'a' should return None (not HumanApprovedReview)"
        );
        assert!(
            app.commit_dialog.is_some(),
            "commit_dialog should be open after pressing 'a' on Tab 6"
        );
        assert_eq!(
            app.commit_dialog.as_ref().unwrap().task_id,
            task_id,
            "commit_dialog should have the selected task_id"
        );
    }

    /// Verifies that pressing `[Alt+Enter]` in the commit dialog emits `HumanApprovedCommit`.
    #[test]
    fn test_commit_dialog_enter_emits_approved_commit() {
        let mut app = App::test_default();
        let task_id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        app.open_commit_dialog(&task_id);

        // Type some text into the editor.
        app.commit_dialog
            .as_mut()
            .unwrap()
            .editor
            .insert_str("feat: my commit");

        let result = handle_input(key_event(KeyCode::Enter, KeyModifiers::ALT), &mut app);

        assert!(
            matches!(result, Some(AppMessage::HumanApprovedCommit { ref commit_message, .. }) if commit_message.contains("feat: my commit")),
            "Alt+Enter in commit dialog should emit HumanApprovedCommit, got: {result:?}"
        );
        assert!(
            app.commit_dialog.is_none(),
            "commit_dialog should be closed after Alt+Enter"
        );
    }

    /// Verifies that bare `[Enter]` inserts a newline rather than confirming the dialog.
    #[test]
    fn test_commit_dialog_bare_enter_inserts_newline() {
        let mut app = App::test_default();
        let task_id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        app.open_commit_dialog(&task_id);

        app.commit_dialog
            .as_mut()
            .unwrap()
            .editor
            .insert_str("line one");

        let result = handle_input(key_event(KeyCode::Enter, KeyModifiers::NONE), &mut app);

        assert!(
            result.is_none(),
            "bare Enter should return None, not confirm"
        );
        assert!(
            app.commit_dialog.is_some(),
            "dialog should remain open after bare Enter"
        );
        let lines = app.commit_dialog.as_ref().unwrap().editor.lines();
        assert!(
            lines.len() >= 2,
            "editor should have at least 2 lines after bare Enter, got {lines:?}"
        );
    }

    /// Verifies that `[Alt+Enter]` with an empty editor keeps the dialog open.
    #[test]
    fn test_commit_dialog_empty_message_keeps_open() {
        let mut app = App::test_default();
        let task_id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        app.open_commit_dialog(&task_id);

        // The dialog pre-fills the editor; replace it with a blank one.
        app.commit_dialog.as_mut().unwrap().editor = tui_textarea::TextArea::default();

        // Alt+Enter on an empty message should NOT confirm.
        let result = handle_input(key_event(KeyCode::Enter, KeyModifiers::ALT), &mut app);

        assert!(
            result.is_none(),
            "Alt+Enter on empty message should return None"
        );
        assert!(
            app.commit_dialog.is_some(),
            "dialog should remain open when message is empty"
        );
    }

    /// Verifies that `[Ctrl+S]` in the commit dialog emits `HumanApprovedCommit`.
    #[test]
    fn test_commit_dialog_ctrl_s_emits_approved_commit() {
        let mut app = App::test_default();
        let task_id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        app.open_commit_dialog(&task_id);
        app.commit_dialog
            .as_mut()
            .unwrap()
            .editor
            .insert_str("feat: ctrl-s commit");
        let result = handle_input(
            key_event(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &mut app,
        );
        assert!(
            matches!(result, Some(AppMessage::HumanApprovedCommit { .. })),
            "Ctrl+S should emit HumanApprovedCommit, got: {result:?}"
        );
        assert!(
            app.commit_dialog.is_none(),
            "commit_dialog should be closed after Ctrl+S"
        );
    }

    /// Verifies that `[Ctrl+S]` with an empty editor keeps the dialog open.
    #[test]
    fn test_commit_dialog_ctrl_s_empty_message_keeps_open() {
        let mut app = App::test_default();
        let task_id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        app.open_commit_dialog(&task_id);
        app.commit_dialog.as_mut().unwrap().editor = tui_textarea::TextArea::default();
        let result = handle_input(
            key_event(KeyCode::Char('s'), KeyModifiers::CONTROL),
            &mut app,
        );
        assert!(
            result.is_none(),
            "Ctrl+S on empty message should return None"
        );
        assert!(
            app.commit_dialog.is_some(),
            "dialog should remain open when message is empty"
        );
    }

    /// Verifies that pressing `[Esc]` in the commit dialog closes it without emitting a message.
    #[test]
    fn test_commit_dialog_esc_cancels() {
        let mut app = App::test_default();
        let task_id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        app.open_commit_dialog(&task_id);
        assert!(
            app.commit_dialog.is_some(),
            "dialog should be open before Esc"
        );

        let result = handle_input(key_event(KeyCode::Esc, KeyModifiers::NONE), &mut app);

        assert!(result.is_none(), "Esc should return None");
        assert!(
            app.commit_dialog.is_none(),
            "dialog should be closed after Esc"
        );
    }

    /// Verifies that arbitrary keys in the commit dialog are consumed (go to editor, not tab handlers).
    #[test]
    fn test_commit_dialog_keys_consumed() {
        let mut app = App::test_default();
        let task_id = crate::tasks::TaskId::from_path("tasks/1.1.md");
        app.open_commit_dialog(&task_id);

        // 'q' should NOT quit -- it goes to the editor.
        let result = handle_input(key_event(KeyCode::Char('q'), KeyModifiers::NONE), &mut app);
        assert!(result.is_none(), "'q' in commit dialog should not quit");
        assert!(app.commit_dialog.is_some(), "dialog should still be open");
        assert!(!app.should_quit, "app should not quit");
    }
}
