//! Builds user messages from task context and prior agent work.
//!
//! The system prompt lives in the opencode agent definition file
//! (`.opencode/agents/clawmux/<agent>.md`). This module only composes the
//! user-facing message injected at runtime, combining task description, story
//! context, accumulated prior work, and any kickback reason.

use crate::tasks::models::Task;
use crate::workflow::agents::AgentKind;

/// Maximum number of work log entries to include in the composed message.
#[allow(dead_code)]
const MAX_WORK_LOG_ENTRIES: usize = 10;

/// Controls which optional sections are included in the composed message.
#[allow(dead_code)]
struct SectionConfig {
    include_prior_qa: bool,
    include_design: bool,
    include_impl_plan: bool,
    include_work_log: bool,
}

/// Returns the section configuration for the given agent based on its pipeline index.
#[allow(dead_code)]
fn section_config(agent: &AgentKind) -> SectionConfig {
    let idx = agent.pipeline_index();
    SectionConfig {
        include_prior_qa: idx >= 1,
        include_design: idx >= 1,
        include_impl_plan: idx >= 2,
        include_work_log: idx >= 2,
    }
}

/// Returns the static role description for the given agent.
#[allow(dead_code)]
fn role_description(agent: &AgentKind) -> &'static str {
    match agent {
        AgentKind::Intake => {
            "Your job is to gather the initial context, clarify requirements, and ask any \
             questions needed to fully understand the task before passing it to the Design Agent."
        }
        AgentKind::Design => {
            "Your job is to produce a thorough design for the task, documenting architectural \
             decisions and trade-offs so that the Planning Agent can create a detailed \
             implementation plan."
        }
        AgentKind::Planning => {
            "Your job is to translate the design into a concrete, step-by-step implementation \
             plan that the Implementation Agent can follow."
        }
        AgentKind::Implementation => {
            "Your job is to implement the code changes described in the implementation plan, \
             following the project conventions and passing all tests."
        }
        AgentKind::CodeQuality => {
            "Your job is to review the implementation for code quality, style, correctness, and \
             adherence to project conventions. Kick back to the Implementation Agent if issues \
             are found."
        }
        AgentKind::SecurityReview => {
            "Your job is to audit the implementation for security vulnerabilities and risks. \
             Kick back to Implementation or Design if issues are found."
        }
        AgentKind::CodeReview => {
            "Your job is to perform a final holistic review of the implementation before it goes \
             to the human for approval. Kick back to Implementation, Design, or Planning if \
             issues are found."
        }
        AgentKind::Human => {
            "You are a human reviewer. Review the completed task and approve or request revisions."
        }
    }
}

/// Builds the Task Context section.
#[allow(dead_code)]
fn section_task_context(task: &Task) -> String {
    format!(
        "## Task Context\n- Story: {}\n- Task: {}\n- Status: {}",
        task.story_name, task.name, task.status
    )
}

/// Builds the Description section, or returns `None` if the description is empty.
#[allow(dead_code)]
fn section_description(task: &Task) -> Option<String> {
    if task.description.trim().is_empty() {
        None
    } else {
        Some(format!("## Description\n{}", task.description))
    }
}

/// Builds the Prior Q&A section from answered questions only.
///
/// Returns `None` if there are no answered questions.
#[allow(dead_code)]
fn section_prior_qa(task: &Task) -> Option<String> {
    let answered: Vec<_> = task
        .questions
        .iter()
        .filter(|q| q.answer.is_some())
        .collect();
    if answered.is_empty() {
        return None;
    }
    let mut lines = vec!["## Prior Q&A".to_string()];
    for q in answered {
        lines.push(format!("Q ({}): {}", q.agent.display_name(), q.text));
        lines.push(format!("A: {}", q.answer.as_deref().unwrap_or("")));
    }
    Some(lines.join("\n"))
}

/// Builds the Design section, or returns `None` if design is absent or empty.
#[allow(dead_code)]
fn section_design(task: &Task) -> Option<String> {
    task.design
        .as_ref()
        .filter(|d| !d.trim().is_empty())
        .map(|d| format!("## Design\n{}", d))
}

/// Builds the Implementation Plan section, or returns `None` if absent or empty.
#[allow(dead_code)]
fn section_implementation_plan(task: &Task) -> Option<String> {
    task.implementation_plan
        .as_ref()
        .filter(|p| !p.trim().is_empty())
        .map(|p| format!("## Implementation Plan\n{}", p))
}

