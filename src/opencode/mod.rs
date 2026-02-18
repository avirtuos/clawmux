//! OpenCode HTTP client and SSE event listener.
//!
//! The `OpenCodeClient` communicates with an opencode server over HTTP + SSE,
//! providing session management, prompt sending, diff retrieval, and health checks.
//! Task 2.2 implements the full client.

pub mod events;
pub mod server;
pub mod session;
pub mod types;
