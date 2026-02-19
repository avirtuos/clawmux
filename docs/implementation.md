# ClawdMux Implementation Plan

## Overview

This document organizes ClawdMux development into 10 stories and 26 tasks. Each task is scoped to approximately 2 hours of AI-agent execution time, with clear objectives, file lists, implementation targets, and test requirements. Tasks follow the MVP phasing defined in `docs/design.md` (Phase 1 → Phase 2 → Phase 3 → Phase 4).

**Before starting any task:**
1. Read `docs/requirements.md` in full.
2. Read `docs/design.md` in full.
3. Read the parent story's objective in this document.
4. Read every file listed under the task's "Files to Read" section.

**After completing any task, run the full verification suite:**
```sh
cargo fmt && cargo build && cargo test && cargo clippy -- -D warnings
```
All four commands must pass with zero errors and zero warnings before the task is marked complete. Update the task status to COMPLETED and update `README.md` and `docs/design.md` with any relevant changes as required by CLAUDE.md.

---

## Dependency Graph

Tasks that must complete before a given task can begin:

```
1.1 (scaffold)
 └─> 1.2 (errors)
      └─> 1.3 (models)
           └─> 1.4 (messages + AgentKind)
                ├─> 2.1 (parser) ──> 2.2 (writer) ──> 2.3 (TaskStore)
                │                                         ├─> 3.1 (config loading)
                │                                         │        └─> 3.2 (init cmd)
                │                                         └─> 4.1 (TUI bootstrap)
                │                                                  └─> 4.2 (task list)
                │                                                           └─> 4.3 (Tab 1)
                ├─> 5.1 (API types) ──> 5.2 (HTTP client)
                │                              ├─> 5.3 (SSE events)
                │                              └─> 5.4 (server lifecycle)
                └─> 6.1 (workflow SM) [needs 1.4 + 5.2]
                         └─> 6.2 (prompt composer) [needs 1.3]
                                   ├─> 7.1 (Tab 2 activity)  [needs 4.1 + 5.3]
                                   ├─> 7.2 (Tab 3 team)      [needs 4.1 + 6.1]
                                   └─> 8.1 (msg dispatcher)  [needs 4.3 + 5.4 + 6.2]
                                              └─> 8.2 (file watcher) [needs 2.3]
                                                           └─> 9.1 (Tab 4 review)
                                                                    └─> 10.1 (E2E wiring)
                                                                              └─> 10.2 (resilience)
                                                                                        └─> 10.3 (polish)
```

## Recommended Execution Order (single agent, sequential)

1.1 → 1.2 → 1.3 → 1.4 → 2.1 → 2.2 → 2.3 → 3.1 → 3.2 → 5.1 → 5.2 → 5.3 → 5.4 → 4.1 → 4.2 → 4.3 → 6.1 → 6.2 → 7.1 → 7.2 → 8.1 → 8.2 → 9.1 → 10.1 → 10.2 → 10.3

---

## Story 1: Project Skeleton & Core Types

**Objective**: Establish the Rust module structure, centralized error type, and all foundational data models that every other story builds upon. At the end of this story, the project compiles cleanly with all module files in place and all core types defined, even if their implementations are skeletal.

**Estimated effort**: ~7 hours (4 tasks)

---

### Task 1.1 — Module Scaffold

**Status**: COMPLETED

**Objective**: Create the full directory and file skeleton for every module defined in the design doc so that subsequent tasks can fill them in without restructuring. All files contain the module declaration, a top-level doc comment, and placeholder `todo!()` items where needed.

**Files to Read**:
- `docs/design.md` — "Crate & Module Structure" section
- `Cargo.toml`

**Files to Create**:
- `src/main.rs`
- `src/app.rs`
- `src/error.rs`
- `src/messages.rs`
- `src/workflow/mod.rs`
- `src/workflow/agents.rs`
- `src/workflow/transitions.rs`
- `src/workflow/prompt_composer.rs`
- `src/tasks/mod.rs`
- `src/tasks/models.rs`
- `src/tasks/parser.rs`
- `src/tasks/writer.rs`
- `src/opencode/mod.rs`
- `src/opencode/types.rs`
- `src/opencode/session.rs`
- `src/opencode/events.rs`
- `src/opencode/server.rs`
- `src/tui/mod.rs`
- `src/tui/layout.rs`
- `src/tui/task_list.rs`
- `src/tui/tabs/mod.rs`
- `src/tui/tabs/task_details.rs`
- `src/tui/tabs/agent_activity.rs`
- `src/tui/tabs/team_status.rs`
- `src/tui/tabs/code_review.rs`
- `src/config/mod.rs`
- `src/config/init.rs`
- `src/config/providers.rs`

**Implementation Details**:
- `src/main.rs`: Add a `fn main()` stub that prints "ClawdMux starting" using `tracing::info!` and returns `Ok(())`. Use `clap` to parse a `Cli` struct with a `command: Option<Commands>` field and a `Commands::Init` variant. Do not implement logic yet.
- Each module file: Add `//! Module doc comment` at the top, declare `pub mod` children where needed, and add `#[allow(dead_code)]` where placeholder types will trigger warnings until used.
- `src/workflow/mod.rs`: Re-export from `agents`, `transitions`, `prompt_composer`.
- `src/tasks/mod.rs`: Re-export from `models`, `parser`, `writer`.
- `src/opencode/mod.rs`: Re-export from `types`, `session`, `events`, `server`.
- `src/tui/mod.rs`: Re-export from `layout`, `task_list`, `tabs`.
- `src/config/mod.rs`: Re-export from `init`, `providers`.

**Tests to Write**:
- One `#[test]` in `src/main.rs` (or `src/lib.rs` if you choose to add one) that asserts `true` as a compile-time sanity check that all modules are accessible.

**Verification**: Run the standard verification suite. The binary must compile. There must be no `mod` resolution errors.

---

### Task 1.2 — Centralized Error Type

**Status**: COMPLETED

**Objective**: Implement `ClawdMuxError`, a single `thiserror`-derived error enum that covers every error category the application will encounter. This becomes the `Err` side of every `Result` in the codebase.

**Files to Read**:
- `src/error.rs` (created in 1.1)
- `docs/design.md` — "Risk Assessment" section (lists error categories)
- `Cargo.toml` (confirm `thiserror` is listed)

**Files to Modify**:
- `src/error.rs`

**Implementation Details**:

```rust
/// Top-level error type for ClawdMux.
#[derive(Debug, thiserror::Error)]
pub enum ClawdMuxError {
    /// I/O errors (file reading, writing, etc.).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Task file parse errors.
    #[error("Parse error in {file}: {message}")]
    Parse { file: String, message: String },

    /// Task file serialization errors.
    #[error("Serialization error: {0}")]
    Serialize(String),

    /// TOML config parse errors.
    #[error("Config error: {0}")]
    Config(#[from] toml::de::Error),

    /// HTTP / opencode API errors.
    #[error("OpenCode API error: {0}")]
    Api(String),

    /// Reqwest HTTP transport errors.
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    /// SSE stream errors.
    #[error("SSE error: {0}")]
    Sse(String),

    /// opencode server spawn / lifecycle errors.
    #[error("Server error: {0}")]
    Server(String),

    /// Workflow state machine violations.
    #[error("Workflow error: {0}")]
    Workflow(String),

    /// JSON (de)serialization errors.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// General internal errors.
    #[error("Internal error: {0}")]
    Internal(String),
}

/// Convenience alias used throughout the codebase.
pub type Result<T> = std::result::Result<T, ClawdMuxError>;
```

**Tests to Write**:
- `test_io_error_from_conversion`: Create a `ClawdMuxError` from a `std::io::Error` via `From`, assert it is `ClawdMuxError::Io`.
- `test_json_error_from_conversion`: Parse invalid JSON, convert the `serde_json::Error` via `From`, assert `ClawdMuxError::Json`.
- `test_parse_error_display`: Assert that `ClawdMuxError::Parse { file: "foo.md".into(), message: "bad".into() }.to_string()` contains both "foo.md" and "bad".
- `test_result_alias`: Write a function `fn dummy() -> crate::error::Result<u32> { Ok(1) }` and assert it returns `Ok(1)`.

**Verification**: Standard suite.

---

### Task 1.3 — Core Task Models

**Status**: COMPLETED

**Objective**: Implement all data model structs and enums in `src/tasks/models.rs` that represent stories, tasks, questions, work log entries, and their statuses. These are plain data structures with no I/O logic.

**Files to Read**:
- `src/tasks/models.rs` (created in 1.1)
- `src/error.rs` (Task 1.2)
- `docs/design.md` — "Key Data Structures" section
- `docs/requirements.md` — "Sample Task File" section

**Files to Modify**:
- `src/tasks/models.rs`

**Implementation Details**:

Implement all types exactly as specified in `docs/design.md` under "Key Data Structures", with these additions:

- `TaskId`: Derive `Debug`, `Clone`, `PartialEq`, `Eq`, `Hash`. Add `impl TaskId { pub fn from_path(p: impl Into<PathBuf>) -> Self }` and `impl Display for TaskId` (shows the file name stem).
- `TaskStatus`: Derive `Debug`, `Clone`, `PartialEq`, `Eq`. Implement `Display` (e.g., `"OPEN"`, `"IN_PROGRESS"`, `"PENDING_REVIEW"`, `"COMPLETED"`, `"ABANDONED"`). Implement `FromStr` mapping these strings case-insensitively; underscores are stripped before matching so `"INPROGRESS"` and `"IN_PROGRESS"` both parse correctly.
- `Story`: Derive `Debug`, `Clone`. Add `impl Story { pub fn sorted_tasks(&self) -> Vec<&Task> }` returning tasks sorted numerically by `task.name` (e.g., `"1.9"` before `"1.10"`).
- `Task`: Derive `Debug`, `Clone`. Add `impl Task { pub fn is_active(&self) -> bool }` returning true when status is `InProgress`.
- `Question`: Derive `Debug`, `Clone`. The `agent` field is `String` at this stage (will be `AgentKind` after Task 1.4 — use a `String` here and convert later, or use `AgentKind` if you complete 1.4 first).
- `WorkLogEntry`: Derive `Debug`, `Clone`. The `agent` field is `String` at this stage for the same reason.

