//! Builds user messages from task context and prior agent work.
//!
//! The system prompt lives in the opencode agent definition file
//! (`.opencode/agents/clawdmux/<agent>.md`). This module only composes the
//! user-facing message injected at runtime, combining task description, story
//! context, accumulated prior work, and any kickback reason.
//! Task 1.4 implements the full prompt composition logic.

//TODO: Task 1.4 -- implement compose_user_message(agent, task, kickback_reason) -> String
