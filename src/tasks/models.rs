//! Data models for stories, tasks, questions, and work log entries.
//!
//! These structs mirror the structure of task markdown files. Each `Task` maps
//! to one markdown file on disk. `Story` groups related tasks by name.

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::error::ClawdMuxError;
use crate::workflow::agents::AgentKind;

/// Unique identifier for a task, derived from its file path.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[allow(dead_code)]
pub struct TaskId(PathBuf);

#[allow(dead_code)]
impl TaskId {
    /// Creates a `TaskId` from a file path.
    pub fn from_path(p: impl Into<PathBuf>) -> Self {
        TaskId(p.into())
    }

    /// Returns the underlying path.
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stem = self.0.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        write!(f, "{stem}")
    }
}

/// The lifecycle status of a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// Task is available to be started.
    Open,
    /// Task is currently being worked on by an agent.
    InProgress,
    /// Task is complete and awaiting human code review.
    PendingReview,
    /// Task has been reviewed and accepted.
    Completed,
    /// Task was cancelled and will not be completed.
    Abandoned,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            TaskStatus::Open => "OPEN",
            TaskStatus::InProgress => "IN_PROGRESS",
            TaskStatus::PendingReview => "PENDING_REVIEW",
            TaskStatus::Completed => "COMPLETED",
            TaskStatus::Abandoned => "ABANDONED",
        };
        write!(f, "{s}")
    }
}

impl FromStr for TaskStatus {
    type Err = ClawdMuxError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Strip underscores for lenient parsing (e.g. "IN_PROGRESS" == "INPROGRESS").
        let normalized = s.to_uppercase().replace('_', "");
        match normalized.as_str() {
            "OPEN" => Ok(TaskStatus::Open),
            "INPROGRESS" => Ok(TaskStatus::InProgress),
            "PENDINGREVIEW" => Ok(TaskStatus::PendingReview),
            "COMPLETED" => Ok(TaskStatus::Completed),
            "ABANDONED" => Ok(TaskStatus::Abandoned),
            other => Err(ClawdMuxError::Parse {
                file: "<task status>".to_string(),
                message: format!("unknown task status: '{other}'"),
            }),
        }
    }
}

/// A question posed by an agent, with an optional human-provided answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct Question {
    /// The pipeline agent that asked this question.
    pub agent: AgentKind,
    /// The question text.
    pub text: String,
    /// The human-provided answer, if one has been given.
    pub answer: Option<String>,
}

/// A single entry in the task's work log.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct WorkLogEntry {
    /// Sequence number of this entry (1-based).
    pub sequence: u32,
    /// When this entry was recorded (UTC).
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// The pipeline agent that produced this entry.
    pub agent: AgentKind,
    /// A short description of the work performed.
    pub description: String,
}

/// A single task loaded from a markdown file.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub struct Task {
    /// Unique identifier derived from the task file path.
    pub id: TaskId,
    /// The name of the story this task belongs to.
    pub story_name: String,
    /// The short name or number of this task (e.g., `"1.3"`).
    pub name: String,
    /// Current lifecycle status.
    pub status: TaskStatus,
    /// The agent currently assigned to this task, if any.
    pub assigned_to: Option<AgentKind>,
    /// Full description of what needs to be done.
    pub description: String,
    /// Optional initial prompt to seed the Intake agent.
    pub starting_prompt: Option<String>,
    /// Questions raised by agents and their human answers.
    pub questions: Vec<Question>,
    /// Design notes accumulated by the Design agent.
    pub design: Option<String>,
    /// Implementation plan accumulated by the Planning agent.
    pub implementation_plan: Option<String>,
    /// Chronological log of agent work performed on this task.
    pub work_log: Vec<WorkLogEntry>,
    /// Path to the markdown file this task was loaded from.
    pub file_path: PathBuf,
    /// Sections not recognized by the parser, preserved verbatim for round-trip fidelity.
    ///
    /// Each entry is `(heading_text, body_content)` where `heading_text` excludes the `## ` prefix.
    pub extra_sections: Vec<(String, String)>,
}

#[allow(dead_code)]
impl Task {
    /// Returns `true` if this task is currently being worked on by an agent.
    pub fn is_active(&self) -> bool {
        self.status == TaskStatus::InProgress
    }
}

/// A story groups related tasks under a common name.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Story {
    /// The name of this story (e.g., `"1. Project Skeleton"`).
    pub name: String,
    /// All tasks belonging to this story.
    pub tasks: Vec<Task>,
}

/// Parses a task name like `"1.10"` into `(1u32, 10u32)` for numeric ordering.
/// Returns `None` if the name does not match the `<story>.<task>` pattern.
fn parse_task_name_numeric(name: &str) -> Option<(u32, u32)> {
    let mut parts = name.splitn(2, '.');
    let story: u32 = parts.next()?.parse().ok()?;
    let task: u32 = parts.next()?.parse().ok()?;
    Some((story, task))
}

