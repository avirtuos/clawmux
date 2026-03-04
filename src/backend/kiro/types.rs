//! ACP (Agent Client Protocol) message type definitions.
//!
//! This module defines the JSON-RPC 2.0 request/response/notification types
//! used to communicate with kiro-cli over stdin/stdout.
//!
//! Reference: Agent Client Protocol spec (JSON-RPC 2.0 over newline-delimited stdin/stdout).

use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// JSON-RPC 2.0 envelope types
// ---------------------------------------------------------------------------

/// A JSON-RPC 2.0 request (client -> agent or agent -> client for bidirectional).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl RpcRequest {
    /// Create a new request with the given id, method, and optional params.
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 notification (no id, fire-and-forget).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl RpcNotification {
    /// Create a new notification with the given method and optional params.
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            method: method.into(),
            params,
        }
    }
}

/// A JSON-RPC 2.0 response to a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcResponse {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl RpcResponse {
    /// Create a successful response with the given result.
    pub fn ok(id: u64, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }

    /// Create an error response.
    pub fn err(id: u64, error: RpcError) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// A JSON-RPC 2.0 error object.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// An incoming message from the agent dispatched to the event loop.
///
/// Agent responses to our requests are handled directly by [`Transport::request`]
/// via oneshot channels and never appear here. This enum only carries items
/// that the event loop must process: bidirectional requests and notifications.
#[derive(Debug, Clone)]
pub enum IncomingMessage {
    /// A bidirectional request from the agent (has id AND method; needs a response).
    Request(RpcRequest),
    /// A one-way notification from the agent (no id).
    Notification(RpcNotification),
}

// ---------------------------------------------------------------------------
// ACP-specific initialize request/response types
// ---------------------------------------------------------------------------

/// Params for the `initialize` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    pub protocol_version: String,
    #[serde(rename = "clientInfo")]
    pub client_info: ClientInfo,
    pub capabilities: ClientCapabilities,
}

/// Client identity information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientInfo {
    pub name: String,
    pub version: String,
}

/// Capabilities advertised to the agent during initialization.
///
/// We set `fs` and `terminal` to false — kiro handles these directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientCapabilities {
    pub fs: bool,
    pub terminal: bool,
}

/// Result returned by the agent from the `initialize` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    #[serde(
        rename = "protocolVersion",
        deserialize_with = "deserialize_string_or_int"
    )]
    pub protocol_version: String,
    #[serde(rename = "agentInfo")]
    pub agent_info: AgentInfo,
    #[serde(default)]
    pub capabilities: Value,
}

/// Agent identity returned during initialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    #[serde(deserialize_with = "deserialize_string_or_int")]
    pub version: String,
}

/// Deserialize a field that kiro-cli may send as either a JSON string or a JSON integer.
///
/// Some versions of kiro-cli return numeric fields (e.g. `protocolVersion`, `version`)
/// as bare integers (`1`) rather than strings (`"1"`). This helper accepts both and
/// converts integers to their decimal string representation.
fn deserialize_string_or_int<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};

    struct StringOrInt;

    impl<'de> Visitor<'de> for StringOrInt {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a string or integer")
        }

        fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_string<E: de::Error>(self, v: String) -> std::result::Result<String, E> {
            Ok(v)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<String, E> {
            Ok(v.to_string())
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<String, E> {
            Ok(v.to_string())
        }
    }

    deserializer.deserialize_any(StringOrInt)
}

// ---------------------------------------------------------------------------
// ACP session types
// ---------------------------------------------------------------------------

/// Params for the `session/new` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNewParams {
    /// Absolute path to the project working directory.
    pub cwd: String,
    /// MCP server configurations to pass to the agent session.
    #[serde(rename = "mcpServers")]
    pub mcp_servers: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

/// Result from the `session/new` request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionNewResult {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

/// A single content part in a prompt array (text type).
///
/// Kiro-cli expects `session/prompt` params to include `prompt` as a sequence
/// of content parts (e.g. `[{"type":"text","text":"..."}]`), not a plain string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContentPart {
    #[serde(rename = "type")]
    pub kind: String,
    pub text: String,
}

impl ContentPart {
    /// Create a text content part.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            kind: "text".to_string(),
            text: s.into(),
        }
    }
}

/// Params for the `session/prompt` request.
///
/// Kiro-cli uses the field name `prompt` (not `content` as in the ACP spec),
/// and expects it to be an array of content parts, not a plain string.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionPromptParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub prompt: Vec<ContentPart>,
}

/// Params for the `session/cancel` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionCancelParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
}

// ---------------------------------------------------------------------------
// ACP notification types (agent -> client)
// ---------------------------------------------------------------------------

/// A streaming text chunk from the agent (`agent_message_chunk` notification).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMessageChunkParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub delta: String,
}

/// Tool call status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// A tool call activity notification (`tool_call` or `tool_call_update`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    #[serde(rename = "toolCallId")]
    pub tool_call_id: String,
    pub name: String,
    pub status: ToolCallStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

/// Params for the `session/error` notification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionErrorParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<i64>,
}

// ---------------------------------------------------------------------------
// ACP permission types (bidirectional: agent requests, client responds)
// ---------------------------------------------------------------------------

/// Permission type requested by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcpPermissionKind {
    FileRead,
    FileWrite,
    FileDelete,
    Execute,
    Network,
    #[serde(other)]
    Unknown,
}

