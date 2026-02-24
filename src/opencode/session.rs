//! Session lifecycle: create, prompt, abort, fork, and diff retrieval.
//!
//! Wraps the opencode session API endpoints:
//! `POST /session`, `POST /session/:id/prompt_async`,
//! `DELETE /session/:id`, `POST /session/:id/fork`, `GET /session/:id/diff`.

use reqwest::Method;

use crate::error::{ClawdMuxError, Result};
use crate::opencode::types::{
    ContentPart, CreateSessionResponse, FileDiff, MessageEntry, OpenCodeSession,
    SendMessageRequest, SessionStatusResponse,
};
use crate::workflow::agents::AgentKind;

use super::OpenCodeClient;

#[allow(dead_code)]
impl OpenCodeClient {
    /// Creates a new opencode session.
    ///
    /// Sends `POST /session` with an empty JSON body and returns the created session.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Http`] on transport failure or [`ClawdMuxError::Api`]
    /// on a non-2xx response.
    pub async fn create_session(&self) -> Result<OpenCodeSession> {
        let resp = self
            .request(Method::POST, "/session")
            .json(&serde_json::json!({}))
            .send()
            .await?;
        let resp = self.check_response(resp).await?;
        let body = resp.text().await?;
        tracing::debug!("create_session response body: {}", body);
        let created: CreateSessionResponse = serde_json::from_str(&body).map_err(|e| {
            tracing::warn!(
                "Failed to parse create_session response: {}; body: {}",
                e,
                body
            );
            ClawdMuxError::Json(e)
        })?;
        Ok(created.0)
    }

    /// Sends a prompt to a session asynchronously.
    ///
    /// Sends `POST /session/{session_id}/prompt_async`. When `agent` is `Some`, the agent
    /// name is forwarded so the server can route the request to the appropriate agent
    /// definition. When `None`, the server uses its default model without a custom agent.
    /// Returns `Ok(())` on success; the server streams results via SSE.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the target session.
    /// * `agent` - The pipeline agent to route the message to, or `None` for the default.
    /// * `prompt` - The text prompt to send.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Http`] on transport failure or [`ClawdMuxError::Api`]
    /// on a non-2xx response.
    pub async fn send_prompt_async(
        &self,
        session_id: &str,
        agent: Option<&AgentKind>,
        prompt: &str,
    ) -> Result<()> {
        let path = format!("/session/{session_id}/prompt_async");
        let body = SendMessageRequest {
            parts: vec![ContentPart::Text {
                text: prompt.to_string(),
            }],
            agent: agent.map(|a| a.opencode_agent_name().to_string()),
        };
        tracing::debug!(
            "send_prompt_async: session={}, agent={}",
            session_id,
            agent.map(|a| a.display_name()).unwrap_or("(default)")
        );
        let resp = self.request(Method::POST, &path).json(&body).send().await?;
        let status = resp.status();
        let resp_body = resp.text().await.unwrap_or_default();
        tracing::debug!(
            "send_prompt_async response: status={}, body={}",
            status,
            resp_body
        );
        if !status.is_success() {
            return Err(ClawdMuxError::Api {
                status: status.as_u16(),
                body: resp_body,
            });
        }
        Ok(())
    }

    /// Aborts an active session.
    ///
    /// Sends `DELETE /session/{session_id}` and returns `Ok(())` on success.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the session to abort.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Http`] on transport failure or [`ClawdMuxError::Api`]
    /// on a non-2xx response.
    pub async fn abort_session(&self, session_id: &str) -> Result<()> {
        let path = format!("/session/{session_id}");
        let resp = self.request(Method::DELETE, &path).send().await?;
        self.check_response(resp).await?;
        Ok(())
    }

