# ClawdMux Design Document (Revised)

## Context

ClawdMux is a greenfield Rust TUI application that orchestrates GenAI coding agents through a scrum-style task management interface. Instead of directly spawning CLI processes, ClawdMux acts as a **client to an opencode server**, leveraging opencode's HTTP API for LLM interaction, tool execution, and session management.

This is a significant architectural improvement over the original design (which used PTY-spawned Claude Code sessions), eliminating terminal emulation complexity while gaining multi-provider LLM support, a stable API contract, and richer integration possibilities.

---

## Why OpenCode

### What OpenCode Provides

OpenCode (`anomalyco/opencode`) is an open-source AI coding agent with a client-server architecture. When you run `opencode`, it starts a server with a built-in TUI client. The server can also run headless via `opencode serve`, exposing a full REST API.

**Key capabilities ClawdMux leverages:**
- **Session API** (`/session/*`): Create, manage, fork, and abort AI conversation sessions
- **Message API** (`/session/:id/message`, `/session/:id/prompt_async`): Send prompts and receive structured responses
- **Diff API** (`/session/:id/diff`): Get file changes produced by a session (perfect for Code Review tab)
- **File API** (`/find`, `/file/*`): Search and read project files
- **Event Stream** (`/global/event`): SSE with 40+ event types for real-time monitoring
- **Agent System**: Built-in agents (`build`, `plan`, `general`, `explore`) plus full support for custom agents defined as markdown files with per-agent system prompts, tool permissions, and model overrides
- **Multi-Provider**: 75+ LLM providers (Anthropic Claude, OpenAI, Google Gemini, local models, etc.)
- **Tool System**: Built-in read/write/edit/bash/LSP tools with permission gates
- **OpenAPI 3.1.1 Spec**: Available at `/doc` -- can generate typed Rust client

### What ClawdMux Adds on Top

- **Scrum-style task management**: Stories, tasks, status tracking from markdown files
- **7-agent sequential pipeline**: Intake -> Design -> Planning -> Implementation -> Code Quality -> Security Review -> Code Review
- **Backward-kick workflow**: Later agents can send tasks back to earlier stages
- **Human-in-the-loop**: Questions, approval gates, code review with comments
- **Orchestration**: Defining custom opencode agents (one per pipeline stage) and composing user messages from task context + prior work
- **Unified TUI**: Left pane task navigation + 4-tab right pane (details, activity, team status, review)

### What We No Longer Need

- ~~`portable-pty`~~ -- No PTY management; communication is HTTP
- ~~`vt100`~~ -- No terminal emulation; responses are structured text/JSON
- ~~`[CLAWDMUX:*]` signal markers~~ -- Use structured output (JSON schema) via opencode's API
- ~~Custom VT rendering in Tab 2~~ -- Display streaming markdown/text instead
- ~~Flat personality `.txt` files~~ -- Agent system prompts are native opencode agent definitions in `.opencode/agents/clawdmux/`

---

## Architecture: Event-Driven OpenCode Client

### High-Level Overview

```
                    +------------------+
                    |    main.rs       |
                    |  (bootstrap &    |
                    |   event loop)    |
                    +--------+---------+
                             |
              +--------------+--------------+
              |              |              |
    +---------v--+  +--------v-----+  +----v----------+
    |  TUI Layer |  |  Workflow    |  |  OpenCode     |
    |  (ratatui) |  |  Engine     |  |  Client       |
    +-----+------+  +------+------+  +-------+-------+
          |                |                  |
          |         +------v------+    +------v------+
          |         |  Task Store |    |  opencode   |
          |         +-------------+    |  serve      |
          |                            +-------------+
          +------- async mpsc channels -------+
```

ClawdMux remains a single Rust binary with internal async subsystems communicating via `mpsc` channels. The key change is the **Session Manager is replaced by an OpenCode Client** that communicates with an opencode server over HTTP + SSE.

### OpenCode Server Lifecycle

ClawdMux manages the opencode server as a child process:

