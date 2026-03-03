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
- **Unified TUI**: Left pane task navigation + 5-tab right pane (details, questions, activity, team status, review)

### What We No Longer Need

- ~~`portable-pty`~~ -- No PTY management; communication is HTTP
- ~~`vt100`~~ -- No terminal emulation; responses are structured text/JSON
- ~~`[CLAWDMUX:*]` signal markers~~ -- Use structured output (JSON schema) via opencode's API
- ~~Custom VT rendering in Tab 2~~ -- Display streaming markdown/text instead
- ~~Flat personality `.txt` files~~ -- Agent system prompts are native opencode agent definitions in `.opencode/agents/clawdmux/`

---

## Architecture: Backend-Abstracted Event-Driven Agent Client

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
    |  TUI Layer |  |  Workflow    |  |  AgentBackend |
    |  (ratatui) |  |  Engine     |  |  (trait)      |
    +-----+------+  +------+------+  +-------+-------+
          |                |                  |
          |         +------v------+    +------v------+  +------v------+
          |         |  Task Store |    |  OpenCode   |  |   Kiro-CLI  |
          |         +-------------+    |  (HTTP+SSE) |  |  (ACP/RPC)  |
          |                            +-------------+  +-------------+
          +------- async mpsc channels -------+
```

ClawdMux is a single Rust binary with internal async subsystems communicating via `mpsc` channels. The **AgentBackend trait** decouples the application from any specific AI coding assistant, enabling pluggable backends. Two backends are implemented:

- **OpenCodeBackend** (default): communicates with an opencode server over HTTP REST + Server-Sent Events (SSE).
- **KiroBackend**: communicates with kiro-cli via the Agent Client Protocol (ACP) -- JSON-RPC 2.0 over stdin/stdout, one process per agent stage.

Backend selection is configured in `.clawdmux/config.toml`:
```toml
backend = "opencode"  # or "kiro"

