# Task and Story File Format

This document describes the markdown format used for task files in the `tasks/` directory. Follow this format exactly when generating implementation plans.

---

## File Naming

Tasks are named `{story}.{task}.md`, where `{story}` is the integer story number and `{task}` is the integer task number within that story.

Examples: `tasks/3.1.md`, `tasks/3.2.md`, `tasks/4.1.md`

Stories are not separate files. The story title and number are embedded in each task file header.

---

## File Structure

Every task file has the following structure:

```
Story: {N}. {Story Title}
Task: {N}.{M}
Status: {STATUS}
Assigned To: [{Agent Name}]

## Description

{task description}

## Questions

{optional — added during agent intake}

## Design

{optional — added during agent design phase}

## Work Log

{optional — added as work progresses}
```

`Assigned To:` is optional and omitted from new task files. It is written automatically by the pipeline as the task advances through agents.

### Header (required)

Three required plain-text lines at the top of the file — no markdown heading — followed by an optional fourth:

```
Story: 3. OpenCode Integration
Task: 3.2
Status: OPEN
Assigned To: [Intake Agent]
```

- `Story:` — the story number and human-readable title this task belongs to.
- `Task:` — the fully qualified task ID (`{story}.{task}`).
- `Status:` — one of `OPEN`, `IN_PROGRESS`, `PENDING_REVIEW`, `COMPLETED`, or `ABANDONED`.
- `Assigned To:` *(optional)* — the agent or person currently responsible for the task. Written in brackets. Valid values: `[Human]`, `[Intake Agent]`, `[Design Agent]`, `[Planning Agent]`, `[Implementation Agent]`, `[Code Quality Agent]`, `[Security Review Agent]`, `[Code Review Agent]`. Automatically updated by the pipeline as a task moves between stages; omit from new task files.

The pipeline sets `Assigned To:` automatically:
- `StartTask` → `[Intake Agent]`
- After each agent completes → the next agent in the pipeline
- When an agent asks a question or the task reaches `PENDING_REVIEW` → `[Human]`
- When the human approves the final review → `[Human]` (status also becomes `COMPLETED`)

### `## Description` (required)

A concise but complete description of the work to be done. Should include:

- **What** to implement — specific structs, functions, files, or behaviors.
- **Where** to put it — file paths if known.
- **Tests** — a comma-separated list of test function names that must be written as part of the task.

Keep descriptions self-contained. An agent reading only the Description section should have enough information to begin work without reading other task files.

Example:

```
## Description

Implement all Rust types in src/opencode/types.rs mirroring the OpenCode OpenAPI
schema: OpenCodeSession, MessagePart, OpenCodeMessage, MessageRole, OpenCodeEvent,
FileDiff, DiffStatus, DiffHunk, DiffLine, SendMessageRequest, ContentPart,
CreateSessionResponse, HealthResponse. Use serde with camelCase renaming where
appropriate.

Tests: test_message_part_text_serde, test_create_session_response_deserialize,
test_health_response_ok, test_file_diff_status_serde, test_opencode_event_serde.
```

### `## Questions` (optional)

Added by agents during the intake or design phase when requirements are ambiguous. Each question and answer pair uses this format:

```
Q{N} [{Agent Name}]: {question text}
A{N}: {answer text}
```

- Questions are numbered sequentially starting at 1.
- The agent role is included in brackets (e.g., `[Intake Agent]`, `[Design Agent]`).
- Answers are written by the human reviewer or orchestrator.

Example:

```
## Questions

Q1 [Intake Agent]: Should file navigation in the diff view use left/right arrow keys
or up/down?
A1: Use up/down for file navigation.
```

### `## Design` (optional)

Added by the design phase agent. Contains notes, diagrams, or pseudocode describing the chosen implementation approach before coding begins. Free-form markdown.

### `## Work Log` (optional)

A numbered, chronological list of work entries appended as the task progresses. Each entry is a single line:

```
{N} {ISO-8601 timestamp} [{Agent Name}] {description of work done}
```

Example:

```
## Work Log

1 2026-02-24T03:00:00+00:00 [Implementation Agent] Implemented OpenCodeSession and
MessagePart types with serde derives; all 42 tests pass.
2 2026-02-24T05:30:00+00:00 [Implementation Agent] Added FileDiff, DiffHunk, DiffLine;
fixed camelCase rename on SendMessageRequest fields; all 51 tests pass.
```

---

## Story Breakdown Guidelines

When breaking a design into stories and tasks, follow these conventions:

- **Stories** group related tasks under a single theme (e.g., "OpenCode Client", "TUI Layout", "Task Parser"). Stories are numbered sequentially starting at 1.
- **Tasks** within a story are numbered sequentially starting at 1 (e.g., 3.1, 3.2, 3.3). Each task should represent roughly a half-day to full-day of focused implementation work.
- Tasks should be **independently completable** — later tasks may depend on earlier ones, but a single task should not require parallel work streams.
- Every task must specify **named tests** in the Description. Do not write tasks without tests.
- Prefer **small, vertical slices** over large horizontal ones: a task that adds one fully-tested feature end-to-end is better than a task that adds many untested stubs.
- Status is always `OPEN` when first written. Never write `IN_PROGRESS` or `COMPLETED` in a generated plan.
