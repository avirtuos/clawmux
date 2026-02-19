//! Rust types mirroring the opencode OpenAPI schema.
//!
//! Covers sessions, messages, message parts, SSE events, and file diffs.
//! This file contains minimal stubs needed for `AppMessage` to compile.
//! Serde derives will be added alongside full implementations in a future task.

//TODO: Task 5.1 -- replace stubs with full opencode API types (OpenCodeSession,
//TODO:             OpenCodeMessage, OpenCodeEvent, MessageRole, etc.) and add serde derives

/// A part of an opencode agent message.
///
/// Agents can produce text, tool calls, reasoning traces, and file content.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum MessagePart {
    /// A plain text segment.
    Text { text: String },
    /// A tool invocation with its optional result.
    Tool {
        name: String,
        input: serde_json::Value,
        result: Option<String>,
    },
    /// An agent reasoning trace.
    Reasoning { text: String },
    /// A file path and its content.
    File { path: String, content: String },
}

/// The modification status of a file in a session diff.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum DiffStatus {
    /// File was newly created.
    Added,
    /// File was modified.
    Modified,
    /// File was removed.
    Deleted,
}

/// Classifies a single line within a diff hunk.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum DiffLineKind {
    /// Unchanged context line.
    Context,
    /// Line added in the new version.
    Added,
    /// Line removed from the old version.
    Removed,
}

/// A single line in a diff hunk, with its classification and content.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DiffLine {
    /// Whether this line was added, removed, or unchanged.
    pub kind: DiffLineKind,
    /// The text content of the line.
    pub content: String,
}

/// A contiguous block of changes within a file diff.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct DiffHunk {
    /// Starting line number in the original file (1-based).
    pub old_start: u32,
    /// Starting line number in the new file (1-based).
    pub new_start: u32,
    /// The lines making up this hunk.
    pub lines: Vec<DiffLine>,
}

/// A file-level diff returned by the opencode `/session/:id/diff` endpoint.
///
/// Contains the modified file path, its change status, and all diff hunks.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct FileDiff {
    /// Relative path to the modified file.
    pub path: String,
    /// Whether the file was added, modified, or deleted.
    pub status: DiffStatus,
    /// All change hunks for this file.
    pub hunks: Vec<DiffHunk>,
}