/// Builds the Work Log section from the last [`MAX_WORK_LOG_ENTRIES`] entries.
///
/// Returns `None` if the work log is empty.
#[allow(dead_code)]
fn section_work_log(task: &Task) -> Option<String> {
    if task.work_log.is_empty() {
        return None;
    }
    let total = task.work_log.len();
    let skip = total.saturating_sub(MAX_WORK_LOG_ENTRIES);
    let mut lines = vec!["## Work Log".to_string()];
    if total > MAX_WORK_LOG_ENTRIES {
        lines.push(format!(
            "(showing last {} of {} entries)",
            MAX_WORK_LOG_ENTRIES, total
        ));
    }
    for entry in task.work_log.iter().skip(skip) {
        match entry {
            crate::tasks::models::WorkLogEntry::Parsed {
                sequence,
                timestamp,
                agent,
                description,
            } => {
                lines.push(format!(
                    "[{}] {} [{}] {}",
                    sequence,
                    timestamp.format("%Y-%m-%dT%H:%M:%SZ"),
                    agent.display_name(),
                    description
                ));
            }
            crate::tasks::models::WorkLogEntry::Raw { text, .. } => {
                lines.push(format!("[raw] {text}"));
            }
        }
    }
    Some(lines.join("\n"))
}

/// Builds the Kickback Context section.
#[allow(dead_code)]
fn section_kickback(reason: &str) -> String {
    format!(
        "## Kickback Context\nThis task has been kicked back to you. Reason:\n{}",
        reason
    )
}

/// Builds the Your Role section for the given agent.
#[allow(dead_code)]
fn section_your_role(agent: &AgentKind) -> String {
    format!(
        "## Your Role\nYou are the {}, step {} of 7 in the ClawMux pipeline.\n{}",
        agent.display_name(),
        agent.pipeline_index() + 1,
        role_description(agent)
    )
}