**Tests to Write**:
- `test_task_status_display`: Assert `TaskStatus::InProgress.to_string() == "IN_PROGRESS"` and `TaskStatus::PendingReview.to_string() == "PENDING_REVIEW"`.
- `test_task_status_from_str`: Assert both `"IN_PROGRESS"` and `"inprogress"` parse to `TaskStatus::InProgress`. Test all 5 variants. Assert that an invalid string produces `ClawdMuxError::Parse { file: "<task status>", .. }`.
- `test_task_id_from_path`: Create a `TaskId` from `PathBuf::from("tasks/1.1-first.md")`, assert `Display` output is `"1.1-first"`.
- `test_story_sorted_tasks`: Build a `Story` with two tasks (names "1.2" and "1.1"), assert `sorted_tasks()` returns them in order "1.1" then "1.2".
- `test_story_sorted_tasks_double_digit`: Build a `Story` with tasks "1.10", "1.9", "1.2" and assert they sort as "1.2", "1.9", "1.10".
- `test_task_is_active`: Assert `is_active()` is true for `InProgress`, false for `Open`.

**Verification**: Standard suite.

---

### Task 1.4 — AgentKind Enum & AppMessage Bus

**Status**: COMPLETED

**Objective**: Implement `AgentKind` in `src/workflow/agents.rs` and the `AppMessage` enum in `src/messages.rs`. These are the central coordination types that all subsystems speak.

**Files to Read**:
- `src/workflow/agents.rs` (created in 1.1)
- `src/messages.rs` (created in 1.1)
- `src/tasks/models.rs` (Task 1.3)
- `src/error.rs` (Task 1.2)
- `docs/design.md` — "Key Data Structures", "Agent Workflow Engine", and "AppMessage" sections

**Files to Modify**:
- `src/workflow/agents.rs`
- `src/messages.rs`
- `src/tasks/models.rs` — update `Question.agent` and `WorkLogEntry.agent` fields from `String` to `AgentKind`

**Implementation Details**:

**`src/workflow/agents.rs`**:

```rust
/// The 7 sequential pipeline agents.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentKind {
    Intake,
    Design,
    Planning,
    Implementation,
    CodeQuality,
    SecurityReview,
    CodeReview,
}
```

Implement:
- `Display for AgentKind` — returns the opencode agent name string (e.g., `"clawdmux/intake"`, `"clawdmux/code-quality"`).
- `AgentKind::pipeline_index(&self) -> usize` — 0 through 6.
- `AgentKind::next(&self) -> Option<AgentKind>` — advances pipeline, returns `None` for `CodeReview`.
- `AgentKind::prev(&self) -> Option<AgentKind>` — reverse, returns `None` for `Intake`.
- `AgentKind::valid_kickback_targets(&self) -> &[AgentKind]` — exactly as shown in design doc.
- `AgentKind::opencode_agent_name(&self) -> &str` — returns the agent slug (e.g., `"clawdmux/intake"`).
- `AgentKind::all() -> &'static [AgentKind]` — returns slice of all 7 in pipeline order.
- `FromStr for AgentKind` — parses the opencode agent name strings (case-insensitive).

**`src/messages.rs`**: Implement the full `AppMessage` enum exactly as shown in the design doc. Use `TaskId` and `AgentKind` from their respective modules. Add `#[derive(Debug)]`.

**Tests to Write** (in `src/workflow/agents.rs`):
- `test_pipeline_order`: Assert `AgentKind::Intake.pipeline_index() == 0` and `AgentKind::CodeReview.pipeline_index() == 6`.
- `test_next_chain`: Walk the chain via `next()` from `Intake`, assert all 7 agents appear in order and `CodeReview.next()` is `None`.
- `test_prev_chain`: Walk in reverse from `CodeReview`, assert `Intake.prev()` is `None`.
- `test_valid_kickback_targets`: Assert `CodeQuality.valid_kickback_targets()` contains only `Implementation`; `SecurityReview` contains `Implementation` and `Design`; `CodeReview` contains `Implementation`, `Design`, `Planning`.
- `test_intake_no_kickback`: Assert `Intake.valid_kickback_targets().is_empty()`.
- `test_display`: Assert `AgentKind::CodeQuality.to_string() == "clawdmux/code-quality"`.
- `test_from_str`: Parse `"clawdmux/code-quality"` and assert it equals `AgentKind::CodeQuality`.

**Verification**: Standard suite.

---

## Story 2: Task File Parsing & Writing

**Objective**: Implement the full round-trip: parse task markdown files into `Task` structs, serialize `Task` structs back to markdown, and manage the in-memory task cache with file discovery.

**Estimated effort**: ~6.5 hours (3 tasks)

---

### Task 2.1 — Task File Parser

**Status**: COMPLETED

**Objective**: Implement the two-phase line-oriented markdown parser that reads task files into `Task` structs, exactly as specified in the design doc.

**Files to Read**:
- `src/tasks/parser.rs` (created in 1.1)
- `src/tasks/models.rs` (Task 1.3)
- `src/error.rs` (Task 1.2)
- `docs/design.md` — "Task File Parsing" section
- `docs/requirements.md` — "Sample Task File" section (the canonical format)

**Files to Modify**:
- `src/tasks/parser.rs`

**Implementation Details**:

```rust
/// Parses a task markdown file into a [`Task`] struct.
///
/// Uses a two-phase approach:
/// - Phase 1: Parses `Key: Value` metadata lines before the first `##` heading.
/// - Phase 2: Splits on `##` headings; known sections are parsed structurally,
///   unknown sections are preserved verbatim for round-trip fidelity.
pub fn parse_task(content: &str, file_path: PathBuf) -> crate::error::Result<Task>
```

Known section headings (case-insensitive match): `## Description`, `## Starting Prompt`, `## Questions`, `## Design`, `## Implementation Plan`, `## Work Log`.

Unknown sections: Collect as `Vec<(String, String)>` and attach to `Task` as a new field `extra_sections: Vec<(String, String)>` (add this field to the `Task` struct in `models.rs`).

Question parsing within `## Questions`: Lines matching `Q<n> [<agent>]: <text>` followed by optional `A<n>: <text>`.

Work log parsing within `## Work Log`: Lines matching `<n> <ISO8601> [<agent>] <description>`. Use `chrono::DateTime::parse_from_rfc3339` for timestamp parsing.

Metadata parsing:
- `Story: <text>` → `task.story_name`
- `Task: <text>` → `task.name`
- `Status: <text>` → `task.status` (via `TaskStatus::from_str`)
- `Assigned To: [<agent>]` → `task.assigned_to` (via `AgentKind::from_str`, optional)

**Tests to Write** (with `#[cfg(test)]` module using `tempfile` or string literals):
- `test_parse_minimal`: Parse a file with only `Story:`, `Task:`, `Status:` metadata. Assert field values.
- `test_parse_full_sample`: Parse the exact sample file from `docs/requirements.md`. Assert all fields including Q&A and work log.
- `test_parse_assigned_to`: Parse `Assigned To: [Planning Agent]`. Assert `task.assigned_to == Some(AgentKind::Planning)`.
- `test_parse_unknown_section`: Parse a file with an extra `## Foo` section. Assert it appears in `task.extra_sections` with the correct content.
- `test_parse_multi_line_description`: Ensure multi-line description text is captured verbatim.
- `test_parse_empty_questions`: Parse a file where `## Questions` section is empty. Assert `task.questions` is empty.
- `test_parse_unanswered_question`: Parse a question with no `A<n>:` line. Assert `question.answer == None`.
- `test_parse_invalid_status`: Parse `Status: BOGUS`. Assert `ClawdMuxError::Parse` is returned.
- `test_parse_missing_story_field`: Parse a file with no `Story:` line. Assert `ClawdMuxError::Parse` is returned.

**Verification**: Standard suite.

---

### Task 2.2 — Task File Writer

**Status**: COMPLETED

**Objective**: Implement the serializer that writes a `Task` struct back to a markdown string, preserving unknown sections and producing output that round-trips cleanly through the parser. The round-trip must also preserve "unknown" sections but it is ok if unknown sections end up at the end of the document.

**Files to Read**:
- `src/tasks/writer.rs` (created in 1.1)
- `src/tasks/models.rs` (Task 1.3 — including `extra_sections` added in 2.1)
- `src/tasks/parser.rs` (Task 2.1)
- `docs/requirements.md` — "Sample Task File" (canonical format to match)

**Files to Modify**:
- `src/tasks/writer.rs`

**Implementation Details**:

```rust
/// Serializes a [`Task`] to a markdown string in the canonical task file format.
///
/// Unknown sections (from `task.extra_sections`) are written verbatim after
/// known sections to ensure round-trip fidelity.
pub fn write_task(task: &Task) -> crate::error::Result<String>
```

The output format must exactly match the sample task file structure:
```
Story: {story_name}
Task: {name}
Status: {status}
Assigned To: [{agent}]

## Description

{description}

## Starting Prompt

{starting_prompt}

## Questions

Q1 [{agent}]: {text}
A1: {answer}

## Design

{design}

## Implementation Plan

{implementation_plan}

## Work Log

{n} {timestamp} [{agent}] {description}
```

- Omit `Assigned To:` line if `task.assigned_to` is `None`.
- Omit `## Starting Prompt` section if `task.starting_prompt` is `None`.
- Omit `## Design` if `task.design` is `None`.
- Omit `## Implementation Plan` if `task.implementation_plan` is `None`.
- Omit `## Questions` and `## Work Log` if their vecs are empty.
- Write each `WorkLogEntry.timestamp` with `chrono::DateTime::to_rfc3339()`.
- Write each unknown section from `task.extra_sections` after the known sections.

**Tests to Write**:
- `test_write_minimal`: Write a task with only required fields. Assert output contains exactly the correct metadata lines and no empty section headings.
- `test_write_full`: Write a fully-populated task. Assert every section appears.
- `test_round_trip`: Parse the sample task file, write it back, parse again, assert the two `Task` structs are equal. This is the most important test.
- `test_round_trip_with_extra_section`: Parse a task with an unknown `## Foo` section, write it, assert `## Foo` is still present with its content.
- `test_omits_none_fields`: Write a task where `design` and `implementation_plan` are `None`. Assert neither `## Design` nor `## Implementation Plan` appears in output.
- `test_write_multiple_questions`: Write a task with 3 questions (some answered, some not). Assert correct Q/A numbering.