1. On startup, check if an opencode server is already running (health check at configured port)
2. If not running, spawn `opencode serve --port <port> --hostname 127.0.0.1` as a background child process with **LLM provider credentials injected as environment variables** (e.g., `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`) read from ClawdMux's global config
3. Wait for health check to pass (`GET /global/health`)
4. On shutdown, send SIGTERM to the child process (if we spawned it)

Configuration allows connecting to an existing server instead:
```toml
# .clawdmux/config.toml (project-level)
[opencode]
# auto = spawn if needed (default), external = expect running server
mode = "auto"
hostname = "127.0.0.1"
port = 4096
password = ""  # optional, for OPENCODE_SERVER_PASSWORD
```

### Project Initialization (`clawdmux init`)

`clawdmux init` is a one-time setup command run per project. It is interactive (terminal prompts, not TUI) and must be run before `clawdmux` (the TUI) is usable.

**Step 1 -- Dependency check: opencode binary**

```
Checking for opencode... not found.
opencode is required. Install it now? [Y/n]
```

If yes, ClawdMux runs the official install script non-interactively:
```bash
curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path
# Installs to ~/.opencode/bin/opencode
```
After installation, verifies the binary works and prints the installed version.

**Step 2 -- Provider credentials (global, written once per machine)**

Reads `~/.config/clawdmux/config.toml`. If no provider is configured:

```
No LLM provider configured. Let's set one up.
Provider: [1] Anthropic  [2] OpenAI  [3] Google  [4] Other
> 1
API key: ****
Default model [claude-sonnet-4-5]:
Credentials saved to ~/.config/clawdmux/config.toml
```

Global config structure:
```toml
# ~/.config/clawdmux/config.toml
[provider]
default = "anthropic"

[provider.anthropic]
api_key = "sk-ant-..."
default_model = "claude-sonnet-4-5"

# Additional providers can be added here
# [provider.openai]
# api_key = "sk-..."
# default_model = "gpt-4.1"
```

Credentials are stored only in ClawdMux's own config file -- they are never written to opencode's config. ClawdMux passes them to opencode via environment variables at process spawn time.

**Step 3 -- Project scaffold**

Creates the project-local files if they don't exist:

```
Scaffolding project...
  created .clawdmux/config.toml
  created .opencode/agents/clawdmux/intake.md
  created .opencode/agents/clawdmux/design.md
  created .opencode/agents/clawdmux/planning.md
  created .opencode/agents/clawdmux/implementation.md
  created .opencode/agents/clawdmux/code-quality.md
  created .opencode/agents/clawdmux/security-review.md
  created .opencode/agents/clawdmux/code-review.md
  created tasks/  (task file directory)
```

The agent definition files contain sensible defaults. Users may edit them to customize agent behaviour. Running `clawdmux init --reset-agents` regenerates them from built-in defaults.

Implementation note: `run_init` delegates to an internal `run_init_with_paths(global_config_path, project_root, args)` that accepts an explicit global config path. This mirrors the `AppConfig::load_from` pattern and allows tests to supply a `TempDir`-based path without touching `~/.config/clawdmux/config.toml`. The opencode binary check lives only in the public `run_init` entry point and is intentionally excluded from `run_init_with_paths`.

**Step 4 -- Summary**

```
clawdmux is ready. Run `clawdmux` to open the TUI.
```

### Model Selection Hierarchy

ClawdMux controls model selection at three layers (highest priority first):

| Priority | Mechanism | How to Set |
|----------|-----------|------------|
| 1 | **Per-prompt user override** | User types a model name into the supplemental prompt field in Tab 1; passed as `model` in the API request body |
| 2 | **Per-agent default** | `model:` field in `.opencode/agents/clawdmux/<agent>.md` frontmatter |
| 3 | **Global ClawdMux default** | `[provider.<name>].default_model` in `~/.config/clawdmux/config.toml`; used as the fallback in agent definition files via a `{global_default}` placeholder resolved at agent file generation time |

This means:
- Agents use their own model by default (e.g., Implementation uses a capable model, Intake uses a lighter/faster one)
- The user can override any specific prompt without changing config
- The global default seeds all agent files at `clawdmux init` time but each can be hand-edited independently