/// Composes the user message for the given agent, task, and optional kickback reason.
///
/// The system prompt lives in the opencode agent definition file; this function
/// only builds the user-facing message injected at runtime. Sections are
/// assembled based on the agent's position in the pipeline:
///
/// - All agents receive: Task Context, Description, Kickback (if present), Your Role.
/// - Design and later agents (pipeline_index >= 1) also receive Prior Q&A and Design.
/// - Planning and later agents (pipeline_index >= 2) also receive Implementation Plan and Work Log.
///
/// # Preconditions
///
/// `agent` must not be [`AgentKind::Human`]. Human is not a pipeline step and
/// has no automated message to compose. This is enforced with a `debug_assert`.
#[allow(dead_code)]
pub fn compose_user_message(
    agent: &AgentKind,
    task: &Task,
    kickback_reason: Option<&str>,
) -> String {
    debug_assert_ne!(
        *agent,
        AgentKind::Human,
        "compose_user_message must not be called for AgentKind::Human"
    );
    let cfg = section_config(agent);
    let mut sections: Vec<String> = Vec::new();

    // Always-present: Task Context
    sections.push(section_task_context(task));

    // Always-present: Description (omitted if empty)
    if let Some(s) = section_description(task) {
        sections.push(s);
    }

    // Present when a kickback reason is provided
    if let Some(reason) = kickback_reason {
        sections.push(section_kickback(reason));
    }

    // Prior Q&A: Design and later agents (pipeline_index >= 1)
    if cfg.include_prior_qa {
        if let Some(s) = section_prior_qa(task) {
            sections.push(s);
        }
    }

    // Design: Design and later agents (pipeline_index >= 1)
    if cfg.include_design {
        if let Some(s) = section_design(task) {
            sections.push(s);
        }
    }

    // Implementation Plan: Planning and later agents (pipeline_index >= 2)
    if cfg.include_impl_plan {
        if let Some(s) = section_implementation_plan(task) {
            sections.push(s);
        }
    }

    // Work Log: Planning and later agents (pipeline_index >= 2)
    if cfg.include_work_log {
        if let Some(s) = section_work_log(task) {
            sections.push(s);
        }
    }

    // Always-present: Your Role
    sections.push(section_your_role(agent));

    sections.join("\n\n")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::Utc;

    use super::*;
    use crate::tasks::models::{Question, TaskId, TaskStatus, WorkLogEntry};

    fn make_test_task() -> Task {
        Task {
            id: TaskId::from_path("tasks/6.2.md"),
            story_name: "6. Workflow Engine".to_string(),
            name: "6.2".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: Some(AgentKind::Planning),
            description: "Implement the prompt composer.".to_string(),
            starting_prompt: None,
            questions: vec![
                Question {
                    agent: AgentKind::Intake,
                    text: "What is the scope?".to_string(),
                    answer: Some("Full pipeline.".to_string()),
                    opencode_request_id: None,
                },
                Question {
                    agent: AgentKind::Design,
                    text: "Unanswered question.".to_string(),
                    answer: None,
                    opencode_request_id: None,
                },
            ],
            design: Some("Use a SectionConfig struct.".to_string()),
            implementation_plan: Some("Step 1: write section builders.".to_string()),
            work_log: vec![
                WorkLogEntry::Parsed {
                    sequence: 1,
                    timestamp: Utc::now(),
                    agent: AgentKind::Intake,
                    description: "Gathered requirements.".to_string(),
                },
                WorkLogEntry::Parsed {
                    sequence: 2,
                    timestamp: Utc::now(),
                    agent: AgentKind::Design,
                    description: "Produced design.".to_string(),
                },
            ],
            file_path: PathBuf::from("tasks/6.2.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        }
    }

    #[test]
    fn test_compose_intake_excludes_design() {
        let task = make_test_task();
        let msg = compose_user_message(&AgentKind::Intake, &task, None);
        assert!(
            !msg.contains("## Design"),
            "Intake should not include ## Design"
        );
        assert!(
            !msg.contains("## Implementation Plan"),
            "Intake should not include ## Implementation Plan"
        );
        assert!(
            !msg.contains("## Prior Q&A"),
            "Intake should not include ## Prior Q&A"
        );
        assert!(
            !msg.contains("## Work Log"),
            "Intake should not include ## Work Log"
        );
    }

    #[test]
    fn test_compose_design_boundary() {
        let task = make_test_task();
        let msg = compose_user_message(&AgentKind::Design, &task, None);
        assert!(
            msg.contains("## Prior Q&A"),
            "Design should include ## Prior Q&A"
        );
        assert!(msg.contains("## Design"), "Design should include ## Design");
        assert!(
            !msg.contains("## Implementation Plan"),
            "Design should not include ## Implementation Plan"
        );
        assert!(
            !msg.contains("## Work Log"),
            "Design should not include ## Work Log"
        );
    }

    #[test]
    fn test_compose_planning_includes_design() {
        let task = make_test_task();
        let msg = compose_user_message(&AgentKind::Planning, &task, None);
        assert!(
            msg.contains("## Design"),
            "Planning should include ## Design"
        );
        assert!(
            msg.contains("Use a SectionConfig struct."),
            "Planning should include design content"
        );
    }

    #[test]
    fn test_compose_with_kickback() {
        let task = make_test_task();
        let reason = "Missing error handling.";
        let msg = compose_user_message(&AgentKind::Implementation, &task, Some(reason));
        assert!(
            msg.contains("## Kickback Context"),
            "Output should contain ## Kickback Context"
        );
        assert!(
            msg.contains(reason),
            "Output should contain the kickback reason"
        );
    }

    #[test]
    fn test_compose_without_kickback() {
        let task = make_test_task();
        let msg = compose_user_message(&AgentKind::Implementation, &task, None);
        assert!(
            !msg.contains("## Kickback Context"),
            "Output should not contain ## Kickback Context when no reason given"
        );
    }

    #[test]
    fn test_compose_work_log_truncated() {
        let mut task = make_test_task();
        task.work_log = (1u32..=15)
            .map(|i| WorkLogEntry::Parsed {
                sequence: i,
                timestamp: Utc::now(),
                agent: AgentKind::Implementation,
                description: format!("Work entry {}", i),
            })
            .collect();

        let msg = compose_user_message(&AgentKind::Planning, &task, None);

        assert!(
            msg.contains("showing last 10 of 15 entries"),
            "Should note truncation"
        );

        // Entries 6-15 must appear (check by sequence number prefix)
        for i in 6u32..=15 {
            assert!(
                msg.contains(&format!("[{}] ", i)),
                "Entry {} should be present",
                i
            );
        }

        // Entries 1-5 must not appear (check by sequence number prefix)
        for i in 1u32..=5 {
            assert!(
                !msg.contains(&format!("[{}] ", i)),
                "Entry {} should be absent",
                i
            );
        }
    }

    #[test]
    fn test_compose_includes_task_context() {
        let task = make_test_task();
        let msg = compose_user_message(&AgentKind::Intake, &task, None);
        assert!(
            msg.contains("6. Workflow Engine"),
            "Should contain story name"
        );
        assert!(msg.contains("6.2"), "Should contain task name");
        assert!(msg.contains("IN_PROGRESS"), "Should contain task status");
    }
}