**Verification**: Standard suite.

---

### Task 2.3 — TaskStore

**Status**: COMPLETED

**Objective**: Implement `TaskStore` in `src/tasks/mod.rs`, providing in-memory task cache management, file discovery at startup, and the ability to load, update, and retrieve tasks and stories.

**Files to Read**:
- `src/tasks/mod.rs` (created in 1.1)
- `src/tasks/parser.rs` (Task 2.1)
- `src/tasks/writer.rs` (Task 2.2)
- `src/tasks/models.rs` (Task 1.3)
- `src/error.rs` (Task 1.2)
- `docs/design.md` — "Task File Parsing" section (file discovery paths)

**Files to Modify**:
- `src/tasks/mod.rs`

**Implementation Details**:

```rust
/// In-memory store for all loaded stories and tasks.
///
/// Discovers task files from `./tasks/` or `./docs/tasks/` on startup.
/// Caches parsed tasks and provides CRUD-style access by [`TaskId`].
pub struct TaskStore {
    tasks: HashMap<TaskId, Task>,
}

impl TaskStore {
    /// Creates an empty task store.
    pub fn new() -> Self

    /// Discovers and loads all `*.md` files from the project task directories.
    ///
    /// Scans `./tasks/` first, then `./docs/tasks/` if the first does not exist.
    /// Returns the number of tasks loaded.
    pub fn load_from_disk(&mut self, project_root: &Path) -> crate::error::Result<usize>

    /// Returns all stories, each with their tasks sorted by name.
    pub fn stories(&self) -> Vec<Story>

    /// Returns the task with the given ID, if present.
    pub fn get(&self, id: &TaskId) -> Option<&Task>

    /// Returns a mutable reference to the task with the given ID, if present.
    pub fn get_mut(&mut self, id: &TaskId) -> Option<&mut Task>

    /// Inserts or replaces a task in the store.
    pub fn insert(&mut self, task: Task)

    /// Writes the task back to its file on disk, then updates the store.
    pub fn persist(&mut self, id: &TaskId) -> crate::error::Result<()>

    /// Reloads a single task from disk, replacing the in-memory copy.
    pub fn reload(&mut self, id: &TaskId) -> crate::error::Result<()>

    /// Returns total number of tasks across all stories.
    pub fn task_count(&self) -> usize
}
```

- `stories()` returns tasks grouped by `story_name`, sorted by story name then task name.
- `load_from_disk` skips files that fail to parse (log a warning via `tracing::warn!`) but does not abort.

**Tests to Write** (using `tempfile::TempDir`):
- `test_load_from_disk_tasks_dir`: Write 2 task files to a temp `tasks/` dir, call `load_from_disk`, assert 2 tasks loaded.
- `test_load_from_disk_docs_tasks_fallback`: Only create `docs/tasks/` in the temp dir, assert tasks are still discovered.
- `test_stories_grouping`: Load 3 tasks (2 in story "1. Foo", 1 in story "2. Bar"), assert `stories()` returns 2 stories with correct task counts.
- `test_persist_roundtrip`: Load a task, mutate `task.status` to `Completed`, call `persist`, read the file from disk, parse it manually, assert status is `Completed`.
- `test_reload_reflects_disk_change`: Load a task, write a modified version to disk externally, call `reload`, assert the in-memory copy reflects the change.
- `test_get_missing_returns_none`: Assert `get(&nonexistent_id)` returns `None`.
- `test_task_count`: Assert `task_count()` matches the number of loaded tasks.
- `test_skips_unparseable_files`: Write one valid and one invalid task file, assert `load_from_disk` returns `Ok(1)` (loads 1, skips 1 with a warning).

**Verification**: Standard suite.

---

## Story 3: Configuration System

**Objective**: Implement the two-level configuration system (global `~/.config/clawdmux/config.toml` + project `.clawdmux/config.toml`) and the interactive `clawdmux init` command that bootstraps a new project.

**Estimated effort**: ~4.5 hours (2 tasks)

---

### Task 3.1 — Config Loading

**Objective**: Implement config loading and the `AppConfig` struct that merges global and project-level configuration.

**Files to Read**:
- `src/config/mod.rs` (created in 1.1)
- `src/config/providers.rs` (created in 1.1)
- `src/error.rs` (Task 1.2)
- `docs/design.md` — "Project Initialization", "Model Selection Hierarchy", and "OpenCode Server Lifecycle" sections

**Files to Modify**:
- `src/config/mod.rs`
- `src/config/providers.rs`

**Implementation Details**:

**`src/config/providers.rs`**:
```rust
/// Per-provider credentials and defaults.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct ProviderConfig {
    pub api_key: String,
    pub default_model: String,
}

/// Global ClawdMux configuration (~/.config/clawdmux/config.toml).
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct GlobalConfig {
    pub provider: ProviderSection,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct ProviderSection {
    pub default: String,
    pub anthropic: Option<ProviderConfig>,
    pub openai: Option<ProviderConfig>,
    pub google: Option<ProviderConfig>,
}
```

Implement `GlobalConfig::load(path: &Path) -> Result<Self>` (reads + parses TOML) and `GlobalConfig::save(path: &Path) -> Result<()>`.

Implement `GlobalConfig::env_vars_for_opencode(&self) -> Vec<(String, String)>` — returns the list of `(ENV_VAR_NAME, value)` pairs to inject when spawning the opencode server (e.g., `("ANTHROPIC_API_KEY", "sk-ant-...")` for the active provider).

**`src/config/mod.rs`**:
```rust
/// Project-level opencode connection config (.clawdmux/config.toml).
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct OpenCodeConfig {
    pub mode: ServerMode,     // "auto" | "external"
    pub hostname: String,     // default "127.0.0.1"
    pub port: u16,            // default 4096
    pub password: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode { Auto, External }

/// Full merged application config.
pub struct AppConfig {
    pub global: GlobalConfig,
    pub opencode: OpenCodeConfig,
}
```

Implement `AppConfig::load(project_root: &Path) -> Result<Self>` — loads global config from `~/.config/clawdmux/config.toml` (uses `dirs` crate or `std::env::var("HOME")`) and project config from `{project_root}/.clawdmux/config.toml`.

Add `dirs = "5"` to `Cargo.toml` `[dependencies]` if not already present.

**Tests to Write** (using `tempfile::TempDir`):
- `test_global_config_load`: Write a valid TOML file to a temp path, call `GlobalConfig::load`, assert field values match.
- `test_global_config_default`: Call `GlobalConfig::load` on a nonexistent path, assert it returns a `ClawdMuxError::Io`.
- `test_opencode_config_defaults`: Deserialize an empty TOML `[opencode]` table; use `#[serde(default)]` attributes to ensure hostname defaults to `"127.0.0.1"` and port to `4096`.
- `test_env_vars_anthropic`: Build a `GlobalConfig` with an Anthropic provider, call `env_vars_for_opencode`, assert `("ANTHROPIC_API_KEY", "sk-ant-...")` is in the result.
- `test_server_mode_serde`: Round-trip `ServerMode::Auto` through TOML, assert it deserializes correctly.

**Verification**: Standard suite.

---

### Task 3.2 — `clawdmux init` Command

**Objective**: Implement the interactive `clawdmux init` command that checks for opencode, sets up provider credentials, and scaffolds the project directory structure.

**Files to Read**:
- `src/config/init.rs` (created in 1.1)
- `src/config/mod.rs` (Task 3.1)
- `src/config/providers.rs` (Task 3.1)
- `src/error.rs` (Task 1.2)
- `src/main.rs` (Task 1.1 — contains CLI parsing)
- `docs/design.md` — "Project Initialization (`clawdmux init`)" section (all 4 steps)
- `Cargo.toml` (confirm `which`, `rpassword` are listed)

**Files to Modify**:
- `src/config/init.rs`
- `src/main.rs` — wire `Commands::Init` to call `run_init()`

**Implementation Details**:

```rust
/// Runs the interactive `clawdmux init` wizard.
///
/// Performs four steps:
/// 1. Checks for the opencode binary; offers to install it.
/// 2. Checks for provider credentials in the global config; prompts if missing.
/// 3. Scaffolds project-local files (.clawdmux/config.toml, .opencode/agents/).
/// 4. Prints a success summary.
pub fn run_init(project_root: &Path, args: &InitArgs) -> crate::error::Result<()>

/// Arguments for `clawdmux init`.
#[derive(clap::Args, Debug)]
pub struct InitArgs {
    /// Regenerate agent definition files from built-in defaults.
    #[arg(long)]
    pub reset_agents: bool,
}
```

**Step 1 — opencode binary check**: Use `which::which("opencode")`. If not found, print the message from the design doc and read a `y/N` from stdin. If confirmed, spawn `curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path` and wait for it. Verify by calling `opencode --version`.

**Step 2 — provider credentials**: Load global config with `GlobalConfig::load`. If `provider.default` is empty, walk the user through the provider selection prompt from the design doc. Use `rpassword::prompt_password` for the API key. Save with `GlobalConfig::save`.

**Step 3 — project scaffold**: Create the following if they do not already exist:
  - `.clawdmux/config.toml` with default `OpenCodeConfig`
  - `tasks/` directory
  - `.opencode/agents/clawdmux/` directory
  - One agent definition `.md` file per `AgentKind` (use `AgentKind::all()`)

Each agent definition file content: Use the template from the design doc for `implementation.md` as a reference; adapt tool permissions per the agent permissions table in the design doc. The `--reset-agents` flag overwrites these files even if they exist.

Print `"clawdmux is ready. Run clawdmux to open the TUI."` on success. Use `tracing::info!` for each created file.

**Tests to Write** (in `#[cfg(test)]` module, using `tempfile::TempDir`):
- `test_scaffold_creates_config`: Call `run_init` in a temp dir (with a pre-written global config so the credential prompt is skipped). Assert `.clawdmux/config.toml` exists.
- `test_scaffold_creates_tasks_dir`: Assert `tasks/` directory is created.
- `test_scaffold_creates_all_agent_files`: Assert all 7 agent `.md` files exist under `.opencode/agents/clawdmux/`.
- `test_scaffold_idempotent`: Call `run_init` twice. Assert no errors and files are not duplicated.
- `test_reset_agents_overwrites`: Modify an agent file, call `run_init --reset-agents`, assert the file is back to defaults.