### Crate & Module Structure

```
clawdmux/
  Cargo.toml
  src/
    main.rs                    # Entry point, bootstrap, event loop
    app.rs                     # Top-level App state, message dispatcher
    error.rs                   # Centralized ClawdMuxError enum

    messages.rs                # AppMessage enum -- the contract between subsystems

    workflow/
      mod.rs                   # WorkflowEngine: agent pipeline state machine
      agents.rs                # AgentKind enum, pipeline ordering, valid transitions
      transitions.rs           # State transition logic, kickback validation
      prompt_composer.rs       # Builds user messages from task context + prior work (system prompt is in opencode agent definition)

    tasks/
      mod.rs                   # TaskStore: in-memory task cache, file watcher
      models.rs                # Story, Task, TaskStatus, Question, WorkLogEntry structs
      parser.rs                # Markdown task file parser
      writer.rs                # Task file serializer

    opencode/
      mod.rs                   # OpenCodeClient: HTTP client + SSE event listener
      types.rs                 # Rust types mirroring opencode's OpenAPI schema
      session.rs               # Session lifecycle: create, prompt, abort, fork
      events.rs                # SSE event stream consumer, maps to AppMessage
      server.rs                # Server lifecycle: spawn, health check, shutdown

    tui/
      mod.rs                   # Top-level draw() and input handling
      layout.rs                # Main layout: header, left pane, right pane, footer
      task_list.rs             # Left pane: story/task tree widget
      tabs/
        mod.rs                 # Tab bar and tab dispatch
        task_details.rs        # Tab 1: task markdown, prompt input, Q&A
        agent_activity.rs      # Tab 2: streaming agent activity view
        team_status.rs         # Tab 3: agent pipeline visualization + work log
        code_review.rs         # Tab 4: diff view + comment input

    config/
      mod.rs                   # AppConfig, config loading (global + project), opencode agent definition management
      init.rs                  # `clawdmux init` command: dependency checks, provider setup, project scaffold
      providers.rs             # ProviderConfig: reads ~/.config/clawdmux/config.toml, resolves env vars to inject into opencode process
```

### Key Data Structures

```rust
// src/tasks/models.rs

/// Unique identifier for a task, derived from its file path.
pub struct TaskId(PathBuf);  // inner field is private; use as_path() accessor

/// A story groups related tasks.
pub struct Story {
    pub name: String,
    pub tasks: Vec<Task>,
}

/// A single task loaded from a markdown file.
pub struct Task {
    pub id: TaskId,
    pub story_name: String,
    pub name: String,
    pub status: TaskStatus,
    pub assigned_to: Option<AgentKind>,
    pub description: String,
    pub starting_prompt: Option<String>,
    pub questions: Vec<Question>,
    pub design: Option<String>,
    pub implementation_plan: Option<String>,
    pub work_log: Vec<WorkLogEntry>,
    pub file_path: PathBuf,
}

pub enum TaskStatus {
    Open,
    InProgress,
    PendingReview,
    Completed,
    Abandoned,
}

pub struct Question {
    pub agent: AgentKind,
    pub text: String,
    pub answer: Option<String>,
}

pub struct WorkLogEntry {
    pub sequence: u32,
    pub timestamp: chrono::DateTime<chrono::Utc>,  // UTC; convert to local time at render
    pub agent: AgentKind,
    pub description: String,
}
```

```rust
// src/workflow/agents.rs

/// The 7 agents in the pipeline.
pub enum AgentKind {
    Intake,
    Design,
    Planning,
    Implementation,
    CodeQuality,
    SecurityReview,
    CodeReview,
}

impl AgentKind {
    /// Returns the next agent in the pipeline, or None if this is the last.
    pub fn next(&self) -> Option<AgentKind> { ... }

    /// Returns the pipeline index (0-6) for ordering comparisons.
    pub fn pipeline_index(&self) -> usize { ... }

    /// Returns which agents this one is allowed to kick back to.
    pub fn valid_kickback_targets(&self) -> &[AgentKind] {
        match self {
            Self::CodeQuality => &[Self::Implementation],
            Self::SecurityReview => &[Self::Implementation, Self::Design],
            Self::CodeReview => &[Self::Implementation, Self::Design, Self::Planning],
            _ => &[],
        }
    }
}
```

