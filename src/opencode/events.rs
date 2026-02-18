//! SSE event stream consumer.
//!
//! Connects to opencode's `GET /global/event` SSE stream, parses the 40+ event
//! types, and maps them to `AppMessage` values routed to the appropriate subsystem.
//! Runs as a long-lived tokio task.
//! Task 2.3 implements the full EventStreamConsumer.

//TODO: Task 2.3 -- implement EventStreamConsumer with run(base_url) -> Result<()>
