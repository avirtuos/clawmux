//! Rust types mirroring the opencode OpenAPI schema.
//!
//! Covers sessions, messages, message parts, SSE events, and file diffs.
//!
//! **Note on serialization conventions**: `rename_all = "camelCase"` is applied uniformly
//! throughout this module as it is typical for JS-origin APIs. This convention was inferred
//! from the API's JavaScript origin and has not been confirmed against an OpenAPI spec.
//! Validate field and variant names against the actual spec before the HTTP client is wired up.
//!
//! **Important serde behavior**: `rename_all = "camelCase"` on an enum renames variant
//! *discriminator* values (e.g. `SessionCreated` -> `"sessionCreated"`) but does **not**
//! rename fields *within* struct variants. Fields in struct variants (e.g. `session_id`)
//! remain in Rust snake_case in JSON unless individually annotated with `#[serde(rename)]`.
//! If the API uses camelCase for these fields, explicit rename attributes will be needed.

use serde::{Deserialize, Serialize};

/// A part of an opencode agent message.
///
/// Agents can produce text, tool calls, reasoning traces, and file content.
/// Unknown part types received from the server are silently ignored via the
/// [`MessagePart::Unknown`] catch-all variant.
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
    /// An unrecognized message part type; ignored gracefully for forward compatibility.
    #[serde(other)]
    Unknown,
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
///
/// Unknown event types received from the server are silently ignored via the
/// [`OpenCodeEvent::Unknown`] catch-all variant.
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
    /// An unrecognized event type; ignored gracefully for forward compatibility.
    #[serde(other)]
    Unknown,
}

/// A single content part in a user request message.
///
/// Modelled as an enum rather than a plain struct to accommodate future API variants
/// (e.g. `Image`, `File`) without a breaking type change, matching the pattern used
/// by [`MessagePart`].
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

    // --- MessagePart ---

    #[test]
    fn test_message_part_text_deserialize() {
        // JSON matching the expected API wire format: tag "text", field "text".
        let json = r#"{"type":"text","text":"hello"}"#;
        let part: MessagePart = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(part, MessagePart::Text { ref text } if text == "hello"),
            "unexpected variant: {part:?}"
        );
    }

    #[test]
    fn test_message_part_unknown_variant() {
        // An unrecognized type tag should produce Unknown rather than an error.
        let json = r#"{"type":"image","url":"https://example.com/img.png"}"#;
        let part: MessagePart = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(part, MessagePart::Unknown),
            "expected Unknown, got: {part:?}"
        );
    }

    // --- DiffStatus ---

    #[test]
    fn test_file_diff_status_deserialize() {
        // Verify the camelCase rename produces lowercase single-word variants.
        let added: DiffStatus = serde_json::from_str(r#""added""#).expect("deserialize");
        assert_eq!(added, DiffStatus::Added);
        let modified: DiffStatus = serde_json::from_str(r#""modified""#).expect("deserialize");
        assert_eq!(modified, DiffStatus::Modified);
        let deleted: DiffStatus = serde_json::from_str(r#""deleted""#).expect("deserialize");
        assert_eq!(deleted, DiffStatus::Deleted);
    }

    // --- OpenCodeEvent ---

    #[test]
    fn test_opencode_event_session_created_deserialize() {
        // The tag value is camelCase ("sessionCreated") because rename_all on the enum
        // renames variant discriminators. However, rename_all does NOT rename fields within
        // struct variants, so the field key remains snake_case ("session_id").
        // NOTE: If the real opencode API sends "sessionId" (camelCase) for this field,
        // explicit #[serde(rename = "sessionId")] attributes will be needed on each field.
        let json = r#"{"type":"sessionCreated","session_id":"s1"}"#;
        let event: OpenCodeEvent = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(event, OpenCodeEvent::SessionCreated { ref session_id } if session_id == "s1"),
            "unexpected variant: {event:?}"
        );
    }

    #[test]
    fn test_opencode_event_unknown_variant() {
        // An unrecognized event type should produce Unknown rather than an error.
        let json = r#"{"type":"sessionPaused","sessionId":"s1"}"#;
        let event: OpenCodeEvent = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(event, OpenCodeEvent::Unknown),
            "expected Unknown, got: {event:?}"
        );
    }

    // --- OpenCodeMessage ---

    #[test]
    fn test_opencode_message_deserialize() {
        // Verifies nested MessagePart deserialization and the "user" role discriminator.
        let json = r#"{"id":"m1","role":"user","parts":[{"type":"text","text":"hi"}]}"#;
        let msg: OpenCodeMessage = serde_json::from_str(json).expect("deserialize");
        assert_eq!(msg.id, "m1");
        assert_eq!(msg.role, MessageRole::User);
        assert_eq!(msg.parts.len(), 1);
        assert!(
            matches!(msg.parts[0], MessagePart::Text { ref text } if text == "hi"),
            "unexpected part: {:?}",
            msg.parts[0]
        );
    }

    // --- SendMessageRequest ---

    #[test]
    fn test_send_message_request_agent_omitted() {
        // When agent is None the "agent" key must be absent from the serialized JSON.
        let req = SendMessageRequest {
            content: vec![ContentPart::Text {
                text: "hello".to_string(),
            }],
            agent: None,
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(
            !json.contains("\"agent\""),
            "\"agent\" key should be absent when None, got: {json}"
        );
        assert!(
            json.contains("\"content\""),
            "\"content\" key should be present, got: {json}"
        );
    }

    #[test]
    fn test_send_message_request_agent_present() {
        // When agent is Some the "agent" key must appear in the serialized JSON.
        let req = SendMessageRequest {
            content: vec![ContentPart::Text {
                text: "hello".to_string(),
            }],
            agent: Some("claude".to_string()),
        };
        let json = serde_json::to_string(&req).expect("serialize");
        assert!(
            json.contains("\"agent\""),
            "\"agent\" key should be present when Some, got: {json}"
        );
        assert!(
            json.contains("\"claude\""),
            "agent value should be present, got: {json}"
        );
    }

    // --- ContentPart ---

    #[test]
    fn test_content_part_text_deserialize() {
        let json = r#"{"type":"text","text":"hello"}"#;
        let part: ContentPart = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(part, ContentPart::Text { ref text } if text == "hello"),
            "unexpected variant: {part:?}"
        );
    }

    // --- Health / CreateSession (existing literal-JSON tests) ---

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
}
