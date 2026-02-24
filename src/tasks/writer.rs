//! Task file serializer.
//!
//! Writes `Task` structs back to markdown files, preserving unknown sections
//! verbatim to ensure round-trip fidelity for agent-added or user-added content.
//! Task 2.2 implements the full serializer.

use crate::error::ClawdMuxError;
use crate::tasks::models::Task;

/// Serializes a [`Task`] to a markdown string in the canonical task file format.
///
/// Sections are written in the standard order: metadata block, Description,
/// Starting Prompt, Questions, Design, Implementation Plan, Work Log, then any
/// unknown sections from `task.extra_sections` (appended verbatim at the end).
///
/// Optional sections are omitted when their corresponding field is `None` or empty.
/// The output round-trips cleanly through [`crate::tasks::parser::parse_task`].
///
/// **Timestamp normalization**: work log timestamps are always written in RFC 3339
/// format with a `+00:00` suffix (e.g. `2026-02-10T10:00:01+00:00`), even if the
/// original file used a bare timestamp (`2026-02-10T10:00:01`). The first
/// `parse -> write` cycle normalizes timestamps; subsequent cycles are idempotent.
///
/// # Errors
///
/// Returns [`crate::error::ClawdMuxError::Encode`] if serialization fails (currently
/// infallible, but returns `Result` for API consistency with the rest of the codebase).
#[allow(dead_code)]
pub fn write_task(task: &Task) -> crate::error::Result<String> {
    if task.parse_error.is_some() {
        return Err(ClawdMuxError::Internal(format!(
            "write_task: refusing to overwrite malformed task '{}' with stub defaults",
            task.id
        )));
    }
    let mut out = String::new();

    // --- Metadata block ---
    out.push_str(&format!("Story: {}\n", task.story_name));
    out.push_str(&format!("Task: {}\n", task.name));
    out.push_str(&format!("Status: {}\n", task.status));
    if let Some(ref agent) = task.assigned_to {
        out.push_str(&format!("Assigned To: [{}]\n", agent.display_name()));
    }
    out.push('\n');

    // --- Description (always present) ---
    out.push_str("## Description\n\n");
    out.push_str(&task.description);
    out.push_str("\n\n");

    // --- Starting Prompt (optional) ---
    if let Some(ref prompt) = task.starting_prompt {
        out.push_str("## Starting Prompt\n\n");
        out.push_str(prompt);
        out.push_str("\n\n");
    }

    // --- Questions (optional) ---
    if !task.questions.is_empty() {
        out.push_str("## Questions\n\n");
        for (i, q) in task.questions.iter().enumerate() {
            let n = i + 1;
            if i > 0 {
                out.push('\n'); // blank line between questions
            }
            out.push_str(&format!("Q{n} [{}]: {}\n", q.agent.display_name(), q.text));
            if let Some(ref answer) = q.answer {
                out.push_str(&format!("A{n}: {answer}\n"));
            }
        }
        out.push('\n'); // blank line to close section
    }

    // --- Design (optional) ---
    if let Some(ref design) = task.design {
        out.push_str("## Design\n\n");
        out.push_str(design);
        out.push_str("\n\n");
    }

    // --- Implementation Plan (optional) ---
    if let Some(ref plan) = task.implementation_plan {
        out.push_str("## Implementation Plan\n\n");
        out.push_str(plan);
        out.push_str("\n\n");
    }

    // --- Work Log (optional) ---
    if !task.work_log.is_empty() {
        out.push_str("## Work Log\n\n");
        for entry in &task.work_log {
            match entry {
                crate::tasks::models::WorkLogEntry::Parsed {
                    sequence,
                    timestamp,
                    agent,
                    description,
                } => {
                    out.push_str(
                        format!(
                            "{} {} [{}] {}",
                            sequence,
                            timestamp.to_rfc3339(),
                            agent.display_name(),
                            description
                        )
                        .trim_end(),
                    );
                    out.push('\n');
                }
                crate::tasks::models::WorkLogEntry::Raw { text, .. } => {
                    out.push_str(text);
                    out.push('\n');
                }
            }
        }
        out.push('\n'); // blank line to close section
    }

    // --- Extra sections (verbatim, appended at end) ---
    for (heading, body) in &task.extra_sections {
        out.push_str(&format!("## {heading}\n\n"));
        out.push_str(body);
        out.push_str("\n\n");
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use chrono::TimeZone;

    use super::*;
    use crate::tasks::models::{Question, TaskId, TaskStatus, WorkLogEntry};
    use crate::tasks::parser::parse_task;
    use crate::workflow::agents::AgentKind;

    /// Canonical full sample — mirrors `FULL_SAMPLE` in parser tests.
    const FULL_SAMPLE: &str = "\
Story: 1. Big Story
Task: 1.1 First Task
Status: IN_PROGRESS
Assigned To: [Planning Agent]

## Description

<description of the task>

## Starting Prompt

<optional starting prompt provided by team leader>

## Questions

Q1 [Intake Agent]: What language do you want to use for this task?
A1: Lets use rust, it is well suited to this.

## Design

<Design considerations to use for this task>

## Implementation Plan

<Plan to use for this task>

## Work Log

1 2026-02-10T10:00:01 [Design Agent] updated task with design and assigned task to [Planning Agent] for next step.
";

    fn path(name: &str) -> PathBuf {
        PathBuf::from(format!("tasks/{name}.md"))
    }

    fn minimal_task() -> Task {
        Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. My Story".to_string(),
            name: "1.1".to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: "A simple description.".to_string(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        }
    }

    #[test]
    fn test_write_minimal() {
        let task = minimal_task();
        let out = write_task(&task).unwrap();

        assert!(out.contains("Story: 1. My Story\n"));
        assert!(out.contains("Task: 1.1\n"));
        assert!(out.contains("Status: OPEN\n"));
        assert!(out.contains("## Description"));
        assert!(out.contains("A simple description."));

        // Optional sections must not appear.
        assert!(!out.contains("Assigned To:"));
        assert!(!out.contains("## Starting Prompt"));
        assert!(!out.contains("## Questions"));
        assert!(!out.contains("## Design"));
        assert!(!out.contains("## Implementation Plan"));
        assert!(!out.contains("## Work Log"));
    }

    #[test]
    fn test_write_full() {
        let ts = chrono::Utc.with_ymd_and_hms(2026, 2, 10, 10, 0, 1).unwrap();
        let task = Task {
            id: TaskId::from_path("tasks/1.1.md"),
            story_name: "1. Big Story".to_string(),
            name: "1.1 First Task".to_string(),
            status: TaskStatus::InProgress,
            assigned_to: Some(AgentKind::Planning),
            description: "A full description.".to_string(),
            starting_prompt: Some("A starting prompt.".to_string()),
            questions: vec![Question {
                agent: AgentKind::Intake,
                text: "What is your name?".to_string(),
                answer: Some("Alice.".to_string()),
            }],
            design: Some("Design notes.".to_string()),
            implementation_plan: Some("Plan notes.".to_string()),
            work_log: vec![WorkLogEntry::Parsed {
                sequence: 1,
                timestamp: ts,
                agent: AgentKind::Design,
                description: "Did some work.".to_string(),
            }],
            file_path: PathBuf::from("tasks/1.1.md"),
            extra_sections: Vec::new(),
            parse_error: None,
        };
        let out = write_task(&task).unwrap();

        assert!(out.contains("Story: 1. Big Story\n"));
        assert!(out.contains("Assigned To: [Planning Agent]\n"));
        assert!(out.contains("## Description"));
        assert!(out.contains("## Starting Prompt"));
        assert!(out.contains("## Questions"));
        assert!(out.contains("## Design"));
        assert!(out.contains("## Implementation Plan"));
        assert!(out.contains("## Work Log"));
        assert!(out.contains("Q1 [Intake Agent]: What is your name?"));
        assert!(out.contains("A1: Alice."));
        assert!(out.contains("Design notes."));
        assert!(out.contains("Plan notes."));
        assert!(out.contains("[Design Agent] Did some work."));
    }

    #[test]
    fn test_round_trip() {
        let p = path("1.1-full");
        let task1 = parse_task(FULL_SAMPLE, p.clone()).unwrap();
        let written = write_task(&task1).unwrap();
        let task2 = parse_task(&written, p).unwrap();
        assert_eq!(task1, task2);
    }

    #[test]
    fn test_round_trip_with_extra_section() {
        let content = "\
Story: S
Task: T
Status: OPEN

## Description

desc

## Foo

bar content
";
        let p = path("t");
        let task1 = parse_task(content, p.clone()).unwrap();
        assert_eq!(task1.extra_sections.len(), 1);

        let written = write_task(&task1).unwrap();
        assert!(written.contains("## Foo"), "## Foo must be preserved");
        assert!(
            written.contains("bar content"),
            "Foo body must be preserved"
        );

        // Verify the round-trip also parses cleanly.
        let task2 = parse_task(&written, p).unwrap();
        assert_eq!(task1, task2);
    }

    #[test]
    fn test_write_malformed_task_returns_error() {
        use crate::tasks::models::ParseErrorInfo;

        let mut task = minimal_task();
        task.parse_error = Some(ParseErrorInfo {
            error_message: "missing Status".to_string(),
            raw_content: "bad content".to_string(),
            suggested_fix: None,
            fix_in_progress: false,
            fix_error: None,
        });
        let result = write_task(&task);
        assert!(
            result.is_err(),
            "write_task should refuse to write a malformed task"
        );
    }

    #[test]
    fn test_write_raw_work_log_entry() {
        let mut task = minimal_task();
        task.work_log = vec![
            WorkLogEntry::Parsed {
                sequence: 1,
                timestamp: chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
                agent: AgentKind::Design,
                description: "Normal entry.".to_string(),
            },
            WorkLogEntry::Raw {
                text: "this line could not be parsed".to_string(),
                warning: "bad sequence number: 'this'".to_string(),
            },
        ];

        let out = write_task(&task).unwrap();
        assert!(out.contains("## Work Log"), "should have Work Log section");
        assert!(
            out.contains("[Design Agent] Normal entry."),
            "parsed entry should be formatted normally"
        );
        assert!(
            out.contains("this line could not be parsed"),
            "raw entry text should be written verbatim"
        );
        // The raw warning should NOT appear in the output (it's metadata, not file content).
        assert!(
            !out.contains("bad sequence number"),
            "raw warning should not appear in output"
        );
    }

    #[test]
    fn test_omits_none_fields() {
        let mut task = minimal_task();
        task.design = None;
        task.implementation_plan = None;

        let out = write_task(&task).unwrap();
        assert!(!out.contains("## Design"));
        assert!(!out.contains("## Implementation Plan"));
    }

    #[test]
    fn test_write_multiple_questions() {
        let mut task = minimal_task();
        task.questions = vec![
            Question {
                agent: AgentKind::Intake,
                text: "First?".to_string(),
                answer: Some("Yes.".to_string()),
            },
            Question {
                agent: AgentKind::Design,
                text: "Second?".to_string(),
                answer: Some("No.".to_string()),
            },
            Question {
                agent: AgentKind::Planning,
                text: "Third?".to_string(),
                answer: None,
            },
        ];

        let out = write_task(&task).unwrap();
        assert!(out.contains("Q1 [Intake Agent]: First?"));
        assert!(out.contains("A1: Yes."));
        assert!(out.contains("Q2 [Design Agent]: Second?"));
        assert!(out.contains("A2: No."));
        assert!(out.contains("Q3 [Planning Agent]: Third?"));
        // Q3 has no answer, so A3 must not appear.
        assert!(!out.contains("A3:"));

        // Verify numbering by checking positions in the string.
        let q1_pos = out.find("Q1 ").unwrap();
        let q2_pos = out.find("Q2 ").unwrap();
        let q3_pos = out.find("Q3 ").unwrap();
        assert!(q1_pos < q2_pos && q2_pos < q3_pos);
    }
}