**Verification**: Standard suite. Note: tests that invoke curl should be behind a `#[cfg(not(feature = "offline_tests"))]` gate or skip the network step by using a mock.

---

## Story 4: TUI Shell & Layout (Phase 1)

**Objective**: Build the TUI skeleton with ratatui: the event loop in `main.rs`, the two-pane layout, the task list widget in the left pane, and Tab 1 (task details). At the end of this story the TUI is runnable and shows real task data from disk.

**Estimated effort**: ~6.5 hours (3 tasks)

---

### Task 4.1 — TUI Bootstrap & Event Loop

**Objective**: Implement `main.rs` event loop, `App` state, and the `tui/mod.rs` draw/input dispatch scaffold. The application must initialize the terminal, render a placeholder frame, handle `q`/`Ctrl-C` to quit, and restore the terminal on exit.

**Files to Read**:
- `src/main.rs` (Task 1.1, Task 3.2 — CLI wiring)
- `src/app.rs` (created in 1.1)
- `src/tui/mod.rs` (created in 1.1)
- `src/tui/layout.rs` (created in 1.1)
- `src/messages.rs` (Task 1.4)
- `src/error.rs` (Task 1.2)
- `Cargo.toml` (confirm `ratatui`, `crossterm`, `tokio` versions)

**Files to Modify**:
- `src/main.rs`
- `src/app.rs`
- `src/tui/mod.rs`
- `src/tui/layout.rs`

**Implementation Details**:

**`src/app.rs`**:
```rust
/// Top-level application state.
pub struct App {
    pub task_store: TaskStore,
    pub selected_task: Option<TaskId>,
    pub active_tab: usize,
    pub should_quit: bool,
}

impl App {
    pub fn new(task_store: TaskStore) -> Self
    pub fn handle_message(&mut self, msg: AppMessage) -> Vec<AppMessage>
}
```

**`src/tui/mod.rs`**:
```rust
/// Draws the full TUI frame.
pub fn draw(frame: &mut ratatui::Frame, app: &App)

/// Converts a crossterm event into an optional AppMessage.
pub fn handle_input(event: crossterm::event::Event, app: &App) -> Option<AppMessage>
```

**`src/tui/layout.rs`**: Implement a `render_layout` function that splits the terminal into:
- A 3-row top header bar.
- A main area split 25% left / 75% right.
- A 2-row bottom footer bar.
Returns the three `Rect` values (header, left_pane, right_pane, footer) as a struct or tuple.

**`src/main.rs`**: Replace the stub with a real `tokio::main` async function that:
1. Parses CLI args. If `Commands::Init`, calls `run_init` and exits.
2. Calls `TaskStore::load_from_disk`.
3. Sets up `AppConfig`.
4. Initializes the crossterm backend and `ratatui::Terminal`.
5. Runs the event loop: polls `crossterm::event::EventStream` (async), sends events as `AppMessage::TerminalEvent` into an `mpsc::channel`, calls `draw` each tick.
6. On `should_quit`, restores the terminal with `crossterm::terminal::disable_raw_mode` and `LeaveAlternateScreen`.

Use `tracing_appender` to write logs to a file (e.g., `clawdmux.log`) since the TUI owns stdout.

**Tests to Write**:
- `test_app_new`: Assert `App::new` with an empty `TaskStore` initializes `selected_task` to `None`, `active_tab` to 0, `should_quit` to false.
- `test_handle_message_shutdown`: Call `App::handle_message(AppMessage::Shutdown)`, assert `app.should_quit` is true.
- `test_render_layout_proportions`: Call `render_layout` with a `Rect` of known size (80x24), assert left pane is approximately 25% wide and right pane approximately 75%.

**Verification**: Standard suite. Running `cargo run` should open a blank ratatui frame and exit cleanly on `q`.

---

### Task 4.2 — Task List Widget

**Objective**: Implement the left pane task list widget showing collapsible stories and selectable tasks with status indicators.

**Files to Read**:
- `src/tui/task_list.rs` (created in 1.1)
- `src/tui/layout.rs` (Task 4.1)
- `src/tui/mod.rs` (Task 4.1)
- `src/app.rs` (Task 4.1)
- `src/tasks/models.rs` (Task 1.3)
- `docs/design.md` — "TUI Layout" section

**Files to Modify**:
- `src/tui/task_list.rs`
- `src/tui/mod.rs` — call `task_list::render` from `draw()`
- `src/app.rs` — add `task_list_state: TaskListState` field

**Implementation Details**:

```rust
/// State for the task list navigation.
pub struct TaskListState {
    /// Which story indices are expanded (collapsed by default).
    pub expanded_stories: HashSet<String>,
    /// The currently highlighted item (story or task).
    pub selected_index: usize,
    /// Flattened list of selectable items for cursor navigation.
    items: Vec<TaskListItem>,
}

enum TaskListItem {
    Story { name: String },
    Task { task_id: TaskId, story_name: String },
}

impl TaskListState {
    pub fn new() -> Self
    /// Rebuilds the flat item list from current store + expanded state.
    pub fn refresh(&mut self, stories: &[Story])
    pub fn move_up(&mut self)
    pub fn move_down(&mut self)
    pub fn toggle_story(&mut self)
    pub fn selected_task_id(&self) -> Option<&TaskId>
}

/// Renders the task list into `area`.
pub fn render(frame: &mut Frame, area: Rect, state: &TaskListState, stories: &[Story])
```

Visual format (from design doc):
```
> 1. Big Story
  [*] 1.1 First
  [ ] 1.2 Second
> 2. Other Story
  [ ] 2.1 Task A
```

Status icons: `[*]` = InProgress, `[x]` = Completed, `[!]` = Abandoned, `[?]` = PendingReview, `[ ]` = Open.

Handle keyboard input in `tui/mod.rs` `handle_input`: `Up`/`k` → `move_up`, `Down`/`j` → `move_down`, `Enter`/`Space` → select task or toggle story, `Tab` → switch active tab.

**Tests to Write**:
- `test_task_list_state_refresh`: Build a `TaskListState` with 2 stories (3 tasks total), all expanded. Assert `items` count is 5 (2 story headers + 3 tasks).
- `test_task_list_collapsed_story`: Collapse a story. Assert only the story header appears in the flattened list for that story.
- `test_move_up_down_wraps`: Assert moving up from index 0 stays at 0; moving down from last stays at last.
- `test_toggle_story_expands_collapses`: Toggle a collapsed story, assert it is now expanded; toggle again, assert collapsed.
- `test_selected_task_id`: Navigate to a task item, assert `selected_task_id()` returns the correct `TaskId`.

**Verification**: Standard suite. `cargo run` should show the task list populated from any `*.md` files in `tasks/` (or `docs/tasks/`).

---

### Task 4.3 — Tab 1: Task Details

**Objective**: Implement the right pane tab bar and Tab 1, which displays the task markdown, a supplemental prompt input field, and the Q&A section.

**Files to Read**:
- `src/tui/tabs/mod.rs` (created in 1.1)
- `src/tui/tabs/task_details.rs` (created in 1.1)
- `src/app.rs` (Task 4.1, 4.2)
- `src/tasks/models.rs` (Task 1.3)
- `docs/design.md` — "TUI Layout" section (Tab 1 description)
- `Cargo.toml` (confirm `tui-textarea`)

**Files to Modify**:
- `src/tui/tabs/mod.rs`
- `src/tui/tabs/task_details.rs`
- `src/tui/mod.rs` — integrate tab rendering
- `src/app.rs` — add `tab1_state: Tab1State`

**Implementation Details**:

**`src/tui/tabs/mod.rs`**:
```rust
/// Renders the tab bar and dispatches to the active tab renderer.
pub fn render(frame: &mut Frame, area: Rect, app: &App)
```

Tab bar uses ratatui `Tabs` widget with 4 labels: `"Details"`, `"Agent Activity"`, `"Team Status"`, `"Review"`. Highlight the active tab.

**`src/tui/tabs/task_details.rs`**:
```rust
/// UI state for Tab 1.
pub struct Tab1State {
    /// Multi-line textarea for the supplemental prompt.
    pub prompt_input: tui_textarea::TextArea<'static>,
    /// Answer text areas (one per unanswered question).
    pub answer_inputs: Vec<tui_textarea::TextArea<'static>>,
    /// Which answer field is focused (None = prompt is focused).
    pub focused_answer: Option<usize>,
    /// Whether the prompt area is in insert mode.
    pub prompt_focused: bool,
}

/// Renders the task details tab.
pub fn render(frame: &mut Frame, area: Rect, task: Option<&Task>, state: &Tab1State)
```

Layout (top to bottom):
1. Task metadata block (story, name, status, assigned_to) — read-only.
2. Task description — scrollable paragraph.
3. `── Supplemental Prompt ──` label + `tui-textarea` for the prompt.
4. `── Questions ──` section — each unanswered question rendered with its own `tui-textarea` for the answer; answered questions shown as read-only text.

When no task is selected, render a centered `"Select a task from the list"` placeholder.

**Tests to Write**:
- `test_tab1_state_new`: Assert `Tab1State::new()` creates an empty `prompt_input` with no focused answer.
- `test_tab_bar_renders_four_tabs`: Smoke-test: create a `ratatui::backend::TestBackend`, call `tabs::render` with `active_tab = 0`, assert the rendered buffer contains the text `"Details"`.
- `test_task_details_no_task`: Render with `task: None`, assert buffer contains `"Select a task"`.
- `test_task_details_shows_description`: Render with a task whose description is `"Hello world"`, assert buffer contains `"Hello world"`.

**Verification**: Standard suite. `cargo run` should show Tab 1 with task content when a task is selected.

---

## Story 5: OpenCode Client (Phase 2)

**Objective**: Implement the full HTTP client for the opencode API, the SSE event stream consumer, and the server lifecycle manager. At the end of this story, ClawdMux can spawn an opencode server, connect to it, send prompts, and receive streaming events.

**Estimated effort**: ~8.5 hours (4 tasks)

---

### Task 5.1 — OpenCode API Types

**Objective**: Implement all Rust types in `src/opencode/types.rs` that mirror the opencode OpenAPI schema, plus `FileDiff` types used by the code review flow.

