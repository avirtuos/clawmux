//! Top-level application state and message dispatcher.
//!
//! The `App` struct holds all runtime state and coordinates between subsystems:
//! the TUI layer, workflow engine, task store, and opencode client.
//! Task 1.3 fills in the full implementation.

/// Top-level application state.
///
/// Coordinates the TUI, workflow engine, task store, and opencode client
/// via async mpsc channels carrying `AppMessage` values.
#[allow(dead_code)]
pub struct App;