#[allow(dead_code)]
impl Story {
    /// Returns references to all tasks sorted by task number.
    ///
    /// Names matching the `<story>.<task>` numeric format (e.g., `"1.9"`, `"1.10"`) are
    /// compared numerically so that `"1.9"` sorts before `"1.10"`. Names that do not
    /// match the format fall back to lexicographic ordering.
    pub fn sorted_tasks(&self) -> Vec<&Task> {
        let mut sorted: Vec<&Task> = self.tasks.iter().collect();
        sorted.sort_by(|a, b| {
            match (
                parse_task_name_numeric(&a.name),
                parse_task_name_numeric(&b.name),
            ) {
                (Some(na), Some(nb)) => na.cmp(&nb),
                _ => a.name.cmp(&b.name),
            }
        });
        sorted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_task(name: &str, story_name: &str) -> Task {
        Task {
            id: TaskId::from_path(format!("tasks/{name}.md")),
            story_name: story_name.to_string(),
            name: name.to_string(),
            status: TaskStatus::Open,
            assigned_to: None,
            description: String::new(),
            starting_prompt: None,
            questions: Vec::new(),
            design: None,
            implementation_plan: None,
            work_log: Vec::new(),
            file_path: PathBuf::from(format!("tasks/{name}.md")),
            extra_sections: Vec::new(),
        }
    }

    #[test]
    fn test_task_status_display() {
        assert_eq!(TaskStatus::Open.to_string(), "OPEN");
        assert_eq!(TaskStatus::InProgress.to_string(), "IN_PROGRESS");
        assert_eq!(TaskStatus::PendingReview.to_string(), "PENDING_REVIEW");
        assert_eq!(TaskStatus::Completed.to_string(), "COMPLETED");
        assert_eq!(TaskStatus::Abandoned.to_string(), "ABANDONED");
    }

    #[test]
    fn test_task_status_from_str() {
        assert_eq!("open".parse::<TaskStatus>().unwrap(), TaskStatus::Open);

        // Underscore-separated (canonical) format.
        assert_eq!(
            "IN_PROGRESS".parse::<TaskStatus>().unwrap(),
            TaskStatus::InProgress
        );
        assert_eq!(
            "PENDING_REVIEW".parse::<TaskStatus>().unwrap(),
            TaskStatus::PendingReview
        );

        // Legacy no-separator format is also accepted.
        assert_eq!(
            "inprogress".parse::<TaskStatus>().unwrap(),
            TaskStatus::InProgress
        );
        assert_eq!(
            "PENDINGREVIEW".parse::<TaskStatus>().unwrap(),
            TaskStatus::PendingReview
        );

        assert_eq!(
            "Completed".parse::<TaskStatus>().unwrap(),
            TaskStatus::Completed
        );
        assert_eq!(
            "ABANDONED".parse::<TaskStatus>().unwrap(),
            TaskStatus::Abandoned
        );

        let err = "BOGUS".parse::<TaskStatus>().unwrap_err();
        assert!(matches!(&err, ClawdMuxError::Parse { file, .. } if file == "<task status>"));
    }

    #[test]
    fn test_task_id_from_path() {
        let id = TaskId::from_path("tasks/1.1-first.md");
        assert_eq!(id.to_string(), "1.1-first");
    }

    #[test]
    fn test_story_sorted_tasks() {
        let story = Story {
            name: "1. Test Story".to_string(),
            tasks: vec![
                make_test_task("1.2", "1. Test Story"),
                make_test_task("1.1", "1. Test Story"),
            ],
        };
        let sorted = story.sorted_tasks();
        assert_eq!(sorted[0].name, "1.1");
        assert_eq!(sorted[1].name, "1.2");
    }

    #[test]
    fn test_story_sorted_tasks_double_digit() {
        let story = Story {
            name: "1. Test Story".to_string(),
            tasks: vec![
                make_test_task("1.10", "1. Test Story"),
                make_test_task("1.9", "1. Test Story"),
                make_test_task("1.2", "1. Test Story"),
            ],
        };
        let sorted = story.sorted_tasks();
        assert_eq!(sorted[0].name, "1.2");
        assert_eq!(sorted[1].name, "1.9");
        assert_eq!(sorted[2].name, "1.10");
    }

    #[test]
    fn test_task_is_active() {
        let mut task = make_test_task("1.1", "Story");

        task.status = TaskStatus::InProgress;
        assert!(task.is_active());

        task.status = TaskStatus::Open;
        assert!(!task.is_active());

        task.status = TaskStatus::Completed;
        assert!(!task.is_active());

        task.status = TaskStatus::PendingReview;
        assert!(!task.is_active());

        task.status = TaskStatus::Abandoned;
        assert!(!task.is_active());
    }
}