    /// Forks an existing session into a new independent session.
    ///
    /// Sends `POST /session/{session_id}/fork` and returns the newly created session.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the session to fork.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Http`] on transport failure or [`ClawdMuxError::Api`]
    /// on a non-2xx response.
    pub async fn fork_session(&self, session_id: &str) -> Result<OpenCodeSession> {
        let path = format!("/session/{session_id}/fork");
        let resp = self.request(Method::POST, &path).send().await?;
        let resp = self.check_response(resp).await?;
        let body = resp.text().await?;
        tracing::debug!("fork_session response body: {}", body);
        let created: CreateSessionResponse = serde_json::from_str(&body).map_err(|e| {
            tracing::warn!(
                "Failed to parse fork_session response: {}; body: {}",
                e,
                body
            );
            ClawdMuxError::Json(e)
        })?;
        Ok(created.0)
    }

    /// Retrieves all file diffs produced by a session.
    ///
    /// Sends `GET /session/{session_id}/diff` and returns all file-level diffs.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the session whose diffs to retrieve.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Http`] on transport failure or [`ClawdMuxError::Api`]
    /// on a non-2xx response.
    pub async fn get_session_diffs(&self, session_id: &str) -> Result<Vec<FileDiff>> {
        let path = format!("/session/{session_id}/diff");
        let resp = self.request(Method::GET, &path).send().await?;
        let resp = self.check_response(resp).await?;
        let diffs: Vec<FileDiff> = resp.json().await?;
        Ok(diffs)
    }

    /// Retrieves the runtime status of all active sessions.
    ///
    /// Sends `GET /session/status` and returns a map of session ID to status.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Http`] on transport failure or [`ClawdMuxError::Api`]
    /// on a non-2xx response.
    pub async fn get_session_statuses(&self) -> Result<SessionStatusResponse> {
        let resp = self.request(Method::GET, "/session/status").send().await?;
        let resp = self.check_response(resp).await?;
        let statuses: SessionStatusResponse = resp.json().await?;
        Ok(statuses)
    }

    /// Retrieves all messages for a session.
    ///
    /// Sends `GET /session/{session_id}/message` and returns the message list.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the session whose messages to retrieve.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Http`] on transport failure or [`ClawdMuxError::Api`]
    /// on a non-2xx response.
    pub async fn get_session_messages(&self, session_id: &str) -> Result<Vec<MessageEntry>> {
        let path = format!("/session/{session_id}/message");
        let resp = self.request(Method::GET, &path).send().await?;
        let resp = self.check_response(resp).await?;
        let body = resp.text().await?;
        tracing::debug!("get_session_messages response body: {}", body);
        let messages: Vec<MessageEntry> = serde_json::from_str(&body).map_err(|e| {
            tracing::warn!(
                "Failed to parse get_session_messages response: {}; body: {}",
                e,
                body
            );
            ClawdMuxError::Json(e)
        })?;
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ClawdMuxError;
    use mockito::Server;

    fn make_client(url: &str) -> OpenCodeClient {
        OpenCodeClient::new(url.to_string(), None)
    }

    #[tokio::test]
    async fn test_create_session_success() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/session")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"sess-abc","createdAt":"2024-01-01T00:00:00Z"}"#)
            .create_async()
            .await;

        let client = make_client(&server.url());
        let session = client.create_session().await.expect("should succeed");
        assert_eq!(session.id, "sess-abc");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_create_session_server_error() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/session")
            .with_status(500)
            .with_body("internal server error")
            .create_async()
            .await;

        let client = make_client(&server.url());
        let err = client.create_session().await.expect_err("should fail");
        assert!(
            matches!(err, ClawdMuxError::Api { status: 500, .. }),
            "expected Api error with status 500, got: {err:?}"
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_health_true() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/global/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"healthy":true,"version":"1.0"}"#)
            .create_async()
            .await;

        let client = make_client(&server.url());
        let result = client.health().await.expect("should succeed");
        assert!(result);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_health_false() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/global/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"healthy":false,"version":"1.0"}"#)
            .create_async()
            .await;

        let client = make_client(&server.url());
        let result = client.health().await.expect("should succeed");
        assert!(!result);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_abort_session() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("DELETE", "/session/abc")
            .with_status(200)
            .create_async()
            .await;

        let client = make_client(&server.url());
        client.abort_session("abc").await.expect("should succeed");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_session_diffs_empty() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/session/abc/diff")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("[]")
            .create_async()
            .await;

