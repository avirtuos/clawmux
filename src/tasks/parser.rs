//! Markdown task file parser.
//!
//! Uses a two-phase line-oriented approach:
//! - Phase 1: parse `Key: Value` metadata lines before the first `##` heading.
//! - Phase 2: split on `##` headings; parse known sections and preserve unknown
//!   ones verbatim for round-trip fidelity.

use std::path::PathBuf;

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};

use crate::error::ClawdMuxError;
use crate::tasks::models::{ParseErrorInfo, Question, Task, TaskId, TaskStatus, WorkLogEntry};
use crate::workflow::agents::AgentKind;

/// Parses a task markdown file's content into a [`Task`] struct.
///
/// Uses a two-phase approach:
/// - Phase 1: Parses `Key: Value` metadata lines before the first `##` heading.
/// - Phase 2: Splits on `##` headings; known sections are parsed structurally,
///   unknown sections are preserved verbatim for round-trip fidelity.
///
/// # Errors
///
/// Returns [`ClawdMuxError::Parse`] if required metadata fields are missing or
/// any field value cannot be parsed.
#[allow(dead_code)]
pub fn parse_task(content: &str, file_path: PathBuf) -> crate::error::Result<Task> {
    let file_name = file_path.to_str().unwrap_or("<unknown>").to_string();

    let lines: Vec<&str> = content.lines().collect();

    // --- Phase 1: Metadata ---
    let mut story_name: Option<String> = None;
    let mut task_name: Option<String> = None;
    let mut status: Option<TaskStatus> = None;
    let mut assigned_to: Option<AgentKind> = None;

    let mut section_start = lines.len(); // index of the first `## ` line
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("## ") {
            section_start = i;
            break;
        }
        // Skip blank lines
        if line.trim().is_empty() {
            continue;
        }
        // Parse key: value pairs (split on first `: `)
        if let Some((key, value)) = line.split_once(": ") {
            let key_lower = key.trim().to_lowercase();
            let value = value.trim();
            match key_lower.as_str() {
                "story" => story_name = Some(value.to_string()),
                "task" => task_name = Some(value.to_string()),
                "status" => {
                    status =
                        Some(
                            value
                                .parse::<TaskStatus>()
                                .map_err(|_| ClawdMuxError::Parse {
                                    file: file_name.clone(),
                                    message: format!("invalid Status value: '{value}'"),
                                })?,
                        );
                }
                "assigned to" => {
                    let inner = strip_brackets(value).unwrap_or(value);
                    assigned_to = Some(AgentKind::from_display_name(inner).map_err(|_| {
                        ClawdMuxError::Parse {
                            file: file_name.clone(),
                            message: format!("invalid Assigned To value: '{inner}'"),
                        }
                    })?);
                }
                _ => {} // unknown keys silently ignored
            }
        }
    }

    let story_name = story_name.ok_or_else(|| ClawdMuxError::Parse {
        file: file_name.clone(),
        message: "missing required 'Story:' field".to_string(),
    })?;
    let name = task_name.ok_or_else(|| ClawdMuxError::Parse {
        file: file_name.clone(),
        message: "missing required 'Task:' field".to_string(),
    })?;
    let status = status.ok_or_else(|| ClawdMuxError::Parse {
        file: file_name.clone(),
        message: "missing required 'Status:' field".to_string(),
    })?;

    // --- Phase 2: Section splitting ---
    // Collect (heading, body_lines) pairs from section_start onwards.
    let mut sections: Vec<(&str, Vec<&str>)> = Vec::new();
    let mut current_heading: Option<&str> = None;
    let mut current_body: Vec<&str> = Vec::new();

    for line in &lines[section_start..] {
        if let Some(heading) = line.strip_prefix("## ") {
            if let Some(h) = current_heading.take() {
                sections.push((h, std::mem::take(&mut current_body)));
            }
            current_heading = Some(heading.trim());
        } else if current_heading.is_some() {
            current_body.push(line);
        }
    }
    if let Some(h) = current_heading {
        sections.push((h, current_body));
    }

    // --- Populate task fields from sections ---
    let mut description = String::new();
    let mut starting_prompt: Option<String> = None;
    let mut questions: Vec<Question> = Vec::new();
    let mut design: Option<String> = None;
    let mut implementation_plan: Option<String> = None;
    let mut work_log: Vec<WorkLogEntry> = Vec::new();
    let mut extra_sections: Vec<(String, String)> = Vec::new();

    for (heading, body_lines) in sections {
        let heading_lower = heading.to_lowercase();
        match heading_lower.as_str() {
            "description" => {
                description = trim_section_body(&body_lines);
            }
            "starting prompt" => {
                let trimmed = trim_section_body(&body_lines);
                starting_prompt = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
            }
            "questions" => {
                let body = trim_section_body(&body_lines);
                questions = parse_questions(&body, &file_name)?;
            }
            "design" => {
                let trimmed = trim_section_body(&body_lines);
                design = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
            }
            "implementation plan" => {
                let trimmed = trim_section_body(&body_lines);
                implementation_plan = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                };
            }
            "work log" => {
                let body = trim_section_body(&body_lines);
                work_log = parse_work_log(&body, &file_name)?;
            }
            _ => {
                let body = trim_section_body(&body_lines);
                extra_sections.push((heading.to_string(), body));
            }
        }
    }

    Ok(Task {
        id: TaskId::from_path(file_path.clone()),
        story_name,
        name,
        status,
        assigned_to,
        description,
        starting_prompt,
        questions,
        design,
        implementation_plan,
        work_log,
        file_path,
        extra_sections,
        parse_error: None,
    })
}