**Files to Read**:
- `src/opencode/types.rs` (created in 1.1)
- `src/error.rs` (Task 1.2)
- `docs/design.md` — "Key Data Structures" (OpenCode types) and "Key API Endpoints" sections

**Files to Modify**:
- `src/opencode/types.rs`

**Implementation Details**:

Implement all types from the design doc exactly as specified: `OpenCodeSession`, `MessagePart`, `OpenCodeMessage`, `MessageRole`, `OpenCodeEvent`, `FileDiff`, `DiffStatus`, `DiffHunk`, `DiffLine`.

Additional types needed:
```rust
/// Request body for POST /session/:id/message
#[derive(Debug, serde::Serialize)]
pub struct SendMessageRequest {
    pub content: Vec<ContentPart>,
    pub model_id: Option<String>,
    pub provider_id: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub kind: String,   // "text"
    pub text: String,
}

/// Response from POST /session
#[derive(Debug, serde::Deserialize)]
pub struct CreateSessionResponse {
    pub id: String,
    pub created: chrono::DateTime<chrono::Utc>,
}

/// Response from GET /global/health
#[derive(Debug, serde::Deserialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub version: Option<String>,
}
```

All types: derive `Debug`, `Clone`. Serialization types derive `serde::Serialize`. Deserialization types derive `serde::Deserialize`. Use `#[serde(rename_all = "camelCase")]` where appropriate to match opencode's JSON naming.

