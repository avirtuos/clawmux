//! Session lifecycle: create, prompt, abort, fork, and diff retrieval.
//!
//! Wraps the opencode session API endpoints:
//! `POST /session`, `POST /session/:id/message`, `POST /session/:id/prompt_async`,
//! `DELETE /session/:id`, `POST /session/:id/fork`, `GET /session/:id/diff`.
//! Task 2.2 implements the full session lifecycle.

//TODO: Task 2.2 -- implement create_session, send_prompt_async, send_prompt_structured,
//TODO:             abort_session, fork_session, get_session_diffs