/// Scans lines before the first `## ` heading for `Story:` and `Task:` values.
///
/// Best-effort; returns `(story_name, task_name)` with `None` for any field
/// that is absent or cannot be extracted. No validation is performed.
#[allow(dead_code)]
pub fn extract_metadata_hints(content: &str) -> (Option<String>, Option<String>) {
    let mut story_name: Option<String> = None;
    let mut task_name: Option<String> = None;
    for line in content.lines() {
        if line.starts_with("## ") {
            break;
        }
        if let Some((key, value)) = line.split_once(": ") {
            match key.trim().to_lowercase().as_str() {
                "story" => story_name = Some(value.trim().to_string()),
                "task" => task_name = Some(value.trim().to_string()),
                _ => {}
            }
        }
    }
    (story_name, task_name)
}

/// Builds a stub [`Task`] for a file that failed to parse.
///
/// Attempts to extract `Story:` and `Task:` values from the raw content for
/// best-effort display. Falls back to `"Unknown Story"` / the file stem when
/// those fields are absent. The returned task has `parse_error: Some(...)` set.
#[allow(dead_code)]
pub fn create_malformed_task(content: &str, file_path: PathBuf, error_message: String) -> Task {
    let (story_hint, task_hint) = extract_metadata_hints(content);
    let story_name = story_hint.unwrap_or_else(|| "Unknown Story".to_string());
    let name = task_hint.unwrap_or_else(|| {
        file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string()
    });
    Task {
        id: TaskId::from_path(file_path.clone()),
        story_name,
        name,
        status: TaskStatus::Open,
        assigned_to: None,
        description: String::new(),
        starting_prompt: None,
        questions: Vec::new(),
        design: None,
        implementation_plan: None,
        work_log: Vec::new(),
        file_path,
        extra_sections: Vec::new(),
        parse_error: Some(ParseErrorInfo {
            error_message,
            raw_content: content.to_string(),
            suggested_fix: None,
            fix_in_progress: false,
        }),
    }
}

/// Strips leading and trailing blank lines from a slice of lines, then joins
/// with `\n`.
#[allow(dead_code)]
fn trim_section_body(lines: &[&str]) -> String {
    // Find first non-blank line.
    let start = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .unwrap_or(lines.len());
    // Find last non-blank line.
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    if start >= end {
        return String::new();
    }
    lines[start..end].join("\n")
}