/// Params for the `session/request_permission` bidirectional request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestPermissionParams {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub permission: AcpPermissionKind,
    #[serde(default)]
    pub patterns: Vec<String>,
    #[serde(default)]
    pub description: String,
}

/// Permission decision sent back to the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
}

/// Result body for the `session/request_permission` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionResult {
    pub decision: PermissionDecision,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_rpc_request_serialization() {
        let req = RpcRequest::new(1, "initialize", Some(json!({"key": "value"})));
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"id\":1"));
        assert!(json.contains("\"method\":\"initialize\""));
        assert!(json.contains("\"params\""));
    }

    #[test]
    fn test_rpc_notification_no_id() {
        let notif = RpcNotification::new("session/prompt", None);
        let json = serde_json::to_string(&notif).unwrap();
        assert!(json.contains("\"jsonrpc\":\"2.0\""));
        assert!(json.contains("\"method\":\"session/prompt\""));
        assert!(!json.contains("\"id\""));
        assert!(!json.contains("\"params\""));
    }

    #[test]
    fn test_rpc_response_ok() {
        let resp = RpcResponse::ok(42, json!({"sessionId": "abc"}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"id\":42"));
        assert!(json.contains("\"result\""));
        assert!(!json.contains("\"error\""));
    }

    #[test]
    fn test_rpc_response_error() {
        let resp = RpcResponse::err(
            1,
            RpcError {
                code: -32600,
                message: "Invalid Request".to_string(),
                data: None,
            },
        );
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains("\"error\""));
        assert!(!json.contains("\"result\""));
    }

    #[test]
    fn test_initialize_params_serialization() {
        let params = InitializeParams {
            protocol_version: "1.0".to_string(),
            client_info: ClientInfo {
                name: "clawdmux".to_string(),
                version: "0.1.0".to_string(),
            },
            capabilities: ClientCapabilities {
                fs: false,
                terminal: false,
            },
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"protocolVersion\":\"1.0\""));
        assert!(json.contains("\"clientInfo\""));
        assert!(json.contains("\"fs\":false"));
        assert!(json.contains("\"terminal\":false"));
    }

    #[test]
    fn test_tool_call_status_deserialization() {
        let s: ToolCallStatus = serde_json::from_str("\"in_progress\"").unwrap();
        assert_eq!(s, ToolCallStatus::InProgress);
        let s: ToolCallStatus = serde_json::from_str("\"completed\"").unwrap();
        assert_eq!(s, ToolCallStatus::Completed);
    }

    #[test]
    fn test_permission_kind_unknown() {
        // Unknown variants should deserialize as Unknown (via #[serde(other)])
        let k: AcpPermissionKind = serde_json::from_str("\"custom_permission\"").unwrap();
        assert!(matches!(k, AcpPermissionKind::Unknown));
    }

    #[test]
    fn test_permission_decision_serialization() {
        let d = PermissionDecision::AllowOnce;
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"allow_once\"");

        let d = PermissionDecision::RejectAlways;
        let json = serde_json::to_string(&d).unwrap();
        assert_eq!(json, "\"reject_always\"");
    }

    #[test]
    fn test_session_new_params_includes_cwd_and_mcp_servers() {
        let params = SessionNewParams {
            cwd: "/home/user/project".to_string(),
            mcp_servers: vec![],
            metadata: None,
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"cwd\":\"/home/user/project\""));
        assert!(json.contains("\"mcpServers\":[]"));
        // metadata: None should be omitted entirely
        assert!(!json.contains("metadata"));
    }

    #[test]
    fn test_session_new_result_deserialization() {
        let json = r#"{"sessionId":"sess-123"}"#;
        let result: SessionNewResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.session_id, "sess-123");
    }

    #[test]
    fn test_initialize_result_string_version() {
        let json =
            r#"{"protocolVersion":"1.0","agentInfo":{"name":"clawdmux-intake","version":"0.1.0"}}"#;
        let result: InitializeResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.protocol_version, "1.0");
        assert_eq!(result.agent_info.version, "0.1.0");
    }

    #[test]
    fn test_initialize_result_integer_version() {
        // kiro-cli may return version fields as bare integers rather than strings.
        let json = r#"{"protocolVersion":1,"agentInfo":{"name":"clawdmux-intake","version":1}}"#;
        let result: InitializeResult = serde_json::from_str(json).unwrap();
        assert_eq!(result.protocol_version, "1");
        assert_eq!(result.agent_info.version, "1");
    }

    #[test]
    fn test_content_part_text_serialization() {
        let part = ContentPart::text("hello world");
        assert_eq!(part.kind, "text");
        assert_eq!(part.text, "hello world");
        let json = serde_json::to_string(&part).unwrap();
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"hello world\""));
    }

    #[test]
    fn test_session_prompt_params_prompt_is_array() {
        // kiro-cli expects prompt as an array of content parts, not a plain string.
        let params = SessionPromptParams {
            session_id: "sess-1".to_string(),
            prompt: vec![ContentPart::text("do something")],
        };
        let json = serde_json::to_string(&params).unwrap();
        assert!(json.contains("\"prompt\":["));
        assert!(json.contains("\"type\":\"text\""));
        assert!(json.contains("\"text\":\"do something\""));
    }
}