```rust
// src/opencode/types.rs -- Rust types for opencode API

/// Represents an opencode session.
pub struct OpenCodeSession {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A message part in an opencode response.
pub enum MessagePart {
    Text { text: String },
    Tool { name: String, input: serde_json::Value, result: Option<String> },
    Reasoning { text: String },
    File { path: String, content: String },
}

/// An opencode API message.
pub struct OpenCodeMessage {
    pub id: String,
    pub role: MessageRole,
    pub parts: Vec<MessagePart>,
}

pub enum MessageRole { User, Assistant }

/// SSE event from opencode's /global/event stream.
pub enum OpenCodeEvent {
    SessionCreated { session_id: String },
    MessageCreated { session_id: String, message: OpenCodeMessage },
    MessageUpdated { session_id: String, message_id: String, parts: Vec<MessagePart> },
    ToolExecuting { session_id: String, tool: String },
    ToolCompleted { session_id: String, tool: String, result: String },
    SessionCompleted { session_id: String },
    SessionError { session_id: String, error: String },
    // ... additional events as needed
}

/// File diff from opencode's /session/:id/diff endpoint.
pub struct FileDiff {
    pub path: String,
    pub status: DiffStatus,
    pub hunks: Vec<DiffHunk>,
}

pub enum DiffStatus { Added, Modified, Deleted }

pub struct DiffHunk {
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

pub enum DiffLine {
    Context(String),
    Added(String),
    Removed(String),
}
```

```rust
// src/messages.rs -- replaces PTY-specific messages with API-based ones

/// All messages flowing between subsystems.
pub enum AppMessage {
    // --- Terminal events ---
    TerminalEvent(crossterm::event::Event),
    Tick,

    // --- Workflow commands ---
    StartTask { task_id: TaskId },
    AgentCompleted { task_id: TaskId, agent: AgentKind, summary: String },
    AgentKickedBack { task_id: TaskId, from: AgentKind, to: AgentKind, reason: String },
    AgentAskedQuestion { task_id: TaskId, agent: AgentKind, question: String },
    HumanAnswered { task_id: TaskId, question_index: usize, answer: String },
    HumanApprovedReview { task_id: TaskId },
    HumanRequestedRevisions { task_id: TaskId, comments: Vec<String> },

    // --- OpenCode session events (replaces PTY events) ---
    CreateSession { task_id: TaskId, agent: AgentKind, prompt: String },
    SessionCreated { task_id: TaskId, session_id: String },
    SendPrompt { task_id: TaskId, session_id: String, prompt: String },
    StreamingUpdate { task_id: TaskId, session_id: String, parts: Vec<MessagePart> },
    ToolActivity { task_id: TaskId, session_id: String, tool: String, status: String },
    SessionCompleted { task_id: TaskId, session_id: String },
    SessionError { task_id: TaskId, session_id: String, error: String },
    AbortSession { task_id: TaskId, session_id: String },

    // --- Diff events ---
    DiffReady { task_id: TaskId, diffs: Vec<FileDiff> },

    // --- Task persistence ---
    TaskUpdated { task_id: TaskId },
    TaskFileChanged { task_id: TaskId },

    // --- Application lifecycle ---
    Shutdown,
}
```

### Agent Workflow Engine

The workflow engine is a pure state machine. Given a `(current_state, message)` pair, it produces `(new_state, Vec<AppMessage>)` side effects. This makes it trivially testable.

```
Pipeline: Intake -> Design -> Planning -> Implementation -> CodeQuality -> SecurityReview -> CodeReview
                                                ^               |               |               |
                                                |               |               |               |
                                                +---(kickback)--+               |               |
                                     ^          ^                               |               |
                                     |          +-----------(kickback)----------+               |
                                     |          |                                               |
                                     +----------+--------------(kickback)----------------------+
```