/// Parses the body of a `## Questions` section into a list of [`Question`]s.
///
/// Expected format per question:
/// ```text
/// Q<n> [<agent display name>]: <question text>
/// A<n>: <answer text>
/// ```
/// The answer line is optional.
#[allow(dead_code)]
fn parse_questions(body: &str, file: &str) -> crate::error::Result<Vec<Question>> {
    let mut questions: Vec<Question> = Vec::new();
    if body.is_empty() {
        return Ok(questions);
    }
    let lines: Vec<&str> = body.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim();
        // Detect question lines: start with 'Q' followed by a digit.
        if line.starts_with('Q')
            && line.len() > 1
            && line.chars().nth(1).is_some_and(|c| c.is_ascii_digit())
        {
            // Extract sequence number (characters between 'Q' and the first space or '[').
            let after_q = &line[1..];
            let seq_end = after_q
                .find(|c: char| !c.is_ascii_digit())
                .unwrap_or(after_q.len());
            let seq: u32 = after_q[..seq_end].parse().unwrap_or(0);

            // Extract agent from [...] brackets.
            let agent = if let Some(bracket_start) = line.find('[') {
                if let Some(bracket_end) = line.find(']') {
                    let inner = &line[bracket_start + 1..bracket_end];
                    AgentKind::from_display_name(inner).map_err(|_| ClawdMuxError::Parse {
                        file: file.to_string(),
                        message: format!("invalid agent in question: '{inner}'"),
                    })?
                } else {
                    return Err(ClawdMuxError::Parse {
                        file: file.to_string(),
                        message: format!("malformed question line (no closing ']'): '{line}'"),
                    });
                }
            } else {
                return Err(ClawdMuxError::Parse {
                    file: file.to_string(),
                    message: format!("malformed question line (no '[' bracket): '{line}'"),
                });
            };

            // Extract text after ']: '.
            let text = if let Some(colon_pos) = line.find("]: ") {
                line[colon_pos + 3..].trim().to_string()
            } else {
                String::new()
            };

            // Look ahead for a matching answer line (Seq is `seq`).
            let answer_prefix = format!("A{seq}:");
            let mut answer: Option<String> = None;
            let mut j = i + 1;
            while j < lines.len() {
                let next = lines[j].trim();
                if next.is_empty() {
                    j += 1;
                    continue;
                }
                if next.starts_with(&answer_prefix) {
                    let ans_text = next[answer_prefix.len()..].trim().to_string();
                    answer = Some(ans_text);
                    i = j; // advance outer loop past the answer line
                }
                break;
            }

            questions.push(Question {
                agent,
                text,
                answer,
            });
        }
        i += 1;
    }
    Ok(questions)
}

/// Parses the body of a `## Work Log` section into a list of [`WorkLogEntry`]s.
///
/// Expected format per entry:
/// ```text
/// <seq> <ISO8601_timestamp> [<agent display name>] <description>
/// ```
#[allow(dead_code)]
fn parse_work_log(body: &str, file: &str) -> crate::error::Result<Vec<WorkLogEntry>> {
    let mut entries: Vec<WorkLogEntry> = Vec::new();
    if body.is_empty() {
        return Ok(entries);
    }
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut tokens = line.splitn(4, ' ');

        // Token 1: sequence number.
        let seq_str = tokens.next().unwrap_or("");
        let sequence: u32 = seq_str.parse().map_err(|_| ClawdMuxError::Parse {
            file: file.to_string(),
            message: format!("invalid sequence number in work log: '{seq_str}'"),
        })?;

        // Token 2: timestamp.
        let ts_str = tokens.next().unwrap_or("");
        let timestamp = parse_timestamp(ts_str).ok_or_else(|| ClawdMuxError::Parse {
            file: file.to_string(),
            message: format!("invalid timestamp in work log: '{ts_str}'"),
        })?;

        // Remainder: `[Agent Name] description`.
        let rest = tokens.next().unwrap_or("").to_string()
            + tokens
                .next()
                .map(|s| format!(" {s}"))
                .unwrap_or_default()
                .as_str();
        let rest = rest.trim();

        // Extract agent from [...].
        let (agent, description) = if let Some(bracket_end) = rest.find(']') {
            let inner = &rest[1..bracket_end]; // skip leading '['
            let agent = AgentKind::from_display_name(inner).map_err(|_| ClawdMuxError::Parse {
                file: file.to_string(),
                message: format!("invalid agent in work log: '{inner}'"),
            })?;
            let desc = rest[bracket_end + 1..].trim().to_string();
            (agent, desc)
        } else {
            return Err(ClawdMuxError::Parse {
                file: file.to_string(),
                message: format!("malformed work log entry (no agent brackets): '{line}'"),
            });
        };

        entries.push(WorkLogEntry {
            sequence,
            timestamp,
            agent,
            description,
        });
    }
    Ok(entries)
}