        let client = make_client(&server.url());
        let diffs = client
            .get_session_diffs("abc")
            .await
            .expect("should succeed");
        assert!(diffs.is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_prompt_async_with_agent() {
        // When Some(agent) is given the agent name is included in the request body.
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/session/abc/prompt_async")
            .match_body(
                r#"{"parts":[{"type":"text","text":"do the thing"}],"agent":"clawdmux/implementation"}"#,
            )
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server.url());
        client
            .send_prompt_async("abc", Some(&AgentKind::Implementation), "do the thing")
            .await
            .expect("should succeed");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_send_prompt_async_no_agent() {
        // When None is given the agent field is omitted from the request body.
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/session/abc/prompt_async")
            .match_body(r#"{"parts":[{"type":"text","text":"fix this"}]}"#)
            .with_status(204)
            .create_async()
            .await;

        let client = make_client(&server.url());
        client
            .send_prompt_async("abc", None, "fix this")
            .await
            .expect("should succeed");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_health_unparseable_body_returns_true() {
        // A 200 response whose body is not valid HealthResponse JSON should still
        // return Ok(true), since the server is clearly reachable and alive.
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/global/health")
            .with_status(200)
            .with_header("content-type", "text/plain")
            .with_body("ok")
            .create_async()
            .await;

        let client = make_client(&server.url());
        let result = client.health().await.expect("should succeed");
        assert!(result, "200 with non-JSON body should return Ok(true)");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_request_sends_basic_auth() {
        let mut server = Server::new_async().await;
        // "user:pass" encodes to "dXNlcjpwYXNz" in base64.
        let mock = server
            .mock("GET", "/global/health")
            .match_header("Authorization", "Basic dXNlcjpwYXNz")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"ok":true,"version":"1.0"}"#)
            .create_async()
            .await;

        let client =
            OpenCodeClient::new(server.url(), Some(("user".to_string(), "pass".to_string())));
        client.health().await.expect("should succeed");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_fork_session_success() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/session/abc/fork")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"id":"sess-forked","createdAt":"2024-01-01T00:00:00Z"}"#)
            .create_async()
            .await;

        let client = make_client(&server.url());
        let session = client.fork_session("abc").await.expect("should succeed");
        assert_eq!(session.id, "sess-forked");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_session_statuses_success() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/session/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"sess-1":{"type":"idle"},"sess-2":{"type":"busy"}}"#)
            .create_async()
            .await;

        let client = make_client(&server.url());
        let statuses = client.get_session_statuses().await.expect("should succeed");
        assert_eq!(
            statuses.get("sess-1"),
            Some(&crate::opencode::types::SessionStatus::Idle)
        );
        assert_eq!(
            statuses.get("sess-2"),
            Some(&crate::opencode::types::SessionStatus::Busy)
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_session_statuses_empty() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/session/status")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body("{}")
            .create_async()
            .await;

        let client = make_client(&server.url());
        let statuses = client.get_session_statuses().await.expect("should succeed");
        assert!(statuses.is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_session_messages_success() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/session/abc/message")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[{"info":{"role":"user","finish":null},"parts":[]},{"info":{"role":"assistant","finish":"end_turn"},"parts":[]}]"#,
            )
            .create_async()
            .await;

        let client = make_client(&server.url());
        let messages = client
            .get_session_messages("abc")
            .await
            .expect("should succeed");
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0].info.role,
            crate::opencode::types::MessageRole::User
        );
        assert_eq!(
            messages[1].info.role,
            crate::opencode::types::MessageRole::Assistant
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_get_session_messages_with_error() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("GET", "/session/abc/message")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[{"info":{"role":"assistant","finish":"error","error":{"message":"agent.model on undefined"}},"parts":[]}]"#,
            )
            .create_async()
            .await;

        let client = make_client(&server.url());
        let messages = client
            .get_session_messages("abc")
            .await
            .expect("should succeed");
        assert_eq!(messages.len(), 1);
        let err = messages[0].info.error.as_ref().expect("error");
        assert_eq!(err.message.as_deref(), Some("agent.model on undefined"));
        mock.assert_async().await;
    }
}