**Flow (API-based):**
1. Human selects a task and hits "Start" -> `StartTask` message
2. Workflow engine assigns task to `Intake` agent, emits `CreateSession`
3. OpenCode client creates a session via `POST /session`, then sends composed user message via `POST /session/:id/message` with `agent: "clawdmux/intake"` (or the appropriate stage agent)
4. SSE events stream back: tool activity, message parts, completion -> mapped to `StreamingUpdate`, `ToolActivity` messages
5. Agent response is parsed for structured output (JSON schema) indicating completion, questions, or kickback
6. On completion -> `AgentCompleted` -> workflow advances to next agent (new session or forked session)
7. On question -> `AgentAskedQuestion` -> pauses workflow, shows question in Tab 1
8. On kickback -> `AgentKickedBack` -> workflow aborts session, restarts at target agent with kickback context
9. On session error -> `SessionError` -> workflow pauses and presents the user with a choice: retry the current agent, mark the task as errored, or skip to the next agent
10. `CodeReview` agent performs an independent review of the code, checking for bugs, maintainability concerns, and adherence to project standards
11. If `CodeReview` finds issues -> `AgentKickedBack` -> task sent back to the appropriate earlier agent (Implementation, Design, or Planning) with review findings as context
12. If `CodeReview` passes its own review -> task enters `PendingReview`, diffs fetched via `GET /session/:id/diff`
13. Human reviews in Tab 4 (diff view + comments), approves or requests revisions
14. If human requests revisions -> `HumanRequestedRevisions` -> `CodeReview` agent receives combined feedback (its own findings + human comments) and kicks back to the appropriate agent
15. If human approves -> `HumanApprovedReview` -> `CodeReview` agent prepares commit message -> task enters `Completed`

### OpenCode Client Layer

```rust
// src/opencode/mod.rs

pub struct OpenCodeClient {
    http: reqwest::Client,
    base_url: String,
    auth: Option<(String, String)>,  // (username, password)
}

impl OpenCodeClient {
    /// Create a new session for an agent.
    pub async fn create_session(&self) -> Result<OpenCodeSession>;

    /// Send a prompt to a session (fire-and-forget).
    /// Responses arrive via SSE event stream.
    pub async fn send_prompt_async(&self, session_id: &str, prompt: &str) -> Result<()>;

    /// Send a prompt with structured output (JSON schema) for parseable agent responses.
    pub async fn send_prompt_structured(
        &self, session_id: &str, prompt: &str, schema: &serde_json::Value,
    ) -> Result<OpenCodeMessage>;

    /// Abort an active session.
    pub async fn abort_session(&self, session_id: &str) -> Result<()>;

    /// Fork a session (branch from existing conversation).
    pub async fn fork_session(&self, session_id: &str) -> Result<OpenCodeSession>;

    /// Get diffs produced by a session.
    pub async fn get_session_diffs(&self, session_id: &str) -> Result<Vec<FileDiff>>;

    /// Health check.
    pub async fn health(&self) -> Result<bool>;
}
```

```rust
// src/opencode/events.rs

/// Connects to opencode's SSE stream and maps events to AppMessages.
pub struct EventStreamConsumer {
    tx: mpsc::Sender<AppMessage>,
    // Maps opencode session_id -> (TaskId, AgentKind) for routing
    session_map: Arc<RwLock<HashMap<String, (TaskId, AgentKind)>>>,
}

impl EventStreamConsumer {
    /// Connect to SSE and begin forwarding events.
    /// Runs as a long-lived tokio task.
    pub async fn run(&self, base_url: &str) -> Result<()>;
}
```

### Custom OpenCode Agent Definitions

Each of ClawdMux's 7 pipeline stages is a custom opencode agent defined as a markdown file in `.opencode/agents/clawdmux/`. ClawdMux writes these files at startup if they do not already exist (or if the user runs `clawdmux agent reset`), giving users a sensible default that they can edit by hand.

**Example: `.opencode/agents/clawdmux/implementation.md`**