/// Attempts to parse an ISO 8601 timestamp string as a UTC `DateTime`.
///
/// Tries RFC 3339 first (handles trailing `Z`), then falls back to
/// `%Y-%m-%dT%H:%M:%S` treated as UTC.
#[allow(dead_code)]
fn parse_timestamp(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(ndt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
        return Some(Utc.from_utc_datetime(&ndt));
    }
    None
}

/// If `s` is wrapped in `[...]`, strips the brackets and returns the inner content.
/// Otherwise returns `None`.
#[allow(dead_code)]
fn strip_brackets(s: &str) -> Option<&str> {
    let s = s.trim();
    if s.starts_with('[') && s.ends_with(']') {
        Some(&s[1..s.len() - 1])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::models::{ParseErrorInfo, TaskStatus};

    /// Minimal valid task file with only the required metadata and a Description section.
    const MINIMAL: &str = "\
Story: 1. Big Story
Task: 1.1 First Task
Status: OPEN

## Description

A minimal task description.
";

    /// Full sample from docs/requirements.md.
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

    #[test]
    fn test_parse_minimal() {
        let task = parse_task(MINIMAL, path("1.1-minimal")).unwrap();
        assert_eq!(task.story_name, "1. Big Story");
        assert_eq!(task.name, "1.1 First Task");
        assert_eq!(task.status, TaskStatus::Open);
        assert_eq!(task.assigned_to, None);
        assert_eq!(task.description, "A minimal task description.");
        assert_eq!(task.starting_prompt, None);
        assert!(task.questions.is_empty());
        assert_eq!(task.design, None);
        assert_eq!(task.implementation_plan, None);
        assert!(task.work_log.is_empty());
        assert!(task.extra_sections.is_empty());
    }

    #[test]
    fn test_parse_full_sample() {
        let task = parse_task(FULL_SAMPLE, path("1.1-full")).unwrap();
        assert_eq!(task.story_name, "1. Big Story");
        assert_eq!(task.name, "1.1 First Task");
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.assigned_to, Some(AgentKind::Planning));
        assert_eq!(task.description, "<description of the task>");
        assert_eq!(
            task.starting_prompt,
            Some("<optional starting prompt provided by team leader>".to_string())
        );
        assert_eq!(task.questions.len(), 1);
        assert_eq!(task.questions[0].agent, AgentKind::Intake);
        assert_eq!(
            task.questions[0].text,
            "What language do you want to use for this task?"
        );
        assert_eq!(
            task.questions[0].answer,
            Some("Lets use rust, it is well suited to this.".to_string())
        );
        assert_eq!(
            task.design,
            Some("<Design considerations to use for this task>".to_string())
        );
        assert_eq!(
            task.implementation_plan,
            Some("<Plan to use for this task>".to_string())
        );
        assert_eq!(task.work_log.len(), 1);
        assert_eq!(task.work_log[0].sequence, 1);
        assert_eq!(task.work_log[0].agent, AgentKind::Design);
        assert!(task.extra_sections.is_empty());
    }

    #[test]
    fn test_parse_assigned_to() {
        let content = "Story: S\nTask: T\nStatus: OPEN\nAssigned To: [Planning Agent]\n\n## Description\n\nx\n";
        let task = parse_task(content, path("t")).unwrap();
        assert_eq!(task.assigned_to, Some(AgentKind::Planning));
    }

    #[test]
    fn test_parse_assigned_to_human() {
        let content =
            "Story: S\nTask: T\nStatus: OPEN\nAssigned To: [Human]\n\n## Description\n\nx\n";
        let task = parse_task(content, path("t")).unwrap();
        assert_eq!(task.assigned_to, Some(AgentKind::Human));
    }

    #[test]
    fn test_parse_no_assigned_to() {
        let task = parse_task(MINIMAL, path("t")).unwrap();
        assert_eq!(task.assigned_to, None);
    }

    #[test]
    fn test_parse_unknown_section() {
        let content = "\
Story: S
Task: T
Status: OPEN

## Description

some desc

## Foo

bar content
";
        let task = parse_task(content, path("t")).unwrap();
        assert_eq!(task.extra_sections.len(), 1);
        assert_eq!(task.extra_sections[0].0, "Foo");
        assert_eq!(task.extra_sections[0].1, "bar content");
    }

    #[test]
    fn test_parse_multi_line_description() {
        let content = "\
Story: S
Task: T
Status: OPEN

## Description

First paragraph.

Second paragraph.
";
        let task = parse_task(content, path("t")).unwrap();
        assert_eq!(task.description, "First paragraph.\n\nSecond paragraph.");
    }

    #[test]
    fn test_parse_empty_questions() {
        let content = "\
Story: S
Task: T
Status: OPEN

## Description

x

## Questions

";
        let task = parse_task(content, path("t")).unwrap();
        assert!(task.questions.is_empty());
    }

    #[test]
    fn test_parse_unanswered_question() {
        let content = "\
Story: S
Task: T
Status: OPEN

## Description

x

## Questions

Q1 [Intake Agent]: An unanswered question?
";
        let task = parse_task(content, path("t")).unwrap();
        assert_eq!(task.questions.len(), 1);
        assert_eq!(task.questions[0].answer, None);
    }

    #[test]
    fn test_parse_invalid_status() {
        let content = "Story: S\nTask: T\nStatus: BOGUS\n\n## Description\n\nx\n";
        let err = parse_task(content, path("t")).unwrap_err();
        assert!(matches!(err, ClawdMuxError::Parse { .. }));
    }

    #[test]
    fn test_parse_missing_story_field() {
        let content = "Task: T\nStatus: OPEN\n\n## Description\n\nx\n";
        let err = parse_task(content, path("t")).unwrap_err();
        assert!(matches!(&err, ClawdMuxError::Parse { message, .. } if message.contains("Story")));
    }

    #[test]
    fn test_parse_missing_task_field() {
        let content = "Story: S\nStatus: OPEN\n\n## Description\n\nx\n";
        let err = parse_task(content, path("t")).unwrap_err();
        assert!(matches!(&err, ClawdMuxError::Parse { message, .. } if message.contains("Task")));
    }

    #[test]
    fn test_extract_metadata_hints_both_present() {
        let content =
            "Story: 1. My Story\nTask: 1.1 My Task\nStatus: OPEN\n\n## Description\n\nx\n";
        let (story, task) = extract_metadata_hints(content);
        assert_eq!(story.as_deref(), Some("1. My Story"));
        assert_eq!(task.as_deref(), Some("1.1 My Task"));
    }

    #[test]
    fn test_extract_metadata_hints_only_story() {
        let content = "Story: 2. Story Only\nthis is garbage\n";
        let (story, task) = extract_metadata_hints(content);
        assert_eq!(story.as_deref(), Some("2. Story Only"));
        assert!(task.is_none());
    }

    #[test]
    fn test_extract_metadata_hints_stops_at_section_heading() {
        // Story line appears after a ## heading — should not be captured.
        let content = "## Description\n\nStory: Hidden\n";
        let (story, task) = extract_metadata_hints(content);
        assert!(story.is_none());
        assert!(task.is_none());
    }

    #[test]
    fn test_extract_metadata_hints_empty_content() {
        let (story, task) = extract_metadata_hints("");
        assert!(story.is_none());
        assert!(task.is_none());
    }

    #[test]
    fn test_create_malformed_task_with_hints() {
        let content = "Story: 3. Big Story\nTask: 3.2 Fix Me\n\nbad content\n";
        let fp = PathBuf::from("tasks/3.2-fix-me.md");
        let task = create_malformed_task(content, fp.clone(), "missing Status".to_string());
        assert_eq!(task.story_name, "3. Big Story");
        assert_eq!(task.name, "3.2 Fix Me");
        assert_eq!(task.file_path, fp);
        let err_info = task
            .parse_error
            .as_ref()
            .expect("parse_error should be set");
        assert_eq!(err_info.error_message, "missing Status");
        assert_eq!(err_info.raw_content, content);
        assert!(err_info.suggested_fix.is_none());
        assert!(!err_info.fix_in_progress);
        assert!(task.is_malformed());
    }

    #[test]
    fn test_create_malformed_task_no_hints_falls_back_to_file_stem() {
        let content = "completely unparseable garbage";
        let fp = PathBuf::from("tasks/7.3-my-task.md");
        let task = create_malformed_task(content, fp, "missing Story".to_string());
        assert_eq!(task.story_name, "Unknown Story");
        assert_eq!(task.name, "7.3-my-task");
    }

    #[test]
    fn test_create_malformed_task_is_malformed() {
        let content = "no metadata at all";
        let fp = PathBuf::from("tasks/bad.md");
        let task = create_malformed_task(content, fp, "parse error".to_string());
        assert!(task.is_malformed());
        assert_eq!(
            task.parse_error,
            Some(ParseErrorInfo {
                error_message: "parse error".to_string(),
                raw_content: content.to_string(),
                suggested_fix: None,
                fix_in_progress: false,
            })
        );
    }

    #[test]
    fn test_parse_work_log_no_timezone() {
        let content = "\
Story: S
Task: T
Status: OPEN

## Description

x

## Work Log

1 2026-02-10T10:00:01 [Design Agent] some work
";
        let task = parse_task(content, path("t")).unwrap();
        assert_eq!(task.work_log.len(), 1);
        let entry = &task.work_log[0];
        assert_eq!(entry.sequence, 1);
        assert_eq!(entry.agent, AgentKind::Design);
        assert_eq!(entry.timestamp.to_rfc3339(), "2026-02-10T10:00:01+00:00");
        assert_eq!(entry.description, "some work");
    }

    #[test]
    fn test_parse_work_log_with_timezone() {
        let content = "\
Story: S
Task: T
Status: OPEN

## Description

x

## Work Log

1 2026-02-10T10:00:01Z [Design Agent] some work
";
        let task = parse_task(content, path("t")).unwrap();
        assert_eq!(task.work_log.len(), 1);
        let entry = &task.work_log[0];
        assert_eq!(entry.timestamp.to_rfc3339(), "2026-02-10T10:00:01+00:00");
    }

    #[test]
    fn test_parse_multiple_questions() {
        let content = "\
Story: S
Task: T
Status: OPEN

## Description

x

## Questions

Q1 [Intake Agent]: First question?
A1: First answer.

Q2 [Design Agent]: Second question?
";
        let task = parse_task(content, path("t")).unwrap();
        assert_eq!(task.questions.len(), 2);
        assert_eq!(task.questions[0].agent, AgentKind::Intake);
        assert_eq!(task.questions[0].text, "First question?");
        assert_eq!(task.questions[0].answer, Some("First answer.".to_string()));
        assert_eq!(task.questions[1].agent, AgentKind::Design);
        assert_eq!(task.questions[1].text, "Second question?");
        assert_eq!(task.questions[1].answer, None);
    }
}
