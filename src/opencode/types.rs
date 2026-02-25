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
    /// Optional because the API may send timestamps in a different format/location.
    #[serde(default)]
    pub created_at: Option<chrono::DateTime<chrono::Utc>>,
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

/// A pending permission request from OpenCode.
///
/// Sent when the agent needs approval to execute a tool operation
/// (e.g. running a shell command). Resolved via `POST /session/:id/permissions/:id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionRequest {
    /// The permission request ID, used when calling the resolve endpoint.
    pub id: String,
    /// The session that owns this request.
    pub session_id: String,
    /// The permission type being requested (e.g. "bash").
    pub permission: String,
    /// The specific command patterns being requested (e.g. ["cargo build 2>&1"]).
    pub patterns: Vec<String>,
    /// Patterns already permanently allowed for this permission type.
    pub always: Vec<String>,
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
    SessionCreated {
        #[serde(alias = "sessionId")]
        session_id: String,
        /// Parent session ID, present when OpenCode spawns a child session from an
        /// existing one (e.g. for parallel agents or sub-tasks). Used to inherit
        /// the parent's task mapping in the session map.
        #[serde(default)]
        parent_id: Option<String>,
    },
    /// A new message was created in a session.
    MessageCreated {
        #[serde(alias = "sessionId")]
        session_id: String,
        message: OpenCodeMessage,
    },
    /// An existing message's parts were updated.
    MessageUpdated {
        #[serde(alias = "sessionId")]
        session_id: String,
        #[serde(alias = "messageId")]
        message_id: String,
        parts: Vec<MessagePart>,
    },
    /// A tool is currently being executed.
    ToolExecuting {
        #[serde(alias = "sessionId")]
        session_id: String,
        tool: String,
    },
    /// A tool finished executing.
    ToolCompleted {
        #[serde(alias = "sessionId")]
        session_id: String,
        tool: String,
        result: String,
    },
    /// An incremental text delta for a message part (OpenCode >= 1.2 streaming format).
    MessagePartDelta {
        #[serde(alias = "sessionID")]
        session_id: String,
        #[serde(alias = "messageID")]
        message_id: String,
        /// The field being updated (e.g. "text").
        field: String,
        /// Incremental text chunk to append.
        delta: String,
    },
    /// A session completed successfully.
    SessionCompleted {
        #[serde(alias = "sessionId")]
        session_id: String,
    },
    /// A session encountered an error.
    SessionError {
        #[serde(alias = "sessionId")]
        session_id: String,
        error: String,
    },
    /// An OpenCode agent is requesting permission to execute a tool operation.
    PermissionAsked {
        session_id: String,
        request: PermissionRequest,
    },
    /// An OpenCode agent asked a question via the `question.asked` SSE event.
    QuestionAsked {
        session_id: String,
        request_id: String,
        question: String,
    },
    /// Session diffs have changed; use as a trigger to poll the diff endpoint.
    SessionDiff { session_id: String },
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

/// Request body for `POST /session/:id/prompt_async`.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendMessageRequest {
    /// The content parts of the message (field name matches the opencode API).
    pub parts: Vec<ContentPart>,
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
    #[serde(alias = "ok")]
    pub healthy: bool,
    /// The server version string, if provided by the server.
    pub version: Option<String>,
}

/// The runtime status of an opencode session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SessionStatus {
    /// Session is idle (no active processing).
    Idle,
    /// Session is actively processing a prompt.
    Busy,
    /// Session is retrying after a transient failure.
    Retry {
        attempt: u32,
        message: String,
        next: u64,
    },
}

/// Response body for `GET /session/status`.
pub type SessionStatusResponse = std::collections::HashMap<String, SessionStatus>;

/// Timing metadata for a message in the session message listing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageTime {
    /// When processing started (epoch ms), if available.
    #[serde(default)]
    pub created: Option<u64>,
    /// When processing completed (epoch ms), if available.
    #[serde(default)]
    pub completed: Option<u64>,
}

/// An error attached to an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageError {
    /// Human-readable error message.
    #[serde(default)]
    pub message: Option<String>,
}

/// Info block for a message in the `GET /session/:id/message` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageInfo {
    /// Message role (user/assistant).
    pub role: MessageRole,
    /// How the message finished (e.g. "end_turn", "error"), if at all.
    #[serde(default)]
    pub finish: Option<String>,
    /// Error details, if the message errored.
    #[serde(default)]
    pub error: Option<MessageError>,
    /// Timing metadata.
    #[serde(default)]
    pub time: Option<MessageTime>,
}

