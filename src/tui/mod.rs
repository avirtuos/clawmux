//! Top-level TUI draw and input handling.
//!
//! Coordinates ratatui rendering across the layout, task list widget, and the
//! 4-tab right pane. Dispatches keyboard events to the focused widget.
//! Task 3.1 implements the full TUI layer.

pub mod layout;
pub mod tabs;
pub mod task_list;
