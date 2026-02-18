# ClawdMux

ClawdMux is a GenAI coding assistance multiplexer and task orchestrator. It manages scrum-style stories and tasks, assigns them to a sequential pipeline of AI agents powered by [opencode](https://opencode.ai), and provides a unified TUI for interacting with those agents.

## Features

- **Scrum-style task management**: Stories and tasks loaded from markdown files
- **7-agent sequential pipeline**: Intake -> Design -> Planning -> Implementation -> Code Quality -> Security Review -> Code Review
- **Human-in-the-loop**: Questions, approval gates, and code review with inline comments
- **Unified TUI**: Left pane task navigation + 4-tab right pane (details, activity, team status, review)
- **Multi-provider LLM support**: Works with any provider supported by opencode (Anthropic, OpenAI, Google, etc.)

## Getting Started

### Prerequisites

- Rust (stable toolchain)
- An API key for a supported LLM provider

### Installation

```bash
cargo install --path .
```

### Project Initialization

Run the interactive setup command once per project:

```bash
clawdmux init
```

This will:
1. Check for and optionally install the `opencode` binary
2. Configure your LLM provider credentials (stored in `~/.config/clawdmux/config.toml`)
3. Scaffold the project directory structure (`.clawdmux/`, `.opencode/agents/`, `tasks/`)
4. Generate default agent definition files

To regenerate agent definitions from built-in defaults:

```bash
clawdmux init --reset-agents
```

### Running

```bash
clawdmux
```

## Task File Format

Tasks are markdown files in the `tasks/` (or `docs/tasks/`) directory:

```markdown
Story: 1. Big Story
Task: 1.1 First Task
Status: OPEN
Assigned To: [Intake Agent]

## Description

<description of the task>

## Starting Prompt

<optional starting prompt>

## Questions

Q1 [Intake Agent]: What language do you want to use?
A1: Rust

## Design

<design considerations>

## Implementation Plan

<implementation plan>

## Work Log

1 2026-02-10T10:00:01 [Design Agent] Updated task with design.
```

## Architecture

ClawdMux acts as a client to an `opencode serve` HTTP server. See `docs/design.md` for the full architecture documentation.

## Development

```bash
cargo build
cargo test
cargo clippy -- -D warnings
```
