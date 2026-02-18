//! OpenCode server lifecycle: spawn, health check, and shutdown.
//!
//! Manages the opencode server as a child process. On startup, checks for a
//! running server via `GET /global/health`; if absent, spawns
//! `opencode serve --port <port> --hostname 127.0.0.1` with LLM provider
//! credentials injected as environment variables. Sends SIGTERM on shutdown.
//! Task 2.1 implements the full server lifecycle.

//TODO: Task 2.1 -- implement spawn, health_check, and shutdown for opencode server process