```markdown
---
description: Implements code changes according to the implementation plan
mode: subagent
model: anthropic/claude-sonnet-4-5
steps: 50
tools:
  read: true
  write: true
  edit: true
  bash: true
permission:
  bash:
    "cargo *": allow
    "git diff *": allow
    "git status": allow
    "*": ask
---
You are the Implementation agent in the ClawdMux pipeline. Your job is to
implement the code changes described in the task's implementation plan.

Follow the plan precisely. Prefer editing existing files over creating new ones.
Write idiomatic, well-tested code. Do not refactor code outside the scope of the task.

When finished, respond with a JSON object matching this schema and nothing else:
{"action":"complete","summary":"<one sentence>"}

If you have a blocking question for the human, respond with:
{"action":"question","question":"<text>","context":"<why you need to know>"}
```

**Agent tool permissions by stage:**

| Agent | read | write | edit | bash | Rationale |
|-------|------|-------|------|------|-----------|
| Intake | yes | no | no | no | Read-only context gathering |
| Design | yes | no | no | no | Read-only analysis |
| Planning | yes | no | no | limited | May run `cargo check` to validate plan feasibility |
| Implementation | yes | yes | yes | yes | Full write access needed |
| CodeQuality | yes | no | yes | limited | May run `cargo clippy`, `cargo fmt` |
| SecurityReview | yes | no | no | no | Read-only audit |
| CodeReview | yes | no | no | limited | May run `git diff` |

The `config/mod.rs` module owns the agent definition lifecycle: generating defaults, detecting if files exist, and exposing the opencode agent name (e.g., `"clawdmux/implementation"`) for each `AgentKind`.

### Agent User Message Composition

The system prompt is embedded in the opencode agent definition file. `prompt_composer.rs` only builds the *user message* sent in the API call body, injecting the runtime context that differs per invocation:

```rust
// src/workflow/prompt_composer.rs

/// Composes the user message sent to opencode for a given agent + task.
/// The system prompt (personality, structured output instructions) lives in
/// the opencode agent definition at .opencode/agents/clawdmux/<agent>.md.
pub fn compose_user_message(
    agent: &AgentKind,
    task: &Task,
    kickback_reason: Option<&str>,
) -> String {
    // Combines:
    // 1. Task description and story context
    // 2. Prior work accumulated so far (design, implementation plan, Q&A history)
    // 3. Kickback context (if this is a retry after a kickback)
}
```

**Structured output schema.** The agent definition instructs each agent to respond with JSON. The schema is the same across all agents:

```json
{
  "action": "complete",
  "summary": "Reviewed task and updated design section with API integration approach.",
  "updates": {
    "design": "...",
    "questions": []
  }
}
```

Or for questions:
```json
{
  "action": "question",
  "question": "What authentication method should the API use?",
  "context": "The design needs to specify OAuth vs API key..."
}
```

Or for kickbacks (only valid for CodeQuality, SecurityReview, CodeReview):
```json
{
  "action": "kickback",
  "target_agent": "implementation",
  "reason": "Found SQL injection vulnerability in the user input handler..."
}
```

This is far more reliable than parsing terminal output for text markers. opencode's built-in JSON schema validation with retries (default: 2 retries) provides an additional reliability layer.

### TUI Layout

```
+-----------------------------------------------------------------------+
| ClawdMux v0.1.0                                   [Task: 1.1 Foo]    |
+-------------------+---------------------------------------------------+
|                   | [Details] [Agent Activity] [Team Status] [Review]  |
| Stories & Tasks   +---------------------------------------------------+
|                   |                                                   |
| > 1. Big Story    |          (Active tab content)                     |
|   [*] 1.1 First   |                                                   |
|   [ ] 1.2 Second  |                                                   |
|                   |                                                   |
| > 2. Other Story  |                                                   |
|   [ ] 2.1 Task A  |                                                   |
+-------------------+---------------------------------------------------+
| Mode: Normal | Agent: Design | Provider: anthropic/claude-sonnet-4-6  |
+-----------------------------------------------------------------------+
```

- Left pane (25%): Story/task tree with collapsible stories
- Right pane (75%): 4 tabs
- Tab 1 (Details): Task markdown (top), supplemental prompt input (middle), Q&A section (bottom)
- Tab 2 (Agent Activity): Streaming text/markdown of agent work, tool execution activity, agent reasoning
- Tab 3 (Team Status): Agent pipeline progress bar + scrollable work log
- Tab 4 (Review): Unified diff view + comment input area

