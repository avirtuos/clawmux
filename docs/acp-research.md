# ACP Research: Agent Client Protocol vs OpenCode for ClawMux

**Date:** 2026-03-02
**Related Issue:** [#36 - Add kiro-cli as an alternative agent backend via ACP](https://github.com/avirtuos/clawmux/issues/36)

## Executive Summary

The Agent Client Protocol (ACP) is a standardized JSON-RPC 2.0 protocol for editor-to-agent communication, jointly governed by Zed Industries and JetBrains, with 40+ client implementations and 50+ compatible agents. It represents a fundamentally different architectural model from OpenCode's HTTP REST + SSE approach, and in many areas provides richer capabilities. This document presents a unified analysis of ACP from three angles -- core protocol specification, advanced/future features, and kiro-cli's concrete implementation -- and distills them into a single perspective on how ClawMux should approach ACP as a backend.

**Key conclusion:** ACP is not just an alternative transport for what we already do with OpenCode. It introduces capabilities that would meaningfully improve ClawMux's orchestration model -- structured plans, inline diffs, richer permissions, dynamic configuration, and a proxy/conductor architecture that maps naturally to our pipeline. However, ACP also has gaps (no turn limits, no token tracking yet, no global event multiplexing) that require orchestrator-level solutions.

---

## 1. Protocol Architecture Comparison

### 1.1 Communication Model

| Aspect | OpenCode | ACP |
|--------|----------|-----|
| Transport | HTTP REST + SSE (network-based) | stdio over subprocess (local) |
| Direction | Unidirectional: client calls server, server pushes SSE | **Bidirectional**: both sides can initiate requests |
| Session multiplexing | Single server handles all sessions; global event stream | One agent process per session (or internal multiplexing) |
| File system access | Agent has direct FS access | Agent **delegates** FS operations to client |
| Command execution | Agent runs commands directly | Agent **delegates** terminal operations to client |
| Streaming | SSE (Server-Sent Events) | JSON-RPC notifications over stdio |
| Diff retrieval | Dedicated `GET /session/:id/diff` endpoint | **Inline** in tool call content as `{ type: "diff" }` blocks |

The most significant architectural difference is **bidirectionality**. In ACP, the agent can call methods on the client -- requesting file reads, file writes, terminal creation, and permission approval. This means ClawMux would act as both a JSON-RPC **client** (sending prompts, creating sessions) and a JSON-RPC **server** (handling agent requests for FS, terminal, and permissions). This is a richer model than OpenCode where ClawMux is purely a consumer.

### 1.2 Session Lifecycle

ACP defines a formal 6-phase lifecycle:

```
1. INITIALIZE     -- Version + capability negotiation (required)
2. AUTHENTICATE   -- If agent requires auth (conditional)
3. SESSION SETUP  -- session/new or session/load (required)
4. PROMPT TURNS   -- Repeatable prompt/response cycles with streaming updates
5. CANCELLATION   -- session/cancel notification (at any time during phase 4)
6. CONFIG CHANGE  -- session/set_config_option (at any time after phase 3)
```

OpenCode has no formal initialization handshake -- ClawMux just starts calling REST endpoints. ACP's capability negotiation at startup means we can detect at runtime which features a specific agent supports, rather than hardcoding assumptions.

### 1.3 Capability Negotiation

During `initialize`, both sides advertise what they support:

**Client advertises to agent:**
- `fs.readTextFile` / `fs.writeTextFile` -- whether the client can handle file operations
- `terminal` -- whether the client can run commands

**Agent advertises to client:**
- `loadSession` -- session resume support
- `promptCapabilities.image` / `.audio` / `.embeddedContext` -- content types accepted
- `mcpCapabilities.http` / `.sse` -- MCP server transport support

OpenCode has no equivalent. ClawMux currently assumes all OpenCode features are available.

---

## 2. Feature-by-Feature Comparison

### 2.1 Where ACP Exceeds OpenCode

| Feature | ACP | OpenCode | Impact on ClawMux |
|---------|-----|----------|-------------------|
| **Structured plans** | `plan` updates with priority (high/medium/low) and status (pending/in_progress/completed) per entry | None | Native plan visualization in the Plan tab; agents report structured progress |
| **Inline diffs** | `{ type: "diff", path, oldText?, newText }` in tool call content | Requires polling `GET /session/:id/diff` | Real-time diff display without polling; diffs arrive with the tool call that produced them |
| **Tool kind categories** | 9 kinds: `read`, `edit`, `delete`, `move`, `search`, `execute`, `think`, `fetch`, `other` | Unstructured tool names | Richer Agent Activity tab -- group/filter by kind, show icons per category. **Note:** these are ACP protocol-level kind labels, NOT kiro-cli built-in tool names (see section 3.1). |
| **Tool call locations** | `locations: [{ path, line? }]` on each tool call | Not available | Show affected file:line in activity view |
| **Permission model** | 4 options: `allow_once`, `allow_always`, `reject_once`, `reject_always` with structured `PermissionOption` objects | 3 options: `once`, `always`, `reject` as strings | `reject_always` enables persistent rejection rules; structured options allow custom labels |
| **Session modes** | Dynamic modes with `session/set_config_option`; categories for `mode`, `model`, `thought_level` | None | Switch between ask/architect/code modes per pipeline stage; adjust model/reasoning dynamically |
| **Slash commands** | Agent advertises available commands via `available_commands_update` | None | Expose agent-specific commands in the TUI |
| **MCP server passthrough** | Client passes MCP server configs at session creation | Not available | Agents can use project-specific MCP tools defined in ClawMux config |
| **FS/terminal delegation** | Agent requests client to perform FS/terminal ops | Agent does everything directly | ClawMux controls what agents can read/write/execute -- centralized security boundary |
| **Rich content types** | Text, images, audio, embedded resources, resource links with annotations | Text only (message parts) | Multimodal content in agent activity; resource links for file references |
| **Session load/replay** | `session/load` replays conversation history via notifications | No equivalent | Resume interrupted pipeline stages with full context |
| **Formal extensibility** | `_meta` fields everywhere; `_`-prefixed custom methods; vendor capabilities | None | Define `_clawmux/` extensions for task context injection, pipeline coordination |
| **W3C trace context** | Built-in `traceparent`/`tracestate`/`baggage` in `_meta` | None | Distributed tracing across ClawMux and agents |

### 2.2 Where OpenCode Exceeds ACP (and Mitigation Strategies)

| OpenCode Feature | ACP Status | Mitigation for ClawMux |
|-----------------|------------|------------------------|
| **Global event stream** | No equivalent -- ACP is per-process | ClawMux fans-in from multiple agent process stdout streams into the single `AppMessage` channel (already the plan) |
| **Session status polling** (`idle`/`busy`/`retry`) | No equivalent | Track status locally: `Running` when prompt sent, `Completed` on turn end, `Errored` on process failure |
| **Token usage reporting** | RFD proposed: `usage_update` with input/output/thought/cached tokens + cost tracking | Implement local token counting if available via `_meta`; zero-fill otherwise. Monitor the RFD -- this will likely be standardized |
| **Session forking** | RFD proposed: `session/fork` | Not critical for current pipeline model (sequential agents). Monitor RFD for future parallel agent support |
| **Question/answer flow** (`question.asked` + reply) | **Elicitation RFD** proposed: structured forms with JSON Schema validation, URL-based auth flows | Near-term: parse questions from agent response JSON (existing `parse_response()`). Long-term: adopt elicitation when standardized -- it's significantly richer than OpenCode's Q&A |
| **Agent routing** (named agent in prompt) | `session/set_mode` + `session/set_config_option` | Use modes to switch agents; or spawn separate processes per agent |
| **Turn/step limits** (`steps` field in agent .md) | **No ACP or kiro equivalent** | **Must implement at orchestrator level**: count `TurnEnd` events per agent, send `session/cancel` when limit reached |
| **Parent/child sessions** | No equivalent | Not needed if using one-process-per-agent model |
| **Retry semantics** (`Retry { attempt, message, next }`) | No equivalent | Handle at orchestrator: detect errors, implement retry with backoff |
| **Health check** | Not needed | Process lifecycle IS the health check (stdio transport) |
| **Message history retrieval** | `session/load` replays but no random-access | Not critical for current use case |

### 2.3 Kiro-Specific ACP Extensions

Kiro extends standard ACP with `_kiro.dev/` prefixed methods:

| Extension | Purpose | Relevance |
|-----------|---------|-----------|
| `_kiro.dev/commands/execute` | Execute slash commands | Could invoke `/agent swap` for mode switching |
| `_kiro.dev/commands/options` | Autocomplete suggestions | Low -- ClawMux manages the pipeline |
| `_kiro.dev/commands/available` | Available command list | Low |
| `_kiro.dev/mcp/oauth_request` | OAuth flows for MCP servers | Useful if agents need authenticated API access |
| `_kiro.dev/mcp/server_initialized` | MCP tool availability notification | Useful for verifying agent tool setup |
| `_kiro.dev/compaction/status` | Context compaction progress | Monitor for context window management |
| `_kiro.dev/clear/status` | Conversation clearing | Low |
| `_session/terminate` | Subagent termination | Useful for cleanup |

---

## 3. Kiro Custom Agent Configuration

### 3.1 Config Format

Kiro uses JSON config files (not YAML/TOML), stored at:
- Global: `~/.kiro/agents/<name>.json`
- Workspace: `.kiro/agents/<name>.json` (takes precedence)

> **Important: ACP tool kind names vs. kiro-cli built-in tool names**
>
> The ACP protocol defines 9 abstract tool *kind* labels (`read`, `edit`, `delete`, `move`,
> `search`, `execute`, `think`, `fetch`, `other`) used in tool call notifications to categorize
> what a tool does. These are NOT the names used in kiro agent JSON configs.
>
> The `tools` and `allowedTools` arrays in kiro config files use kiro-cli's actual **built-in
> tool names**: `read`, `write`, `glob`, `grep`, `shell`, `code`, `thinking`. Kiro silently
> ignores any unrecognized tool name, so using ACP kind names (e.g. `"edit"`, `"execute"`,
> `"search"`, `"think"`) in these arrays will NOT enable the corresponding capabilities.

Complete schema:

```json
{
  "name": "clawmux-intake",
  "description": "Reviews task file and clarifies requirements",
  "prompt": "You are the Intake Agent...",
  "model": "claude-sonnet-4-6",
  "tools": ["read"],
  "allowedTools": ["read"],
  "toolsSettings": {
    "read": {
      "allowedPaths": ["tasks/**", "docs/**"]
    },
    "bash": {
      "allowedCommands": ["cargo test", "cargo build"],
      "deniedCommands": ["rm -rf"]
    }
  },
  "resources": [
    "file://CLAUDE.md",
    "file://.kiro/steering/**/*.md"
  ],
  "mcpServers": {
    "my-server": {
      "command": "npx",
      "args": ["-y", "my-mcp-server"],
      "autoApprove": ["read_*"]
    }
  },
  "hooks": {
    "stop": [{ "command": "notify-clawmux pipeline-advance" }],
    "preToolUse": [{ "command": "check-allowed", "matcher": "bash" }]
  },
  "env": { "MY_VAR": "value" },
  "welcomeMessage": "Starting intake review..."
}
```

### 3.2 Three-Layer Permission Model

Kiro uses a three-layer system that is significantly more granular than OpenCode's per-tool boolean:

1. **`tools`** -- Declares which tools are *available* to the agent
2. **`allowedTools`** -- Subset that is *pre-approved* (no user confirmation needed)
3. **`toolsSettings`** -- Granular restrictions per tool:
   - `allowedPaths` / `deniedPaths` (for file operations)
   - `allowedCommands` / `deniedCommands` (for bash)
   - `allowedServices` (for network operations)
   - `availableAgents` / `trustedAgents` (for subagent operations)

**Gotcha**: Tools must appear in BOTH `tools` AND `allowedTools` for auto-approval. Putting a tool in `allowedTools` overrides its `toolsSettings` restrictions.

Compared to OpenCode's agent definition format:
```yaml
# OpenCode (.md with YAML frontmatter)
tools:
  read: true
  write: false
  bash: false
```

Kiro's model allows per-path and per-command restrictions -- e.g., an Implementation agent could have `bash` available but restricted to `cargo *` commands only.

### 3.3 Hook System

Five lifecycle hooks that fire during agent execution:

| Hook | Trigger | Exit Code Semantics |
|------|---------|-------------------|
| `agentSpawn` | Agent process starts | 0 = success |
| `userPromptSubmit` | Before prompt is sent | 0 = proceed, non-0 = transform |
| `preToolUse` | Before tool execution | 0 = allow, **2 = block**, other = warn |
| `postToolUse` | After tool execution | 0 = success |
| `stop` | Agent turn completes | 0 = success |

Hooks receive JSON context via stdin and can output transformation instructions. This is powerful for orchestration:
- `stop` hook could notify ClawMux to advance the pipeline
- `preToolUse` hook could enforce additional security constraints
- `agentSpawn` hook could inject task context

### 3.4 Comparison: OpenCode vs Kiro Agent Definitions

| Aspect | OpenCode | Kiro |
|--------|----------|------|
| Format | Markdown + YAML frontmatter | JSON |
| Location | `.opencode/agents/clawmux/*.md` | `.kiro/agents/*.json` |
| System prompt | Markdown body | `prompt` field (inline or `file://` URI) |
| Model | `model` in frontmatter | `model` field |
| Turn limits | `steps` field | **Not available** |
| Tool permissions | Boolean per-tool | Three-layer with path/command restrictions |
| Context injection | Manual in prompt | `resources` field with glob patterns |
| Hooks | None | 5 lifecycle hooks |
| MCP servers | None (global only) | Per-agent MCP server definitions |

---

## 4. Advanced ACP Features and RFDs

### 4.1 ACP Proxy/Conductor Architecture

The most architecturally significant finding: ACP defines a **proxy chain** model where intermediaries sit between client and agent:

```
Client -> Conductor -> Proxy1 -> ... -> Agent
```

Proxies/conductors can:
- Inject context into prompts
- Filter/transform agent responses
- Coordinate tools across agents
- Manage multi-agent workflows

**ClawMux is naturally an ACP Conductor.** Rather than just wrapping agents, it could implement the conductor protocol, gaining:
- Standard proxy chain composition (e.g., ClawMux -> security-proxy -> agent)
- Context injection via the standardized mechanism
- Tool coordination across pipeline stages
- Compatibility with any ACP-compliant agent (not just kiro)

A reference implementation exists: `sacp-conductor` Rust crate.

### 4.2 Elicitation (Proposed RFD)

Structured user input that goes far beyond OpenCode's simple Q&A:
- **Form mode**: JSON Schema-defined forms with string/number/boolean/enum fields and validation
- **URL mode**: External OAuth/credential flows
- Three response actions: accept, decline, cancel

This would let agents present structured forms (e.g., "Choose deployment target: staging/production" with validation) rather than freeform text questions.

### 4.3 MCP-over-ACP (Proposed RFD)

MCP servers communicating through ACP channels:
- `mcp/connect`, `mcp/message`, `mcp/disconnect`
- Client-injected tools without separate MCP processes
- Transparent bridging for agents that don't natively support it

This would let ClawMux provide custom tools to agents (e.g., a `clawmux-context` MCP tool that serves task/story information) without running separate processes.

### 4.4 Usage Tracking (Proposed RFD)

Per-turn token counts:
- `total`, `input`, `output`, `thought`, `cached_read`, `cached_write` tokens
- Session-level context window tracking (used/size with percentage)
- Cumulative cost tracking (amount + ISO 4217 currency)
- Recommended UI thresholds at 75%/90%/95% context usage

This would fill the token tracking gap and add cost monitoring that OpenCode doesn't provide.

### 4.5 Session Operations Suite (Proposed RFDs)

- `session/list` -- Paginated session enumeration
- `session/stop` -- Cancel + free resources without killing process
- `session/resume` -- Resume without replaying messages
- `session/delete` -- Soft/hard delete
- `session/fork` -- Branch conversations

### 4.6 Telemetry (Proposed RFD)

OpenTelemetry-based agent telemetry:
- Client runs local OTLP receiver
- Injects env vars when spawning agent
- Captures spans, metrics, logs, errors
- Out-of-band via HTTP (not through ACP stdio)

---

## 5. Rust SDK Maturity

The ACP Rust SDK (`agent-client-protocol` crate, being rewritten as `sacp` v1.0) is production-grade -- it powers Zed editor. Key features:

- Builder pattern for client/agent construction
- Directional link types (`ClientToAgent`, `ProxyToConductor`)
- Component trait for handler chains
- Session builders with MCP injection
- Custom message derive macros
- **Proxy/conductor support** built-in
- Published as `sacp` crates

This means ClawMux could potentially use the official Rust SDK rather than building JSON-RPC transport from scratch. The SDK handles framing, capability negotiation, and message routing.

---

## 6. Critical Gaps and Gotchas

### 6.1 No Turn/Step Limits

Neither ACP nor kiro-cli provide a way to limit how many turns an agent takes. OpenCode has `steps: 20` in agent definitions. ClawMux must implement this at the orchestrator level by counting turn completions and sending `session/cancel` when the limit is reached.

### 6.2 No Token Usage (Yet)

The `usage_update` RFD is proposed but not yet standardized. Until then, token tracking shows zeros for ACP agents. Kiro may expose this via `_meta` or kiro-specific extensions -- needs runtime testing.

### 6.3 Kiro Authentication

Kiro requires interactive login before first use -- there is no API key authentication. Auth tokens persist in `~/.kiro/`. This means the first run of ClawMux with kiro backend may require manual kiro login.

### 6.4 Auto-Compaction

Kiro automatically compacts conversation context when it grows large. This can lose context mid-pipeline. ClawMux should monitor `_kiro.dev/compaction/status` and consider creating fresh sessions for later pipeline stages rather than reusing a single session with accumulated context.

### 6.5 MCP Tool Naming

Kiro formats MCP tool names as `@server-name___tool-name` (with triple underscore). This affects how ClawMux displays tool activity in the TUI.

---

## 7. Revised Architectural Perspective

### 7.1 ACP as a First-Class Backend (Not Just an Alternative)

The original issue (#36) framed kiro-cli/ACP as "an alternative to opencode." After deep research, we believe ACP should be viewed as a **first-class backend** that in several areas provides a superior model:

| Dimension | OpenCode | ACP | Winner |
|-----------|----------|-----|--------|
| Protocol standardization | Proprietary REST API | Open standard, 40+ clients | ACP |
| Feature richness | Adequate for current needs | Structured plans, inline diffs, tool kinds, rich permissions | ACP |
| Ecosystem compatibility | Only opencode | Any ACP agent (kiro, Zed agents, etc.) | ACP |
| Process model simplicity | Shared server with SSE | Subprocess per agent | Tie (different tradeoffs) |
| Orchestrator role | Passive consumer | **Active participant** (FS/terminal/permission provider) | ACP |
| Turn limits | Built-in `steps` | Must implement externally | OpenCode |
| Token tracking | Built-in | RFD (coming) | OpenCode (for now) |
| Maturity | Proven in ClawMux | New integration | OpenCode |

### 7.2 Recommended Architecture: ACP Conductor

Rather than treating ACP as a simple backend swap, ClawMux should adopt the **ACP Conductor** pattern:

```
TUI (ratatui) -> App (message dispatch) -> BackendDispatcher
                                              |
                          +-------------------+-------------------+
                          |                                       |
                  OpenCodeBackend                          AcpConductor
                  (HTTP REST + SSE)                    (JSON-RPC stdio)
                          |                                       |
                  OpenCode Server                    KiroProcess (per agent)
                                                     or any ACP agent
```

The `AcpConductor` would:
1. Spawn agent processes and manage their lifecycle
2. Handle bidirectional JSON-RPC (send prompts, respond to permission/FS/terminal requests)
3. Translate ACP notifications to `AppMessage` variants
4. Implement turn counting (replacing OpenCode's `steps`)
5. Provide FS/terminal services to agents (with ClawMux-controlled security boundaries)
6. Inject task/story context via prompt enrichment or resource attachments

### 7.3 Process Model Recommendation

After analysis, we recommend a **hybrid approach** for kiro specifically:

- **One kiro-cli process per pipeline agent** (not per task)
- Use `session/set_mode` within a process to configure the agent role
- Create a new session (`session/new`) for each pipeline stage
- This avoids the auto-compaction issue from context buildup
- Each process gets its own agent JSON config with appropriate tool permissions

For generic ACP agents (non-kiro), spawn one process per session since `session/set_mode` behavior is agent-specific.

### 7.4 Leveraging the Rust SDK

Consider using the official `sacp` Rust crate instead of building JSON-RPC transport from scratch. Benefits:
- Handles JSON-RPC framing, capability negotiation, message routing
- Built-in conductor/proxy support
- Type-safe message handling with derive macros
- Maintained by Zed/JetBrains ecosystem

Tradeoff: adds a dependency and couples to the SDK's design patterns. But it significantly reduces implementation effort for the transport layer.

---

## 8. Feature Opportunities Unique to ACP

Beyond matching OpenCode's capabilities, ACP enables features ClawMux cannot achieve today:

### 8.1 Centralized Security Boundary (FS/Terminal Delegation)

With ACP's delegation model, the *agent* asks *ClawMux* to read files and run commands. This means ClawMux can:
- Enforce path restrictions centrally (not per-agent config)
- Log every file read/write and command execution
- Show real-time FS/terminal activity in the TUI
- Implement a unified allowlist across all pipeline agents

### 8.2 Structured Plan Visualization

ACP agents report structured plans with `priority` and `status` per entry. ClawMux could render these in the Plan tab as a live task tracker showing which sub-steps the agent is working on, completed, or hasn't started.

### 8.3 Inline Diff Display

Instead of polling a diff endpoint after session completion, ACP delivers diffs inline with tool calls as `{ type: "diff", path, oldText, newText }`. ClawMux could show diffs in real-time as they are produced, not just at the end.

### 8.4 Dynamic Model/Reasoning Configuration

Via `session/set_config_option`, ClawMux could adjust model and reasoning level per pipeline stage:
- Intake/Design: use a cheaper/faster model (e.g., Haiku)
- Implementation: use the most capable model (e.g., Opus)
- Code review: increase reasoning/thought level

### 8.5 Custom ClawMux Extensions

Using ACP's `_meta` and `_`-prefixed method extensibility:
- `_clawmux/task_context` -- inject story/task details into agent sessions
- `_clawmux/pipeline_status` -- notify agents of their position in the pipeline
- `_clawmux/artifact_share` -- pass artifacts between pipeline stages

---

## 9. References

### ACP Specification
- [Protocol Overview](https://agentclientprotocol.com/protocol/overview)
- [Transports](https://agentclientprotocol.com/protocol/transports)
- [Session Setup](https://agentclientprotocol.com/protocol/session-setup)
- [Prompt Turn](https://agentclientprotocol.com/protocol/prompt-turn)
- [Tool Calls](https://agentclientprotocol.com/protocol/tool-calls)
- [Session Modes](https://agentclientprotocol.com/protocol/session-modes)
- [Terminals](https://agentclientprotocol.com/protocol/terminals)
- [Slash Commands](https://agentclientprotocol.com/protocol/slash-commands)
- [Rust SDK](https://agentclientprotocol.com/libraries/rust)

### Kiro CLI
- [ACP Integration](https://kiro.dev/docs/cli/acp/)
- [Custom Agents](https://kiro.dev/docs/cli/custom-agents/)
- [Agent Configuration Reference](https://kiro.dev/docs/cli/custom-agents/configuration-reference/)
- [MCP Configuration](https://kiro.dev/docs/cli/mcp/configuration/)
- [Hooks](https://kiro.dev/docs/cli/hooks/)
- [Steering](https://kiro.dev/docs/cli/steering/)
- [Built-in Tools Reference](https://kiro.dev/docs/cli/reference/built-in-tools/)

### Related
- [acpx - Headless ACP Client](https://github.com/openclaw/acpx) (reference implementation)
- [sacp - Rust ACP SDK](https://crates.io/crates/sacp) (official, used by Zed)
- [Kiro Adopts ACP Blog Post](https://kiro.dev/blog/kiro-adopts-acp/)
