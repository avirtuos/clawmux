//! Tab bar and tab dispatch.
//!
//! Renders the 9-tab right pane (Details, Questions, Design, Plan, Agent Activity, Team Status, Review, Code Diff, Research)
//! and dispatches input events to the currently active tab.

use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Tabs};
use ratatui::{symbols, Frame};

use crate::app::App;

pub mod agent_activity;
pub mod code_review;
pub mod design;
pub mod plan;
pub mod questions;
pub mod research;
pub mod review;
pub mod task_details;
pub mod team_status;

/// Returns tab titles for the eight right-pane tabs.
///
/// Appends `*` to "Questions" when the selected task has any unanswered questions,
/// so the user can see at a glance that input is needed.
pub fn tab_titles(app: &App) -> Vec<&'static str> {
    let has_unanswered = app
        .selected_task()
        .and_then(|id| app.task_store.get(id))
        .map(|t| t.questions.iter().any(|q| q.answer.is_none()))
        .unwrap_or(false);

    vec![
        "Details",
        if has_unanswered {
            "Questions*"
        } else {
            "Questions"
        },
        "Design",
        "Plan",
        "Agent Activity",
        "Team Status",
        "Review",
        "Code Diff",
        "Research",
    ]
}

/// Renders the tab bar and the currently active tab's content into `area`.
///
/// Splits `area` into a 3-row tab bar at the top and a content area below.
/// Highlights the tab corresponding to `app.active_tab`.
/// Dispatches rendering to the appropriate tab module.
pub fn render(frame: &mut Frame, area: ratatui::layout::Rect, app: &App) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let tab_bar_area = layout[0];
    let content_area = layout[1];

    // Build tab bar with dynamic titles.
    let titles: Vec<ratatui::text::Line> = tab_titles(app)
        .iter()
        .map(|t| ratatui::text::Line::from(*t))
        .collect();

    let tabs = Tabs::new(titles)
        .block(Block::default().borders(Borders::ALL).title(""))
        .select(app.active_tab)
        .style(Style::default().fg(Color::White))
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .divider(symbols::line::VERTICAL);

    frame.render_widget(tabs, tab_bar_area);

    // Dispatch to the active tab.
    match app.active_tab {
        0 => {
            let task = app.selected_task().and_then(|id| app.task_store.get(id));
            task_details::render(frame, content_area, task, &app.tab1_state);
        }
        1 => {
            let task = app.selected_task().and_then(|id| app.task_store.get(id));
            questions::render(frame, content_area, task, &app.questions_state);
        }
        2 => {
            let task = app.selected_task().and_then(|id| app.task_store.get(id));
            design::render(frame, content_area, task, &app.design_state);
        }
        3 => {
            let task = app.selected_task().and_then(|id| app.task_store.get(id));
            plan::render(frame, content_area, task, &app.plan_state);
        }
        4 => {
            let task_id = app.selected_task();
            agent_activity::render(frame, content_area, task_id, &app.tab2_state);
        }
        5 => {
            let task_id = app.selected_task();
            let task = task_id.and_then(|id| app.task_store.get(id));
            let wf_state = task_id.and_then(|id| app.workflow_engine.state(id));
            team_status::render(frame, content_area, task, wf_state, &app.tab3_state);
        }
        6 => {
            let task_id = app.selected_task();
            review::render(
                frame,
                content_area,
                task_id,
                &app.review_state,
                &app.tab4_state,
            );
        }
        7 => {
            let task_id = app.selected_task();
            code_review::render(frame, content_area, task_id, &app.tab4_state);
        }
        8 => {
            research::render(frame, content_area, &app.research_state);
        }
        _ => {
            let titles = tab_titles(app);
            let label = titles.get(app.active_tab).copied().unwrap_or("Unknown");
            let placeholder = Paragraph::new(format!("{label}: Not yet implemented"))
                .style(Style::default().fg(Color::DarkGray))
                .block(Block::default().borders(Borders::ALL));
            frame.render_widget(placeholder, content_area);
        }
    }
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    use super::*;
    use crate::app::App;

    #[test]
    fn test_tab_bar_renders_nine_tabs() {
        let backend = TestBackend::new(200, 24);
        let mut terminal = Terminal::new(backend).unwrap();
        let app = App::test_default();

        terminal
            .draw(|frame| {
                render(frame, frame.area(), &app);
            })
            .unwrap();

        let buf = terminal.backend().buffer().clone();
        let content: String = buf
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect();

        assert!(
            content.contains("Details"),
            "Buffer should contain 'Details' tab label"
        );
        assert!(
            content.contains("Questions"),
            "Buffer should contain 'Questions' tab label"
        );
        assert!(
            content.contains("Design"),
            "Buffer should contain 'Design' tab label"
        );
        assert!(
            content.contains("Plan"),
            "Buffer should contain 'Plan' tab label"
        );
        assert!(
            content.contains("Agent Activity"),
            "Buffer should contain 'Agent Activity' tab label"
        );
        assert!(
            content.contains("Team Status"),
            "Buffer should contain 'Team Status' tab label"
        );
        assert!(
            content.contains("Review"),
            "Buffer should contain 'Review' tab label"
        );
        assert!(
            content.contains("Code Diff"),
            "Buffer should contain 'Code Diff' tab label"
        );
        assert!(
            content.contains("Research"),
            "Buffer should contain 'Research' tab label"
        );
    }

    #[test]
    fn test_tab_titles_questions_star_when_unanswered() {
        use crate::tasks::models::{Question, Task, TaskId, TaskStatus};
        use crate::workflow::agents::AgentKind;
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
            questions: vec![Question {
                agent: AgentKind::Intake,
                text: "scope?".to_string(),
                answer: None,
                opencode_request_id: None,
            }],
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
        app.task_list_state.selected_index = 1; // select the task row

        let titles = tab_titles(&app);
        assert!(
            titles.contains(&"Questions*"),
            "tab title should be 'Questions*' when unanswered questions exist; got: {titles:?}"
        );
    }

    #[test]
    fn test_tab_titles_questions_no_star_when_answered() {
        use crate::tasks::models::{Question, Task, TaskId, TaskStatus};
        use crate::workflow::agents::AgentKind;
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
            questions: vec![Question {
                agent: AgentKind::Intake,
                text: "scope?".to_string(),
                answer: Some("Minimal.".to_string()),
                opencode_request_id: None,
            }],
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

        let titles = tab_titles(&app);
        assert!(
            titles.contains(&"Questions"),
            "tab title should be 'Questions' when all answered; got: {titles:?}"
        );
        assert!(
            !titles.contains(&"Questions*"),
            "tab title should not have '*' when all answered; got: {titles:?}"
        );
    }
}