**Tab 2** shows streaming content from SSE message events rather than an embedded terminal, eliminating VT emulation complexity while providing more structured visibility into agent activity.

**Footer** shows the LLM provider/model in use (configurable through opencode).

### Task File Parsing

A custom line-oriented section parser (not a markdown AST library) reads the structured task format using a two-phase approach:

**Phase 1 -- Metadata**: All `Key: Value` lines before the first `##` heading are parsed as metadata fields (`Story`, `Task`, `Status`, `Assigned To`).

**Phase 2 -- Sections**: The remainder of the file is split on `##` headings. Known sections (`## Description`, `## Starting Prompt`, `## Questions`, `## Design`, `## Implementation Plan`, `## Work Log`) are parsed for their internal structure. Unknown sections are preserved verbatim as raw `(heading, content)` pairs and written back unchanged on serialization, ensuring round-trip fidelity for any future or agent-added sections.

File discovery at startup: scan `./tasks/` then `./docs/tasks/` for `*.md` files. File watching via `notify` crate detects external modifications.

### Recommended Technology Stack

| Need | Crate | Rationale |
|------|-------|-----------|
| TUI framework | `ratatui` + `crossterm` | De facto Rust TUI standard, rich widget ecosystem |
| Text input | `tui-textarea` | Multi-line editor widget for prompt/answer/comment fields |
| Async runtime | `tokio` | Industry standard, needed for HTTP + SSE + TUI events |
| HTTP client | `reqwest` | For opencode REST API communication |
| SSE client | `reqwest-eventsource` | For consuming opencode's `/global/event` stream |
| Serialization | `serde` + `serde_json` | For API request/response types |
| Code diffing | `similar` + `git2` | `similar` for local diff, `git2` for git state; also use opencode's `/session/:id/diff` |
| File watching | `notify` | Cross-platform fs event watching for task files |
| Timestamps | `chrono` | Work log timestamps |
| CLI args | `clap` | Standard Rust CLI parsing |
| Logging | `tracing` + `tracing-appender` | Required by CLAUDE.md. Log to file since TUI owns stdout |
| Errors | `thiserror` | Ergonomic derive for error enums |
| State machine | Custom enum | 7-stage pipeline, hand-rolled match logic |
| ~~PTY management~~ | ~~`portable-pty`~~ | **REMOVED: No longer needed** |
| ~~Terminal emulation~~ | ~~`vt100`~~ | **REMOVED: No longer needed** |

### Risk Assessment

| Risk | Severity | Mitigation |
|------|----------|------------|
| **opencode API stability** -- API may change between versions | Medium | Pin opencode version in docs. Generate/maintain Rust types from OpenAPI spec. Isolate all API calls in `opencode/` module. |
| **Structured output reliability** -- LLM may not return valid JSON matching schema | Medium | opencode supports JSON schema validation with retries (default 2). Add fallback: if structured output fails, parse free-text response for keywords. Allow human manual advance. |
| **opencode server lifecycle** -- process crashes, port conflicts, startup timing | Medium | Health check polling with exponential backoff on startup. Automatic restart with configurable retry limit. Clear error messages in TUI if server unreachable. On mid-workflow session errors, the user is presented with a choice: retry, mark as errored, or skip. |
| **Task file format edge cases** -- multi-line fields, special characters | Medium | Comprehensive parser test suite with edge cases. Lenient parsing with sensible defaults. |
| **SSE connection reliability** -- dropped connections, reconnection | Low | `reqwest-eventsource` handles reconnection. Add heartbeat monitoring. Graceful degradation: poll API if SSE fails. |
| **opencode dependency** -- external binary must be installed | Low | `clawdmux init` auto-installs via official curl script with user consent. `clawdmux doctor` verifies prerequisites at any time. |
| **Provider credential exposure** -- API keys stored in config file | Low | Keys live in `~/.config/clawdmux/config.toml` (user-owned, 0600). Never written to project files or opencode config. Passed to opencode process via env vars, not command-line args. |