[kiro]
# binary = "/usr/local/bin/kiro"  # optional path, defaults to PATH lookup
```

### KiroBackend: Agent Client Protocol (ACP)

When `backend = "kiro"` is configured, ClawdMux communicates with kiro-cli via ACP
(JSON-RPC 2.0 over newline-delimited stdin/stdout). One fresh kiro-cli process is
spawned per agent stage to avoid context compaction across pipeline stages.

**Process lifecycle per agent stage:**
1. Spawn `kiro --acp --agent clawdmux-<stage>` with piped stdin/stdout
2. Send `initialize` request; receive capabilities; send `initialized` notification
3. Send `session/new`; receive `sessionId`
4. Send `session/prompt` notification with task prompt
5. Event loop translates ACP notifications to `AppMessage` variants
6. On `turn_end`, process exits; next stage spawns a fresh process

**ACP notifications handled:**
- `agent_message_chunk` -> `AppMessage::StreamingUpdate` (accumulated text)
- `tool_call` / `tool_call_update` -> `AppMessage::ToolActivity`
- `turn_end` -> `AppMessage::SessionCompleted` or `SessionError`
- `session/error` -> `AppMessage::SessionError`
- `session/request_permission` (bidirectional) -> `AppMessage::PermissionAsked`

**Kiro agent configs** are scaffolded into `.kiro/agents/clawdmux-*.json` during
`clawdmux init`. Tool permissions are scoped per agent role:
- Read-only (Intake, Design, SecurityReview): `["read", "search", "think"]`
- Read + execute (Planning, CodeQuality, CodeReview): `["read", "execute", "search", "think"]`
- Full access (Implementation): `["read", "edit", "delete", "execute", "search", "think"]`

**Diff support**: Run `git diff HEAD` after `SessionCompleted` (no ACP diff endpoint).

**Git commits**: Performed directly via `git add` + `git commit` (no agent needed).

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
Provider: [1] Anthropic  [2] OpenAI  [3] Google  [4] OpenRouter
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

# [provider.openrouter]
# api_key = "sk-or-..."
# default_model = "openrouter/openrouter/auto"
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
  created tasks/tasks.md
  created tasks/1.1.md
  created tasks/1.2.md
  created tasks/2.1.md
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
| 1 | **Per-prompt explicit model** | `model: { providerID, modelID }` passed in the `POST /session/:id/prompt_async` request body — ClawdMux always sets this from agent frontmatter or the global default |
| 2 | **Per-agent default** | `model:` field in `.opencode/agents/clawdmux/<agent>.md` frontmatter |
| 3 | **Global ClawdMux default** | `[provider.<name>].default_model` in `~/.config/clawdmux/config.toml`; used as the fallback in agent definition files via a `{global_default}` placeholder resolved at agent file generation time |

#### Per-Turn Model Selection (implementation)

At startup, `build_agent_model_map()` in `config/init.rs` reads the embedded agent `.md` files, extracts the `model:` frontmatter field from each, and parses it into a `HashMap<AgentKind, ModelId>`. The global default model is read from the active provider's `default_model` field via `GlobalConfig::default_model_id()`.

Both are passed to `App::new()` and stored as `agent_models` and `default_model`. On every `CreateSession` and `SendPrompt` message, the handler looks up the agent's `ModelId` from `agent_models` (falling back to `default_model`) and passes it to `OpenCodeClient::send_prompt_async()`, which serializes it as `"model": { "providerID": "...", "modelID": "..." }` in the JSON body.

The `ModelId::parse(s)` function splits on the first `/` only, matching OpenCode's `parseModel` behavior: `"openrouter/anthropic/claude-sonnet-4.6"` → `providerID: "openrouter"`, `modelID: "anthropic/claude-sonnet-4.6"`.

Commit and fix sessions (which have no dedicated agent) use `default_model` directly.

This means:
- Agents use their own model by default (e.g., Implementation uses a capable model, Intake uses a lighter/faster one)
- The explicit per-turn model in the request body takes the highest OpenCode resolution priority, overriding agent frontmatter and session history
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
        task_details.rs        # Tab 0: task markdown and prompt input
        questions.rs           # Tab 1: question/answer display and navigation
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
    ResumeTask { task_id: TaskId },
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
7. On question -> `AgentAskedQuestion` -> pauses workflow, shows question in Tab 1 (Questions)
8. On kickback -> `AgentKickedBack` -> workflow aborts session, restarts at target agent with kickback context
9. On session error -> `SessionError` -> workflow pauses and presents the user with a choice: retry the current agent, mark the task as errored, or skip to the next agent
10. `CodeReview` agent performs an independent review of the code, checking for bugs, maintainability concerns, and adherence to project standards
11. If `CodeReview` finds issues -> `AgentKickedBack` -> task sent back to the appropriate earlier agent (Implementation, Design, or Planning) with review findings as context
12. If `CodeReview` passes its own review -> task enters `PendingReview`, diffs fetched via `GET /session/:id/diff`
13. Human reviews in Tab 4 (diff view + comments), approves or requests revisions
14. If human requests revisions -> `HumanRequestedRevisions` -> `CodeReview` agent receives combined feedback (its own findings + human comments) and kicks back to the appropriate agent
15. If human approves -> `[a]` opens the commit dialog (pre-filled with CodeReview agent's proposed commit message and file list) -> human can edit message -> `[Enter]` emits `HumanApprovedCommit` -> an opencode session runs `git add -A && git commit` -> on success: `CommitCompleted` -> task enters `Completed`; on failure: `CommitFailed` -> task stays in `PendingReview` for retry

### Human Approval Gate

Between each agent handoff, ClawdMux can pause and require explicit human approval before starting the next agent. This allows the user to inspect intermediate results at each pipeline stage without having to race against automatic progression.

**Behavior:**
- When an agent completes (via `AgentCompleted`, `AgentKickedBack`, or `SessionCompleted` fallback), the workflow engine transitions to `WorkflowPhase::AwaitingApproval { next_agent, context }` instead of immediately emitting `CreateSession`.
- The Team Status tab (Tab 5) shows `"Awaiting approval to start <Agent Name>"` in the Phase panel.
- The next agent is highlighted in **magenta** in the pipeline bar.
- The footer shows `[n] next agent` as an available action.
- When the human presses `n` on Tab 5, a `HumanApprovedTransition` message is dispatched. The workflow engine transitions to `Running` and emits `CreateSession` for the pending agent.

**Configuration:**
The gate is **on by default**. Disable it by adding the following to `.clawdmux/config.toml`:

```toml
[workflow]
approval_gate = false
```

When disabled, the pipeline advances automatically without pausing between agents (original behavior).

### Resume Interrupted Tasks

When a task is interrupted — by a session error or an app crash/restart — it is left in `InProgress` with no active pipeline. Pressing **Enter** on an `InProgress` task in Tab 0 re-enters the pipeline at the correct agent without discarding history.

**Agent resolution priority (highest first):**
1. **Workflow engine `Errored` state** — if the engine has an `Errored` entry for the task, use `current_agent` (the agent that was running when the error occurred).
2. **Task's `assigned_to` field** — if no workflow state exists (crash scenario), use the persisted agent, excluding `Human`.
3. **Fallback** — `AgentKind::Intake`.

**Implementation:**
- `AppMessage::ResumeTask { task_id }` — new workflow command dispatched by the Enter key handler on `InProgress` tasks.
- `WorkflowEngine::resume(task_id, agent)` — creates a fresh `WorkflowState` in `Running` phase at the resolved agent, overwriting any prior state, and emits `CreateSession` with `context: Some("Task resumed")`.
- `App::handle_message(ResumeTask)` — resolves the agent, updates `assigned_to`, logs the resumption to the work log, calls `resume()`, and appends `TaskUpdated`.
- Footer hint shows `[Enter] resume` when an `InProgress` (non-malformed) task is selected on Tab 0.

**Implementation:**
- `WorkflowPhase::AwaitingApproval { next_agent: AgentKind, context: Option<String> }` -- new phase variant
- `AppMessage::HumanApprovedTransition { task_id: TaskId }` -- new message dispatched by Tab 5 `n` key
- `WorkflowEngine::new(approval_gate_enabled: bool)` -- gate flag wired from `AppConfig::workflow.approval_gate`
- `WorkflowConfig` struct in `src/config/mod.rs` with `approval_gate: bool` (default: `true`)

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
- Tab 0 (Details): Task markdown (top), supplemental prompt input (bottom); questions moved to Tab 1
- Tab 1 (Questions): One question at a time with answer textarea; tab title shows `*` when unanswered questions exist
- Tab 2 (Agent Activity): Streaming text/markdown of agent work, tool execution activity, agent reasoning
- Tab 3 (Team Status): Agent pipeline progress bar + scrollable work log
- Tab 4 (Review): Unified diff view with colored +/-/space prefixed lines (green/red/dim). Press `r` to enter review mode: Up/Down move the cursor line; PageUp/PageDown navigate between files; Space marks a line-range selection (git diff hunk coordinates); Enter in comment-input mode attaches an inline comment after the selected range; Esc cancels and exits review mode. `a` approves, `R` (Shift+R) emits HumanRequestedRevisions with all accumulated inline comments formatted as `path:start-end: text`.

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

### Implementation Status

- Task 4.1 (TUI Bootstrap & Event Loop): COMPLETED. `cargo run` opens a full-screen ratatui terminal with a placeholder layout (header, left/right panes, footer). Pressing `q` or `Ctrl-C` exits cleanly and restores the terminal. Logging redirected to `clawdmux.log` to avoid corrupting the TUI display. `App` struct, `tui::draw()`, `tui::handle_input()`, and `tui::layout::render_layout()` are all implemented with unit tests.
- Task 4.2 (Task List Widget): COMPLETED. Left pane now renders a real collapsible story/task tree via `tui::task_list`. `TaskListState` tracks expansion, selection index, and the flattened item list. Arrow keys (`Up`/`Down`/`j`/`k`) navigate the list; `Enter`/`Space` toggles story expansion or selects a task; `Tab` cycles the active right-pane tab. Status icons use distinct ratatui colors (`[ ]` Open, `[*]` InProgress, `[x]` Completed, `[!]` Abandoned, `[?]` PendingReview). `handle_input` now takes `&mut App` to mutate navigation state directly. `App` gains `task_list_state: TaskListState`, initialized via `refresh()` in `App::new()`. 103 tests passing.
- Task 6.1 (Workflow State Machine): COMPLETED. `WorkflowEngine` pure state machine implemented in `src/workflow/transitions.rs`. `WorkflowPhase` (`Idle`, `Running`, `AwaitingAnswer`, `PendingReview`, `Completed`, `Errored`) and `WorkflowState` drive per-task lifecycle. `process(AppMessage) -> Vec<AppMessage>` handles all workflow transitions: `StartTask`, `SessionCreated`, `SessionCompleted`, `AgentCompleted`, `AgentKickedBack` (with kickback validation), `AgentAskedQuestion`, `HumanAnswered`, `HumanApprovedReview`, `HumanRequestedRevisions`, `SessionError`. Placeholder prompt helper marked `//TODO: Wire compose_user_message`. TOCTOU race in `EventStreamConsumer::handle_event()` fixed with 3-attempt/50ms retry on `SessionCreated`. 12 unit tests added (185 total). Re-exported via `workflow::{WorkflowEngine, WorkflowPhase, WorkflowState}`.
- Task 6.2 (Prompt Composer): COMPLETED. `compose_user_message(agent, task, kickback_reason) -> String` implemented in `src/workflow/prompt_composer.rs`. Builds per-agent user messages with sections gated by pipeline index: Task Context and Description (all agents); Prior Q&A (Design+, index >= 1); Design, Implementation Plan, Work Log last 10 entries (Planning+, index >= 2); Kickback Context (when reason provided); Your Role (all agents). Private section builders each return `Option<String>` and are omitted when content is empty. Work log truncation shows a `(showing last 10 of N entries)` note when truncated. 6 unit tests added (195 total).
- Task 8.1 (App::handle_message Dispatcher): COMPLETED. All 25 `AppMessage` variants are now fully dispatched in `App::handle_message`. Workflow messages (`StartTask`, `AgentCompleted`, `AgentKickedBack`, `AgentAskedQuestion`, `HumanAnswered`, `HumanApprovedReview`, `HumanRequestedRevisions`, `SessionCreated`, `SessionCompleted`, `SessionError`) forward to `WorkflowEngine::process`. Async session ops (`CreateSession`, `SendPrompt`, `AbortSession`) spawn tokio tasks that call the `OpenCodeClient` and route results back through `async_tx`. Task persistence (`TaskUpdated`, `TaskFileChanged`) calls `TaskStore::persist`/`reload`. `DiffReady` stores diffs in the new `Tab4State`. `Tick` drains `pending_messages`. `App` gained 5 fields: `opencode_client: Option<Arc<OpenCodeClient>>`, `session_map: SessionMap`, `async_tx: mpsc::Sender<AppMessage>`, `pending_messages: Vec<AppMessage>`, `tab4_state: Tab4State`. `Tab4State` implemented in `src/tui/tabs/code_review.rs` with `set_diffs`, `diffs_for`, `set_displayed_task`. `EventStreamConsumer` spawned in `main.rs` when server is available. Channel widened to 64 slots and renamed from `fix_tx`/`fix_rx` to `async_tx`/`async_rx`. All test helper calls updated to pass new `App::new()` parameters. 5 required tests added. 237 tests total, all passing.
- Task 9.1 (Code Review Tab): COMPLETED. Full `Tab4State` implementation in `src/tui/tabs/code_review.rs`. Expanded `Tab4State` with `selected_file: usize`, `diff_scroll: u16`, `comment_input: TextArea`, `comments: Vec<String>`, `comment_focused: bool`. Added methods: `reset_for_diffs`, `select_prev_file`, `select_next_file`, `scroll_up`, `scroll_down`, `set_comment_focused`, `set_comment_unfocused`, `submit_comment`, `take_comments`. `render()` shows a file carousel header `< N/M: path [status] >`, scrollable diff view with colored +/-/space prefixed hunk lines, comment textarea, and hint bar with accumulated comment count. `DiffReady` handler now also switches to Tab 4 (`active_tab = 4`) and resets navigation. Keybindings on Tab 4: Left/Right navigate files, PgUp/PgDn scroll diff, `c` focuses comment textarea, Esc unfocuses, Enter appends comment to list, `a` emits `HumanApprovedReview`, `r` emits `HumanRequestedRevisions` with accumulated comments. `FocusedInput` enum added to `tui/mod.rs` to consolidate prompt/answer/comment focus state for the footer hint. `footer_hint_text` refactored from 8 params to 6 (resolving clippy too-many-arguments). 13 new tests in code_review.rs, 7 new tests in tui/mod.rs. 378 tests total, all passing.
- Issue #32 (Commit Workflow): COMPLETED. `[a]` on Tabs 6 and 7 now opens a centered commit dialog instead of immediately completing the task. The dialog shows changed files (colored `[A]`/`[M]`/`[D]` prefix, up to 10) and an editable textarea pre-filled with the CodeReview agent's proposed commit message (stored in `Tab4State::commit_messages` when `AgentResponse::Complete` includes a `commit_message`). `[Enter]` emits `HumanApprovedCommit`; `[Esc]` closes the dialog. On confirmation, `App` spawns an opencode session that runs `git add -A && git commit -m '<message>'`. The session is tracked in `App::pending_commit_sessions` (via `RegisterCommitSession`) so `SessionCompleted` routes to `CommitCompleted` (transitions task to `Completed`) and `SessionError` routes to `CommitFailed` (task stays in `PendingReview` for retry). New messages: `HumanApprovedCommit`, `CommitCompleted`, `CommitFailed`, `RegisterCommitSession`. New struct: `CommitDialogState` (in `app.rs`). 8 new unit tests added (580 total, all passing).
- PR #33 review fixes (bug-fix-3): Multiple fixes addressing code review feedback. **Permission dialog key handling**: all keystrokes are now consumed when the permission dialog is active (the `has_pending` block ends with `return None` to prevent keys from leaking to tab-level handlers). **Permission dialog scrolling**: `Down` scroll is bounded by the wrapped line count of the pattern list (computed via `Paragraph::line_count`); `Up/Down` clamping lives in the input handler so the render function uses `permission_scroll` directly. **Hint text wrapping**: the key-hint `Paragraph` in the permission dialog now uses `Wrap { trim: false }` and the hint area is 2 rows; dialog height increased from 9 to 10. **Reject-with-response grammar**: "lets" corrected to "let's". **SendPrompt ordering**: `PermissionResolved` now computes the steering prompt before spawning `resolve_permission`; the `SendPrompt` is dispatched inside the spawn *after* the API call succeeds, eliminating the race where the agent received guidance before the permission was acknowledged. **Parse failure session refresh**: when `SessionCompleted` response text cannot be parsed, the workflow's stale `session_id` is now cleared via `WorkflowEngine::reset_session_id` and a new bare session is created asynchronously; the resulting `SessionCreated` registers the fresh session so `[p]` steering works correctly. `WorkflowEngine` gained a new `reset_session_id(&mut self, task_id)` method. **Reject guard**: the steering prompt is only sent when `response == "reject"`, preventing unexpected agent messages on approve/always resolutions. **PgUp/PgDn task switching**: on Tab 0 (Details), `PgUp`/`PgDn` navigate to the previous/next task in the list without scrolling the description pane. **Known limitation**: the queued-prompt drain in `SessionCompleted` (lines 714-722) runs before `parse_response`; if a queued prompt exists when a session with an unparseable response completes, the raw response is silently discarded. This is a pre-existing issue to be addressed in a follow-up.

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
