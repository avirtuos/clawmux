//! TaskStore: in-memory task cache with file watching.
//!
//! Loads task markdown files from `./tasks/` or `./docs/tasks/`, watches for
//! external modifications via the `notify` crate, and maintains an in-memory
//! cache keyed by `TaskId`.
//! Task 1.5 implements the full TaskStore.

pub mod models;
pub mod parser;
pub mod writer;

#[allow(unused_imports)]
pub use models::{Question, Story, Task, TaskId, TaskStatus, WorkLogEntry};
#[allow(unused_imports)]
pub use writer::write_task;
