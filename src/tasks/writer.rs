//! Task file serializer.
//!
//! Writes `Task` structs back to markdown files, preserving unknown sections
//! verbatim to ensure round-trip fidelity for agent-added or user-added content.
//! Task 1.5 implements the full serializer.

//TODO: Task 1.5 -- implement TaskWriter with write(task, path) -> Result<()> method
