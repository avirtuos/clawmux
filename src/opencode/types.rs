//! Rust types mirroring the opencode OpenAPI schema.
//!
//! Covers sessions, messages, message parts, SSE events, and file diffs.

use serde::{Deserialize, Serialize};

/// A part of an opencode agent message.
///
/// Agents can produce text, tool calls, reasoning traces, and file content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffStatus {
    /// File was newly created.
    Added,
    /// File was modified.
    Modified,
    /// File was removed.
    Deleted,
}

/// Classifies a single line within a diff hunk.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffLineKind {
    /// Unchanged context line.
    Context,
    /// Line added in the new version.
    Added,
    /// Line removed from the old version.
    Removed,
}

/// A single line in a diff hunk, with its classification and content.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffLine {
    /// Whether this line was added, removed, or unchanged.
    pub kind: DiffLineKind,
    /// The text content of the line.
    pub content: String,
}

/// A contiguous block of changes within a file diff.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileDiff {
    /// Relative path to the modified file.
    pub path: String,
    /// Whether the file was added, modified, or deleted.
    pub status: DiffStatus,
    /// All change hunks for this file.
    pub hunks: Vec<DiffHunk>,
}

/// The role of an opencode message sender.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MessageRole {
    /// A message sent by the user.
    User,
    /// A message sent by the assistant.
    Assistant,
}

/// An opencode session record returned by the API.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeSession {
    /// Unique session identifier.
    pub id: String,
    /// UTC timestamp when the session was created.
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// A message within an opencode session.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeMessage {
    /// Unique message identifier.
    pub id: String,
    /// Whether this message came from the user or assistant.
    pub role: MessageRole,
    /// The ordered list of content parts comprising this message.
    pub parts: Vec<MessagePart>,
}

/// An SSE event emitted by the opencode server during a session.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OpenCodeEvent {
    /// A new session was created.
    SessionCreated { session_id: String },
    /// A new message was created in a session.
    MessageCreated {
        session_id: String,
        message: OpenCodeMessage,
    },
    /// An existing message's parts were updated.
    MessageUpdated {
        session_id: String,
        message_id: String,
        parts: Vec<MessagePart>,
    },
    /// A tool is currently being executed.
    ToolExecuting { session_id: String, tool: String },
    /// A tool finished executing.
    ToolCompleted {
        session_id: String,
        tool: String,
        result: String,
    },
    /// A session completed successfully.
    SessionCompleted { session_id: String },
    /// A session encountered an error.
    SessionError { session_id: String, error: String },
}

/// A single content part in a user request message.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ContentPart {
    /// Plain text content.
    Text { text: String },
}

/// Request body for `POST /session/:id/message`.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    /// The content parts of the message.
    pub content: Vec<ContentPart>,
    /// Optional agent identifier to route the message to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
}

/// Response body for `POST /session` (create session).
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateSessionResponse(pub OpenCodeSession);

/// Response body for `GET /global/health`.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthResponse {
    /// Whether the server is healthy.
    pub ok: bool,
    /// The server version string.
    pub version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_message_part_text_serde() {
        let part = MessagePart::Text {
            text: "hello world".to_string(),
        };
        let json = serde_json::to_string(&part).expect("serialize");
        let round_tripped: MessagePart = serde_json::from_str(&json).expect("deserialize");
        match round_tripped {
            MessagePart::Text { text } => assert_eq!(text, "hello world"),
            other => panic!("unexpected variant: {:?}", other),
        }
    }

    #[test]
    fn test_create_session_response_deserialize() {
        let json = r#"{"id":"sess-1","createdAt":"2024-01-01T00:00:00Z"}"#;
        let resp: CreateSessionResponse = serde_json::from_str(json).expect("deserialize");
        assert_eq!(resp.0.id, "sess-1");
    }

    #[test]
    fn test_health_response_ok() {
        let json = r#"{"ok":true,"version":"1.0"}"#;
        let resp: HealthResponse = serde_json::from_str(json).expect("deserialize");
        assert!(resp.ok);
        assert_eq!(resp.version, "1.0");
    }

    #[test]
    fn test_file_diff_status_serde() {
        for status in [DiffStatus::Added, DiffStatus::Modified, DiffStatus::Deleted] {
            let json = serde_json::to_string(&status).expect("serialize");
            let round_tripped: DiffStatus = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(round_tripped, status);
        }
    }

    #[test]
    fn test_opencode_event_serde() {
        let event = OpenCodeEvent::SessionCreated {
            session_id: "s1".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let round_tripped: OpenCodeEvent = serde_json::from_str(&json).expect("deserialize");
        match round_tripped {
            OpenCodeEvent::SessionCreated { session_id } => assert_eq!(session_id, "s1"),
            other => panic!("unexpected variant: {:?}", other),
        }
    }
}