**Tests to Write**:
- `test_message_part_text_serde`: Round-trip `MessagePart::Text { text: "hello".into() }` through JSON, assert equality.
- `test_create_session_response_deserialize`: Parse a JSON string matching opencode's response shape, assert `id` field.
- `test_health_response_ok`: Parse `{"ok": true}`, assert `health.ok == true`.
- `test_file_diff_status_serde`: Assert `DiffStatus::Added` serializes to `"added"` (or the exact string opencode uses — document the chosen string in a comment).
- `test_opencode_event_serde`: Build a `SessionCreated` event JSON (matching opencode's actual SSE format), deserialize to `OpenCodeEvent::SessionCreated`, assert `session_id` field.

**Verification**: Standard suite.

---

### Task 5.2 — HTTP Client

**Objective**: Implement `OpenCodeClient` in `src/opencode/mod.rs` and `src/opencode/session.rs` with all methods for session management and prompt sending.

**Files to Read**:
- `src/opencode/mod.rs` (created in 1.1)
- `src/opencode/session.rs` (created in 1.1)
- `src/opencode/types.rs` (Task 5.1)
- `src/error.rs` (Task 1.2)
- `docs/design.md` — "OpenCode Client Layer" section (full method signatures)

**Files to Modify**:
- `src/opencode/mod.rs`
- `src/opencode/session.rs`

**Implementation Details**:

Implement `OpenCodeClient` exactly as specified in the design doc:
```rust
pub struct OpenCodeClient {
    http: reqwest::Client,
    base_url: String,
    auth: Option<(String, String)>,
}
```

Methods (all `async`, return `crate::error::Result<_>`):
- `new(base_url: String, auth: Option<(String, String)>) -> Self`
- `create_session(&self) -> Result<OpenCodeSession>`
  - `POST /session`, body `{}`, deserialize to `CreateSessionResponse`
- `send_prompt_async(&self, session_id: &str, agent: &AgentKind, prompt: &str) -> Result<()>`
  - `POST /session/{id}/message` with `SendMessageRequest`; agent maps to `model_id` from agent definition
- `abort_session(&self, session_id: &str) -> Result<()>`
  - `DELETE /session/{id}`
- `fork_session(&self, session_id: &str) -> Result<OpenCodeSession>`
  - `POST /session/{id}/fork` (or equivalent endpoint if different)
- `get_session_diffs(&self, session_id: &str) -> Result<Vec<FileDiff>>`
  - `GET /session/{id}/diff`
- `health(&self) -> Result<bool>`
  - `GET /global/health`, returns `response.ok`

Map non-2xx HTTP responses to `ClawdMuxError::Api` with the response body as the error message.

Add helper: `fn auth_header(&self) -> Option<reqwest::header::HeaderValue>` for basic auth.

**Tests to Write** (using `mockito::Server`):
- `test_create_session_success`: Mock `POST /session` returning `{"id":"abc","created":"..."}`. Call `create_session()`, assert `session.id == "abc"`.
- `test_create_session_server_error`: Mock `POST /session` returning 500. Assert `ClawdMuxError::Api` is returned.
- `test_health_true`: Mock `GET /global/health` returning `{"ok":true}`. Assert `health()` returns `Ok(true)`.
- `test_health_false`: Mock returning `{"ok":false}`. Assert `Ok(false)`.
- `test_abort_session`: Mock `DELETE /session/abc` returning 200. Call `abort_session("abc")`. Assert `Ok(())`.
- `test_get_session_diffs_empty`: Mock `GET /session/abc/diff` returning `[]`. Assert empty vec.
- `test_send_prompt_async`: Mock `POST /session/abc/message` returning 200. Assert `Ok(())`.

**Verification**: Standard suite.

---

### Task 5.3 — SSE Event Stream Consumer

**Objective**: Implement `EventStreamConsumer` in `src/opencode/events.rs` that connects to opencode's SSE stream and maps events to `AppMessage` values.

**Files to Read**:
- `src/opencode/events.rs` (created in 1.1)
- `src/opencode/types.rs` (Task 5.1)
- `src/messages.rs` (Task 1.4)
- `src/error.rs` (Task 1.2)
- `docs/design.md` — "OpenCode Client Layer" (`EventStreamConsumer`) and "Agent Workflow Engine" sections

**Files to Modify**:
- `src/opencode/events.rs`

**Implementation Details**:

```rust
pub struct EventStreamConsumer {
    tx: tokio::sync::mpsc::Sender<AppMessage>,
    session_map: std::sync::Arc<tokio::sync::RwLock<HashMap<String, (TaskId, AgentKind)>>>,
}

impl EventStreamConsumer {
    pub fn new(
        tx: tokio::sync::mpsc::Sender<AppMessage>,
        session_map: std::sync::Arc<tokio::sync::RwLock<HashMap<String, (TaskId, AgentKind)>>>,
    ) -> Self

    /// Connects to `GET {base_url}/global/event` and processes the SSE stream.
    /// Runs indefinitely; reconnects on connection loss with exponential backoff.
    /// Should be spawned as a `tokio::task::spawn`.
    pub async fn run(&self, base_url: String) -> crate::error::Result<()>
}
```

Event mapping (from `OpenCodeEvent` → `AppMessage`):
- `SessionCreated { session_id }` → `AppMessage::SessionCreated { task_id, session_id }` (look up `task_id` from `session_map`)
- `MessageUpdated { session_id, message_id, parts }` → `AppMessage::StreamingUpdate { task_id, session_id, parts }`
- `ToolExecuting { session_id, tool }` → `AppMessage::ToolActivity { task_id, session_id, tool, status: "executing".into() }`
- `ToolCompleted { session_id, tool, result }` → `AppMessage::ToolActivity { ... status: "completed".into() }`
- `SessionCompleted { session_id }` → `AppMessage::SessionCompleted { task_id, session_id }`
- `SessionError { session_id, error }` → `AppMessage::SessionError { task_id, session_id, error }`

Events whose `session_id` does not appear in `session_map` are silently ignored (log at `tracing::debug!`).

Use `reqwest_eventsource::EventSource` for the SSE connection. Reconnect on error with exponential backoff starting at 1 second, capped at 30 seconds.

**Tests to Write**:
- `test_event_routing_known_session`: Insert a session into `session_map`, push a `SessionCompleted` event through a mock SSE source, assert the `tx` channel receives `AppMessage::SessionCompleted` with the correct `task_id`.
- `test_event_routing_unknown_session`: Push an event for an unknown session, assert nothing is sent on `tx`.
- `test_session_map_insert_and_remove`: Verify `Arc<RwLock<HashMap>>` concurrent read/write correctness.

**Verification**: Standard suite.

---

### Task 5.4 — Server Lifecycle

**Objective**: Implement `src/opencode/server.rs` which manages spawning the opencode server as a child process, polling for health readiness, and graceful shutdown.

**Files to Read**:
- `src/opencode/server.rs` (created in 1.1)
- `src/opencode/mod.rs` (Task 5.2)
- `src/config/mod.rs` (Task 3.1)
- `src/error.rs` (Task 1.2)
- `docs/design.md` — "OpenCode Server Lifecycle" section

**Files to Modify**:
- `src/opencode/server.rs`
- `src/main.rs` — call server startup before entering TUI loop

**Implementation Details**:

```rust
/// Manages the opencode server child process lifecycle.
pub struct OpenCodeServer {
    child: Option<std::process::Child>,
    port: u16,
    hostname: String,
}

impl OpenCodeServer {
    /// Ensures an opencode server is reachable.
    ///
    /// In `auto` mode: spawns `opencode serve` if no server is already running.
    /// In `external` mode: only checks health, returns error if unreachable.
    pub async fn ensure_running(
        config: &OpenCodeConfig,
        env_vars: &[(String, String)],
    ) -> crate::error::Result<Self>

    /// Returns the base URL of the running server.
    pub fn base_url(&self) -> String

    /// Sends SIGTERM to the child process (if we spawned it) and waits up to 5s.
    pub async fn shutdown(&mut self) -> crate::error::Result<()>
}
```

`ensure_running` algorithm:
1. Try `GET /global/health` on the configured address. If it returns `ok: true`, return with `child: None` (external server).
2. If it fails and mode is `External`, return `ClawdMuxError::Server("opencode server not reachable")`.
3. In `Auto` mode, spawn `opencode serve --port {port} --hostname {hostname}` with `std::process::Command::new("opencode")`, injecting `env_vars` into the child's environment.
4. Poll health at 500ms intervals, up to 30 seconds. Use exponential backoff: 100ms, 200ms, 400ms, ... capped at 2000ms.
5. On timeout, kill the child and return `ClawdMuxError::Server("opencode server did not become healthy")`.

**Tests to Write** (where possible without a real opencode binary):
- `test_base_url_format`: Assert `base_url()` returns `"http://127.0.0.1:4096"` for default config.
- `test_external_mode_health_fail`: Mock the health endpoint to return 500; in external mode, assert `ClawdMuxError::Server` is returned (use `mockito`).
- `test_external_mode_health_ok`: Mock the health endpoint to return `{"ok":true}`; in external mode, assert `Ok(server)` and `server.child.is_none()`.

**Verification**: Standard suite.

---

## Story 6: Workflow Engine (Phase 3)

**Objective**: Implement the pure state machine that drives the 7-agent pipeline and the prompt composer that builds per-agent user messages from task context.

**Estimated effort**: ~4.5 hours (2 tasks)

---

### Task 6.1 — Workflow State Machine

**Objective**: Implement `WorkflowEngine` and the transition logic that advances the pipeline, handles kickbacks, pauses for questions, and emits `AppMessage` side effects.

**Files to Read**:
- `src/workflow/mod.rs` (created in 1.1)
- `src/workflow/agents.rs` (Task 1.4)
- `src/workflow/transitions.rs` (created in 1.1)
- `src/messages.rs` (Task 1.4)
- `src/tasks/models.rs` (Task 1.3)
- `docs/design.md` — "Agent Workflow Engine" section (full flow description)

**Files to Modify**:
- `src/workflow/mod.rs`
- `src/workflow/transitions.rs`

**Implementation Details**:

```rust
/// State of a single active task's workflow execution.
#[derive(Debug, Clone)]
pub struct WorkflowState {
    pub task_id: TaskId,
    pub current_agent: AgentKind,
    pub session_id: Option<String>,
    pub phase: WorkflowPhase,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowPhase {
    /// Waiting to start.
    Idle,
    /// Agent is actively working.
    Running,
    /// Paused waiting for a human answer.
    AwaitingAnswer { question_index: usize },
    /// Agent work done; awaiting human code review approval.
    PendingReview,
    /// Completed successfully.
    Completed,
    /// Permanently errored.
    Errored { reason: String },
}

/// The pure workflow state machine.
///
/// `process` takes the current state and an incoming message, returns the
/// new state plus a list of side-effect messages to dispatch.
pub struct WorkflowEngine {
    states: HashMap<TaskId, WorkflowState>,
}

impl WorkflowEngine {
    pub fn new() -> Self
    pub fn state(&self, task_id: &TaskId) -> Option<&WorkflowState>
    pub fn process(&mut self, msg: AppMessage) -> Vec<AppMessage>
}
```

Transition logic in `src/workflow/transitions.rs` — implement `process` as a large `match` on `(current_phase, msg)`:
- `StartTask { task_id }` → set `current_agent = Intake`, `phase = Running`, emit `CreateSession { task_id, agent: Intake, prompt: compose_user_message(...) }`.
- `SessionCreated { task_id, session_id }` → record `session_id` on state.
- `SessionCompleted { task_id, session_id }` → the session's final message should have been parsed already; this triggers advancement to the next agent (emit `CreateSession` for next agent) or transitions to `PendingReview`.
- `AgentCompleted { task_id, agent, summary }` → advance to `agent.next()`, emit `CreateSession` for next agent, or transition to `PendingReview` if `CodeReview` completed.
- `AgentKickedBack { task_id, from, to, reason }` → validate via `from.valid_kickback_targets()` (return `ClawdMuxError::Workflow` if invalid), set `current_agent = to`, emit `CreateSession`.
- `AgentAskedQuestion { task_id, agent, question }` → set `phase = AwaitingAnswer`, do NOT emit further messages (wait for human).
- `HumanAnswered { task_id, question_index, answer }` → set `phase = Running`, emit `CreateSession` to continue the current agent.
- `HumanApprovedReview { task_id }` → set `phase = Completed`.
- `HumanRequestedRevisions { task_id, comments }` → set `current_agent = CodeReview`, emit `CreateSession` with combined feedback.
- `SessionError { task_id, session_id, error }` → set `phase = Errored`.

**Tests to Write**:
- `test_start_task_transitions_to_running`: Call `process(StartTask { task_id })`, assert state is `Running` and one `CreateSession` message emitted for `Intake`.
- `test_agent_completed_advances_pipeline`: For each of the first 6 agents, assert `AgentCompleted` causes `CreateSession` for the next agent.
- `test_code_review_completed_transitions_to_pending_review`: Assert `AgentCompleted { agent: CodeReview }` transitions to `PendingReview`.
- `test_valid_kickback_accepted`: Assert `AgentKickedBack { from: CodeQuality, to: Implementation }` transitions `current_agent` to `Implementation` and emits `CreateSession`.
- `test_invalid_kickback_rejected`: Assert `AgentKickedBack { from: Intake, to: Implementation }` is rejected (returns `SessionError` or similar error message).
- `test_question_pauses_workflow`: Assert `AgentAskedQuestion` transitions to `AwaitingAnswer` and emits no `CreateSession`.
- `test_human_answer_resumes_workflow`: After a question, `HumanAnswered` transitions back to `Running` and emits `CreateSession`.
- `test_human_approved_completes`: `HumanApprovedReview` on a `PendingReview` task transitions to `Completed`.
- `test_session_error_transitions_to_errored`: `SessionError` transitions to `Errored`.

**Verification**: Standard suite.

---

### Task 6.2 — Prompt Composer

**Objective**: Implement `compose_user_message` in `src/workflow/prompt_composer.rs` that builds per-agent user messages from task context, prior work, and optional kickback reason.

**Files to Read**:
- `src/workflow/prompt_composer.rs` (created in 1.1)
- `src/workflow/agents.rs` (Task 1.4)
- `src/tasks/models.rs` (Task 1.3)
- `docs/design.md` — "Agent User Message Composition" section (full template)

**Files to Modify**:
- `src/workflow/prompt_composer.rs`

**Implementation Details**:

```rust
/// Composes the user message sent to opencode for a given agent and task.
///
/// The system prompt (agent persona, structured output instructions) lives
/// in the opencode agent definition file. This function only builds the
/// user-visible context message.
///
/// # Arguments
/// * `agent` - The pipeline agent receiving the message.
/// * `task` - The full task, including all accumulated prior work.
/// * `kickback_reason` - If this is a retry after a kickback, the reason text.
pub fn compose_user_message(
    agent: &AgentKind,
    task: &Task,
    kickback_reason: Option<&str>,
) -> String
```

The composed message structure:
1. `## Task Context` — `Story: {story_name}`, `Task: {name}`, `Status: {status}`
2. `## Description` — task description text
3. `## Prior Q&A` — all answered questions (agent, question, answer), if any
4. `## Design` — design section, if non-empty (skip for Intake which has not seen it yet)
5. `## Implementation Plan` — if non-empty (skip for Intake and Design)
6. `## Work Log` — most recent 10 entries, formatted as `{timestamp} [{agent}] {description}`
7. `## Kickback Context` — only present if `kickback_reason.is_some()`
8. `## Your Role` — one line: `"You are the {agent_display_name} agent. Proceed with your role as defined in your system prompt."`

Agent-specific inclusions:
- `Intake`: include only 1, 2, 7, 8.
- `Design`: include 1, 2, 3, 7, 8.
- `Planning`, `Implementation`, `CodeQuality`, `SecurityReview`, `CodeReview`: include all sections.

**Tests to Write**:
- `test_compose_intake_excludes_design`: Compose for `Intake`, assert the output does NOT contain `"## Design"`.
- `test_compose_planning_includes_design`: Compose for `Planning` with a non-empty `task.design`, assert `"## Design"` appears.
- `test_compose_with_kickback`: Compose with `kickback_reason: Some("SQL injection found")`, assert `"## Kickback Context"` and `"SQL injection found"` appear.
- `test_compose_without_kickback`: Compose with `kickback_reason: None`, assert `"## Kickback Context"` does NOT appear.
- `test_compose_work_log_truncated`: Build a task with 15 work log entries; compose for `CodeReview`, assert only the most recent 10 appear.
- `test_compose_includes_task_context`: Assert every composed message contains the task name and story name.

**Verification**: Standard suite.

---

## Story 7: TUI Tabs 2 & 3

**Objective**: Implement Tab 2 (Agent Activity) showing the streaming SSE content from the active agent, and Tab 3 (Team Status) showing the pipeline visualization and work log.

**Estimated effort**: ~4 hours (2 tasks)

---

### Task 7.1 — Tab 2: Agent Activity

**Objective**: Implement the Agent Activity tab that displays streaming text from opencode SSE events and tool execution activity.

**Files to Read**:
- `src/tui/tabs/agent_activity.rs` (created in 1.1)
- `src/tui/tabs/mod.rs` (Task 4.3)
- `src/app.rs` (Task 4.1)
- `src/messages.rs` (Task 1.4)
- `src/opencode/types.rs` (Task 5.1)
- `docs/design.md` — "TUI Layout" (Tab 2 description)

**Files to Modify**:
- `src/tui/tabs/agent_activity.rs`
- `src/app.rs` — add `tab2_state: Tab2State`

**Implementation Details**:

```rust
/// State for Tab 2 (Agent Activity).
pub struct Tab2State {
    /// Accumulated text lines from streaming SSE events, per task.
    lines: HashMap<TaskId, Vec<ActivityLine>>,
    /// Current scroll offset.
    pub scroll: u16,
}

pub enum ActivityLine {
    Text(String),
    ToolActivity { tool: String, status: String },
    AgentBanner { agent: String },
}

impl Tab2State {
    pub fn new() -> Self
    /// Appends streaming text parts from a `StreamingUpdate` message.
    pub fn push_streaming(&mut self, task_id: &TaskId, parts: &[MessagePart])
    /// Appends a tool activity line.
    pub fn push_tool(&mut self, task_id: &TaskId, tool: &str, status: &str)
    /// Clears activity for a task (called when a new agent session starts).
    pub fn clear(&mut self, task_id: &TaskId)
    /// Scrolls the view.
    pub fn scroll_up(&mut self)
    pub fn scroll_down(&mut self)
}

/// Renders Tab 2 into `area`.
pub fn render(frame: &mut Frame, area: Rect, task_id: Option<&TaskId>, state: &Tab2State)
```

Display: Scrollable paragraph of `ActivityLine`s. Text lines are rendered in the default style. Tool activity lines are rendered with a dim style (e.g., `"  [tool] bash: executing"`). Agent banners are rendered bold (e.g., `"=== Implementation Agent ==="`). Auto-scroll to bottom when new content arrives.

Wire `App::handle_message` to call `tab2_state.push_streaming` on `AppMessage::StreamingUpdate` and `tab2_state.push_tool` on `AppMessage::ToolActivity`.

**Tests to Write**:
- `test_push_streaming_text`: Push a `MessagePart::Text` item, assert the `lines` vec has one `ActivityLine::Text` entry.
- `test_push_tool_activity`: Call `push_tool`, assert `ActivityLine::ToolActivity` is added.
- `test_clear_removes_task_lines`: Push lines, then `clear`, assert no lines remain for that task.
- `test_scroll_bounds`: Call `scroll_up()` when at top, assert scroll stays at 0.

**Verification**: Standard suite.

---

### Task 7.2 — Tab 3: Team Status

**Objective**: Implement the Team Status tab that shows the agent pipeline progress visualization and the task's work log.

**Files to Read**:
- `src/tui/tabs/team_status.rs` (created in 1.1)
- `src/workflow/agents.rs` (Task 1.4)
- `src/workflow/mod.rs` (Task 6.1)
- `src/app.rs` (Task 4.1)
- `src/tasks/models.rs` (Task 1.3)
- `docs/design.md` — "TUI Layout" (Tab 3 description)

**Files to Modify**:
- `src/tui/tabs/team_status.rs`
- `src/app.rs` — add `workflow_engine: WorkflowEngine` field, add `tab3_state: Tab3State`

**Implementation Details**:

```rust
pub struct Tab3State {
    /// Scroll offset for the work log.
    pub log_scroll: u16,
}

/// Renders Tab 3 into `area`.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    task: Option<&Task>,
    workflow_state: Option<&WorkflowState>,
    state: &Tab3State,
)
```

Layout (top to bottom):
1. **Pipeline visualization** (fixed height ~5 rows): Show all 7 agents as blocks, highlighting the `current_agent`. Completed agents shown with a checkmark. Use ratatui `Block` widgets or a custom row of spans.
   ```
   [Intake v] [Design v] [Planning v] [>> Implementation <<] [CodeQuality] [SecReview] [CodeReview]
   ```
2. **Current phase** (1 row): e.g., `Phase: Running` or `Phase: Awaiting Answer`.
3. **Work Log** (remaining height): Scrollable list of `WorkLogEntry` items, newest first, formatted as `{timestamp} [{agent}] {description}`.

**Tests to Write**:
- `test_pipeline_render_all_agents`: Render with `current_agent = Implementation`. Use `TestBackend` and assert the rendered buffer contains `"Implementation"`.
- `test_work_log_empty`: Render with a task that has no work log entries, assert no panic.
- `test_tab3_no_task`: Render with `task: None`, assert buffer contains a placeholder.

**Verification**: Standard suite.

---

## Story 8: App Integration

**Objective**: Wire all subsystems together into a coherent application by implementing the full `App::handle_message` dispatcher and integrating the `notify`-based file watcher.

**Estimated effort**: ~4.5 hours (2 tasks)

---

### Task 8.1 — Message Dispatcher

**Objective**: Implement the complete `App::handle_message` method that routes every `AppMessage` variant to the correct subsystem (TaskStore, WorkflowEngine, TUI state, OpenCodeClient).

**Files to Read**:
- `src/app.rs` (Tasks 4.1, 4.2, 4.3, 7.1, 7.2)
- `src/messages.rs` (Task 1.4)
- `src/workflow/mod.rs` (Task 6.1)
- `src/tasks/mod.rs` (Task 2.3)
- `src/opencode/mod.rs` (Task 5.2)
- `docs/design.md` — "Agent Workflow Engine" (full flow, steps 1-15)

**Files to Modify**:
- `src/app.rs`

**Implementation Details**:

Expand `App` struct:
```rust
pub struct App {
    pub task_store: TaskStore,
    pub selected_task: Option<TaskId>,
    pub active_tab: usize,
    pub should_quit: bool,
    pub workflow_engine: WorkflowEngine,
    pub tab1_state: Tab1State,
    pub tab2_state: Tab2State,
    pub tab3_state: Tab3State,
    pub opencode_client: Option<Arc<OpenCodeClient>>,
    pub session_map: Arc<RwLock<HashMap<String, (TaskId, AgentKind)>>>,
    /// Messages produced during `handle_message` that need to be dispatched on the next tick.
    pending_messages: Vec<AppMessage>,
}
```

`handle_message` routes:

| Message | Action |
|---------|--------|
| `TerminalEvent` | Call `tui::handle_input`, push returned message if Some |
| `Tick` | Drain and dispatch `pending_messages` |
| `Shutdown` | Set `should_quit = true` |
| `StartTask { task_id }` | Push result of `workflow_engine.process(StartTask)` into `pending_messages` |
| `CreateSession { task_id, agent, prompt }` | Async: call `client.create_session()`, push `SessionCreated` |
| `SessionCreated` | Update `session_map`; push `workflow_engine.process(SessionCreated)` |
| `SendPrompt` | Async: call `client.send_prompt_async()` |
| `StreamingUpdate` | Call `tab2_state.push_streaming()` |
| `ToolActivity` | Call `tab2_state.push_tool()` |
| `SessionCompleted` | Parse structured output from last message, push `AgentCompleted` or `AgentKickedBack` or `AgentAskedQuestion` |
| `AgentCompleted` | `task_store.persist(task_id)`, push `workflow_engine.process()` |
| `AgentKickedBack` | `task_store.persist(task_id)`, push `workflow_engine.process()` |
| `AgentAskedQuestion` | `task_store.persist(task_id)`, push `workflow_engine.process()` |
| `HumanAnswered` | Update question in `task_store`, push `workflow_engine.process()` |
| `HumanApprovedReview` | Push `workflow_engine.process()` |
| `HumanRequestedRevisions` | Push `workflow_engine.process()` |
| `TaskUpdated` | `task_store.reload(task_id)` |
| `TaskFileChanged` | `task_store.reload(task_id)` |
| `DiffReady` | Store diffs for Tab 4 |

Note: Async operations (`CreateSession`, `SendPrompt`) must be dispatched via `tokio::spawn` and send results back through the `mpsc::Sender<AppMessage>` channel, not executed inline (which would block the TUI).

**Tests to Write**:
- `test_handle_shutdown`: Call `handle_message(AppMessage::Shutdown)`, assert `app.should_quit == true`.
- `test_handle_terminal_event_q_key`: Simulate a `'q'` key event, assert `Shutdown` is in pending messages.
- `test_handle_start_task_emits_create_session`: Call `handle_message(AppMessage::StartTask { task_id })`, assert `pending_messages` contains `AppMessage::CreateSession`.
- `test_handle_streaming_update_updates_tab2`: Call with `StreamingUpdate`, assert `tab2_state` has a new line.
- `test_handle_task_updated_reloads_store`: Requires a temp task file; call `TaskUpdated`, assert the in-memory task reflects disk content.

**Verification**: Standard suite.

---

### Task 8.2 — File Watcher Integration

**Objective**: Integrate the `notify` file watcher so that external changes to task files (e.g., a human editing a file) are detected and reloaded into the `TaskStore`.

**Files to Read**:
- `src/tasks/mod.rs` (Task 2.3)
- `src/app.rs` (Task 8.1)
- `src/messages.rs` (Task 1.4)
- `src/error.rs` (Task 1.2)
- `Cargo.toml` (confirm `notify = "6"`)
- `docs/design.md` — "Task File Parsing" section (file watching mention)

**Files to Modify**:
- `src/tasks/mod.rs` — add `watch` method
- `src/main.rs` — spawn the watcher task

**Implementation Details**:

```rust
// in src/tasks/mod.rs

/// Spawns a file watcher that sends `AppMessage::TaskFileChanged` when any
/// task file in the watch directory is modified.
///
/// Returns the `notify::RecommendedWatcher` which must be kept alive.
pub fn watch_task_files(
    task_dir: &Path,
    tx: tokio::sync::mpsc::Sender<AppMessage>,
    store: Arc<RwLock<TaskStore>>,
) -> crate::error::Result<notify::RecommendedWatcher>
```

Use `notify::recommended_watcher` with `notify::RecursiveMode::NonRecursive`. On `notify::EventKind::Modify`, compute the `TaskId` from the event path and send `AppMessage::TaskFileChanged { task_id }` on the channel.

In `src/main.rs`: After creating the `TaskStore`, call `watch_task_files` with the `mpsc::Sender`. Keep the returned watcher alive for the duration of the program by storing it in `main`.

**Tests to Write** (using `tempfile`):
- `test_file_watcher_detects_modify`: Write a task file to a temp dir, start the watcher, modify the file, assert `AppMessage::TaskFileChanged` is received on the channel within 2 seconds.
- `test_file_watcher_ignores_non_md`: Create a `.txt` file change in the task dir, assert no message is sent.

**Verification**: Standard suite.

---

## Story 9: Code Review Tab (Phase 4)

**Objective**: Implement Tab 4 — a unified diff viewer that shows file changes produced by the agent session, with a comment input area and Approve/Request Revisions actions.

**Estimated effort**: ~2.5 hours (1 task)

---

### Task 9.1 — Tab 4: Code Review Diff Viewer

**Objective**: Implement the Code Review tab with syntax-highlighted diff rendering, a comment input area, and keyboard shortcuts to Approve or Request Revisions.

**Files to Read**:
- `src/tui/tabs/code_review.rs` (created in 1.1)
- `src/tui/tabs/mod.rs` (Task 4.3)
- `src/opencode/types.rs` (Task 5.1 — `FileDiff`, `DiffLine`)
- `src/app.rs` (Task 8.1)
- `src/messages.rs` (Task 1.4)
- `docs/design.md` — "TUI Layout" (Tab 4), "Agent Workflow Engine" (steps 12-15)
- `Cargo.toml` (confirm `similar = "2"`)

**Files to Modify**:
- `src/tui/tabs/code_review.rs`
- `src/app.rs` — add `tab4_state: Tab4State`, handle `DiffReady` message

**Implementation Details**:

```rust
/// State for Tab 4 (Code Review).
pub struct Tab4State {
    /// Diffs fetched from the opencode session.
    pub diffs: Vec<FileDiff>,
    /// Index of the currently viewed file diff.
    pub selected_file: usize,
    /// Scroll offset within the diff.
    pub scroll: u16,
    /// Multi-line comment input.
    pub comment_input: tui_textarea::TextArea<'static>,
    /// Accumulated review comments from the human.
    pub comments: Vec<String>,
    /// Whether the comment input area is focused.
    pub comment_focused: bool,
}

/// Renders Tab 4 into `area`.
pub fn render(frame: &mut Frame, area: Rect, task: Option<&Task>, state: &Tab4State)
```

Layout:
1. **File list header** (1 row): `"File 1/3: src/foo.rs"` with `←`/`→` to switch files.
2. **Diff view** (70% height): Scrollable. `DiffLine::Added` in green, `DiffLine::Removed` in red, `DiffLine::Context` in default style. Prefix with `"+"`, `"-"`, `" "`.
3. **Comment input** (remaining height): `tui-textarea` with a `"── Review Comment ──"` label.
4. **Action bar** (1 row): `"[a] Approve  [r] Request Revisions  [Enter] Add Comment"`.

Keyboard actions (handled in `tui/mod.rs` `handle_input` when Tab 4 is active):
- `a`: Emit `HumanApprovedReview { task_id }`.
- `r`: Collect all comments from `tab4_state.comments`, emit `HumanRequestedRevisions { task_id, comments }`.
- `Enter` (when comment input focused): Append `comment_input` text to `tab4_state.comments`, clear input.
- `←`/`→`: Navigate files.
- `Up`/`Down`: Scroll diff.

Wire `App::handle_message(DiffReady { task_id, diffs })` to store diffs in `tab4_state.diffs` and switch `active_tab` to 3 (Tab 4 index).

After `HumanApprovedReview` is handled in the workflow, fetch diffs via `client.get_session_diffs(session_id)` and send `DiffReady`.

**Tests to Write**:
- `test_tab4_diff_render_added_line`: Build a `FileDiff` with one `DiffLine::Added("hello")`, render, assert the buffer contains `"+hello"`.
- `test_tab4_diff_render_removed_line`: Assert `"-hello"` for a removed line.
- `test_tab4_add_comment`: Call `tab4_state.comments.push("fix this".into())`, assert it appears in the list.
- `test_tab4_file_navigation`: Set `diffs` to 3 files, assert `selected_file` increments correctly and wraps.
- `test_tab4_no_diffs_renders_placeholder`: Render with empty `diffs` vec, assert a placeholder message is shown.

**Verification**: Standard suite.

---

## Story 10: Integration & Polish

**Objective**: Wire all subsystems end-to-end, harden error handling and resilience, and perform final cleanup (docs, lint, test coverage). At the end of this story, ClawdMux is a functionally complete MVP.

**Estimated effort**: ~6 hours (3 tasks)

---

### Task 10.1 — End-to-End Wiring

**Objective**: Ensure all subsystems are fully connected in `main.rs` and validate the full workflow from task selection through agent completion with an integration test using a mock opencode server.

**Files to Read**:
- `src/main.rs` (Task 4.1, 8.2)
- `src/app.rs` (Task 8.1)
- `src/opencode/server.rs` (Task 5.4)
- `src/opencode/events.rs` (Task 5.3)
- `src/workflow/mod.rs` (Task 6.1)
- `docs/design.md` — "Verification Plan" section

**Files to Modify**:
- `src/main.rs` — full startup sequence
- Any module where stubs remain that block integration

**Implementation Details**:

Implement the full startup sequence in `main.rs`:
1. Initialize `tracing_appender` file sink; log to `clawdmux.log`.
2. Parse CLI args. If `Commands::Init`, run init and exit.
3. Load `AppConfig` from project root.
4. Spawn opencode server via `OpenCodeServer::ensure_running`.
5. Create `OpenCodeClient` with the server's base URL.
6. Create `TaskStore` and call `load_from_disk`.
7. Create `session_map: Arc<RwLock<...>>`.
8. Create `EventStreamConsumer` and spawn it as a `tokio::task`.
9. Spawn the file watcher task.
10. Initialize the TUI (crossterm raw mode, alternate screen, `ratatui::Terminal`).
11. Create `App` with all subsystems initialized.
12. Run the event loop: process crossterm events, dispatch `AppMessage::Tick` at ~16ms intervals, draw each frame.
13. On `should_quit`, restore terminal, call `server.shutdown()`.

**Integration Tests** (in `tests/integration_test.rs`):
- `test_full_workflow_intake_to_completed`: Use `mockito` to mock all opencode API calls. Simulate: `StartTask` → mock `POST /session` → mock SSE events for `SessionCompleted` → simulate `AgentCompleted` for all 7 agents in sequence → assert `workflow_engine.state(&task_id).map(|s| &s.phase) == Some(&WorkflowPhase::PendingReview)` after CodeReview completes.
- `test_task_file_updated_after_agent_completion`: After `AgentCompleted`, verify `task_store.get(id).status == TaskStatus::InProgress` and `work_log` has a new entry.

**Verification**: Standard suite. `cargo run` must open the TUI, load tasks, and respond to keyboard input without panicking.

---

### Task 10.2 — Error Handling & Resilience

**Objective**: Audit all error paths, add missing error handling, implement the opencode server restart-on-crash logic, and add TUI error display for user-visible errors.

**Files to Read**:
- All source files (quick pass to find `unwrap()`, `expect()`, `todo!()`, `panic!()` outside of tests)
- `docs/design.md` — "Risk Assessment" section (all mitigations)
- `src/app.rs`
- `src/tui/layout.rs`

**Files to Modify**:
- Any file with unhandled errors
- `src/app.rs` — add `error_message: Option<String>` field
- `src/tui/layout.rs` — render error banner in footer when `app.error_message.is_some()`

**Implementation Details**:

1. **Eliminate `unwrap()`/`expect()`** outside of tests. Replace with proper error propagation using `?` or explicit `match`.
2. **Server restart**: In `EventStreamConsumer::run`, if SSE disconnects, attempt to reconnect. If reconnection fails after 3 retries, send `AppMessage::SessionError`.
3. **Structured output parse failure**: In `App::handle_message(SessionCompleted)`, if the last message does not contain valid JSON matching the expected schema, log a warning and emit an `AppMessage::SessionError` instead of panicking.
4. **TUI error banner**: Add a red-background error message row to the footer when `app.error_message` is set. Pressing `Esc` clears it.
5. **`clawdmux doctor` sub-command**: Implement `Commands::Doctor` in `main.rs` that checks for the opencode binary (`which::which("opencode")`), checks server health, and prints a summary. Does not open the TUI.

**Tests to Write**:
- `test_structured_output_parse_failure_does_not_panic`: Send a `SessionCompleted` with a non-JSON final message, assert `AppMessage::SessionError` is in `pending_messages`, not a panic.
- `test_error_message_set_on_session_error`: Handle `SessionError`, assert `app.error_message.is_some()`.
- `test_doctor_command_no_opencode`: If `opencode` is not in PATH, `run_doctor` should return an `Err` describing the issue (or print and return `Ok` — choose one approach and test it).

**Verification**: Standard suite. Run `cargo clippy -- -D warnings` and fix every diagnostic.

---

### Task 10.3 — Final Cleanup & Documentation

**Objective**: Bring the project to full compliance with all CLAUDE.md rules: complete doc comments on all public items, update `README.md` and `docs/design.md` with accurate final information, achieve >70% test coverage, and ensure all four verification commands pass cleanly.

**Files to Read**:
- All source files (for missing doc comments)
- `README.md` (current content)
- `docs/design.md` (current content)
- `CLAUDE.md` (all rules)

**Files to Modify**:
- Any source file missing `///` doc comments on public items
- `README.md` — update with installation instructions, usage, and architecture overview
- `docs/design.md` — update any sections that diverged from the final implementation

**Implementation Details**:

1. **Doc comments**: Add `///` comments to every `pub` struct, enum, function, and module that lacks one. Describe purpose, arguments, and return values.
2. **README.md**: Write a concise README with:
   - Project description (2-3 sentences)
   - Prerequisites (`opencode`, Rust toolchain)
   - Quick start: `clawdmux init` then `clawdmux`
   - Task file format summary (link to `docs/requirements.md`)
   - Architecture overview (link to `docs/design.md`)
   - Development: `cargo build`, `cargo test`, `cargo clippy`
3. **Test coverage audit**: Run `cargo test` and review which modules have thin coverage. Add tests where coverage is below 70% for non-trivial logic. Focus on `parser.rs`, `writer.rs`, `workflow/transitions.rs`, and `prompt_composer.rs`.
4. **Clippy final pass**: Run `cargo clippy -- -D warnings`. Fix every warning. Common items: unused imports, missing `Default` impls, unnecessary clones.
5. **`cargo fmt` pass**: Ensure all code is formatted.

**Tests to Write** (gap-filling, as identified during the coverage audit — examples):
- Additional parser edge case tests if not already at 70%.
- Additional writer round-trip tests if not already at 70%.
- Any missing `AgentKind` or `TaskStatus` conversion tests.

**Verification**: Standard suite. The project must have:
- Zero `cargo build` errors or warnings.
- Zero `cargo clippy -- -D warnings` diagnostics.
- All `cargo test` tests passing.
- `cargo fmt -- --check` returning exit code 0.

---

## Appendix: Story Effort Summary

| Story | Tasks | Estimated Hours | Phase |
|-------|-------|-----------------|-------|
| 1. Project Skeleton & Core Types | 1.1 – 1.4 | ~7h | Pre-Phase |
| 2. Task File Parsing & Writing | 2.1 – 2.3 | ~6.5h | Pre-Phase |
| 3. Configuration System | 3.1 – 3.2 | ~4.5h | Pre-Phase |
| 4. TUI Shell & Layout | 4.1 – 4.3 | ~6.5h | Phase 1 |
| 5. OpenCode Client | 5.1 – 5.4 | ~8.5h | Phase 2 |
| 6. Workflow Engine | 6.1 – 6.2 | ~4.5h | Phase 3 |
| 7. TUI Tabs 2 & 3 | 7.1 – 7.2 | ~4h | Phase 3 |
| 8. App Integration | 8.1 – 8.2 | ~4.5h | Phase 3 |
| 9. Code Review Tab | 9.1 | ~2.5h | Phase 4 |
| 10. Integration & Polish | 10.1 – 10.3 | ~6h | Phase 4 |
| **Total** | **26 tasks** | **~54.5h** | |