---

## Verification Plan

1. **Build & lint**: `cargo fmt && cargo build && cargo clippy -- -D warnings` -- zero warnings
2. **Unit tests**: `cargo test` -- each subsystem tested via injected messages
   - Workflow engine: verify forward advancement, kickback validation, question pause/resume
   - Task parser: round-trip tests (parse -> serialize -> parse, assert equality)
   - Prompt composer: verify correct prompt assembly for each agent
   - OpenCode client: mock HTTP server tests for session lifecycle, prompt sending, diff retrieval
3. **Integration test**: Mock opencode server that returns structured JSON responses, verify full pipeline from `StartTask` through `Completed`
4. **Manual TUI test**: Run `cargo run` in a project with sample task files and a running opencode server, verify all 4 tabs render
5. **Server lifecycle test**: Verify ClawdMux spawns opencode server on startup, connects, and cleans up on shutdown

---

## MVP Phasing

- **Phase 1**: TUI shell with task loading, left pane navigation, Tab 1 (task details)
- **Phase 2**: `clawdmux init`, opencode server lifecycle, Tab 2 (agent activity stream)
- **Phase 3**: Agent workflow engine with structured output parsing, Tab 3 (team status)
- **Phase 4**: Tab 4 (code review) with diffs from opencode API

---

## Appendix: OpenCode Research (Feb 2026)

### Current State of OpenCode

- **Active repo**: `anomalyco/opencode` (the original `opencode-ai/opencode` Go repo was archived Sept 2025)
- **Language**: TypeScript (Hono HTTP framework, Bun build system)
- **License**: MIT
- **Architecture**: Client-server. TUI is a client; server exposes OpenAPI 3.1.1 REST API
- **Website**: https://opencode.ai
- **SDK**: `@opencode-ai/sdk` (JS/TS, v1.2.5, generated from OpenAPI spec)

### Key API Endpoints

| Category | Endpoints | Purpose |
|----------|-----------|---------|
| Health | `GET /global/health` | Server status and version |
| Events | `GET /global/event` | SSE stream (40+ event types) |
| Sessions | `POST /session`, `GET /session/:id`, `DELETE /session/:id` | Session CRUD |
| Messages | `POST /session/:id/message`, `POST /session/:id/prompt_async` | Send prompts |
| Commands | `POST /session/:id/command` | Execute opencode commands |
| Diffs | `GET /session/:id/diff` | File changes from session |
| Files | `GET /find`, `GET /file/content` | Search and read files |
| Config | `GET /config`, `PUT /config` | Configuration management |
| Providers | `GET /config/providers` | List LLM providers |
| Agents | `GET /agent` | List available agents |
| Tools | `GET /experimental/tool/ids` | List available tools |
| Auth | `POST /provider/:id/oauth/authorize` | OAuth authorization |

### CLI Commands

| Command | Purpose |
|---------|---------|
| `opencode` | Start TUI (includes embedded server) |
| `opencode serve [--port] [--hostname]` | Headless HTTP server |
| `opencode run --command "..."` | Non-interactive prompt execution |
| `opencode attach [--session]` | Connect TUI to running server |
| `opencode web` | Headless server + web browser UI |
| `opencode acp` | Agent Client Protocol (JSON-RPC over stdio) |

### Supported LLM Providers (partial list)

Anthropic (Claude 3.5-4), OpenAI (GPT-4.1, GPT-4.5, O1/O3), Google (Gemini 2.0-2.5), GitHub Copilot, AWS Bedrock, Groq, Azure OpenAI, Google VertexAI, plus local model support.

### Codebases to Study

1. **opencode** (`anomalyco/opencode`) -- Primary: API contract, session model, event system, agent patterns
2. **gitui** (`extrawurst/gitui`) -- Complex ratatui app with git integration and diff display
3. **bottom** (`ClementTsang/bottom`) -- Complex ratatui app with tabs, splits, async data
4. **tenere** (`pythops/tenere`) -- Rust TUI + LLM streaming integration