/// A single entry in the `GET /session/:id/message` response array.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageEntry {
    /// Message metadata (role, finish reason, error, timing).
    pub info: MessageInfo,
    /// The content parts of this message.
    #[serde(default)]
    pub parts: Vec<MessagePart>,
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
            matches!(event, OpenCodeEvent::SessionCreated { ref session_id, .. } if session_id == "s1"),
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

    #[test]
    fn test_opencode_event_camel_case_field() {
        // The API sends camelCase field names (e.g. "sessionId"); aliases must accept them.
        let json = r#"{"type":"sessionCreated","sessionId":"s1"}"#;
        let event: OpenCodeEvent = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(event, OpenCodeEvent::SessionCreated { ref session_id, .. } if session_id == "s1"),
            "unexpected variant: {event:?}"
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
            parts: vec![ContentPart::Text {
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
            json.contains("\"parts\""),
            "\"parts\" key should be present, got: {json}"
        );
    }

    #[test]
    fn test_send_message_request_agent_present() {
        // When agent is Some the "agent" key must appear in the serialized JSON.
        let req = SendMessageRequest {
            parts: vec![ContentPart::Text {
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
        // API does not send createdAt; only id is required.
        let json = r#"{"id":"sess-1"}"#;
        let resp: CreateSessionResponse = serde_json::from_str(json).expect("deserialize");
        assert_eq!(resp.0.id, "sess-1");
        assert!(resp.0.created_at.is_none());
    }

    #[test]
    fn test_create_session_response_with_created_at() {
        // If a server does send createdAt, it should still deserialize correctly.
        let json = r#"{"id":"sess-1","createdAt":"2024-01-01T00:00:00Z"}"#;
        let resp: CreateSessionResponse = serde_json::from_str(json).expect("deserialize");
        assert_eq!(resp.0.id, "sess-1");
        assert!(resp.0.created_at.is_some());
    }

    #[test]
    fn test_health_response_ok() {
        let json = r#"{"healthy":true,"version":"1.0"}"#;
        let resp: HealthResponse = serde_json::from_str(json).expect("deserialize");
        assert!(resp.healthy);
        assert_eq!(resp.version, Some("1.0".to_string()));
    }

    #[test]
    fn test_health_response_ok_alias() {
        // Older servers that send "ok" instead of "healthy" should still deserialize.
        let json = r#"{"ok":true,"version":"1.0"}"#;
        let resp: HealthResponse = serde_json::from_str(json).expect("deserialize");
        assert!(resp.healthy);
    }

    #[test]
    fn test_health_response_no_version() {
        // Servers that omit the "version" field should still deserialize cleanly.
        let json = r#"{"healthy":true}"#;
        let resp: HealthResponse = serde_json::from_str(json).expect("deserialize");
        assert!(resp.healthy);
        assert_eq!(resp.version, None);
    }

    // --- SessionStatus ---

    #[test]
    fn test_session_status_idle_deserialize() {
        let json = r#"{"type":"idle"}"#;
        let status: SessionStatus = serde_json::from_str(json).expect("deserialize");
        assert_eq!(status, SessionStatus::Idle);
    }

    #[test]
    fn test_session_status_busy_deserialize() {
        let json = r#"{"type":"busy"}"#;
        let status: SessionStatus = serde_json::from_str(json).expect("deserialize");
        assert_eq!(status, SessionStatus::Busy);
    }

    #[test]
    fn test_session_status_retry_deserialize() {
        let json = r#"{"type":"retry","attempt":2,"message":"rate limited","next":1700000000}"#;
        let status: SessionStatus = serde_json::from_str(json).expect("deserialize");
        assert!(
            matches!(
                &status,
                SessionStatus::Retry { attempt: 2, message, next: 1700000000 }
                    if message == "rate limited"
            ),
            "unexpected: {status:?}"
        );
    }

    #[test]
    fn test_session_status_response_deserialize() {
        let json = r#"{"sess-1":{"type":"idle"},"sess-2":{"type":"busy"}}"#;
        let resp: SessionStatusResponse = serde_json::from_str(json).expect("deserialize");
        assert_eq!(resp.get("sess-1"), Some(&SessionStatus::Idle));
        assert_eq!(resp.get("sess-2"), Some(&SessionStatus::Busy));
    }

    // --- MessageEntry ---

    #[test]
    fn test_message_entry_with_error_deserialize() {
        let json = r#"{
            "info": {
                "role": "assistant",
                "finish": "error",
                "error": {"message": "agent.model on undefined"}
            },
            "parts": []
        }"#;
        let entry: MessageEntry = serde_json::from_str(json).expect("deserialize");
        assert_eq!(entry.info.role, MessageRole::Assistant);
        assert_eq!(entry.info.finish.as_deref(), Some("error"));
        assert_eq!(
            entry.info.error.as_ref().and_then(|e| e.message.as_deref()),
            Some("agent.model on undefined")
        );
    }

    // --- PermissionRequest ---

    #[test]
    fn test_permission_request_deserialize() {
        let json = r#"{
            "id": "perm-1",
            "session_id": "sess-abc",
            "permission": "bash",
            "patterns": ["cargo build 2>&1"],
            "always": ["cargo fmt"]
        }"#;
        let req: PermissionRequest = serde_json::from_str(json).expect("deserialize");
        assert_eq!(req.id, "perm-1");
        assert_eq!(req.session_id, "sess-abc");
        assert_eq!(req.permission, "bash");
        assert_eq!(req.patterns, vec!["cargo build 2>&1"]);
        assert_eq!(req.always, vec!["cargo fmt"]);
    }

    #[test]
    fn test_permission_request_empty_patterns() {
        let json = r#"{
            "id": "perm-2",
            "session_id": "sess-abc",
            "permission": "write",
            "patterns": [],
            "always": []
        }"#;
        let req: PermissionRequest = serde_json::from_str(json).expect("deserialize");
        assert!(req.patterns.is_empty());
        assert!(req.always.is_empty());
    }

    #[test]
    fn test_message_entry_completed_deserialize() {
        let json = r#"{
            "info": {
                "role": "assistant",
                "finish": "end_turn",
                "time": {"created": 1000, "completed": 2000}
            },
            "parts": [{"type":"text","text":"done"}]
        }"#;
        let entry: MessageEntry = serde_json::from_str(json).expect("deserialize");
        assert_eq!(entry.info.finish.as_deref(), Some("end_turn"));
        assert!(entry.info.error.is_none());
        let time = entry.info.time.as_ref().expect("time");
        assert_eq!(time.created, Some(1000));
        assert_eq!(time.completed, Some(2000));
        assert_eq!(entry.parts.len(), 1);
    }
}
