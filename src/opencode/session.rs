//! Session lifecycle: create, prompt, abort, fork, and diff retrieval.
//!
//! Wraps the opencode session API endpoints:
//! `POST /session`, `POST /session/:id/message`,
//! `DELETE /session/:id`, `POST /session/:id/fork`, `GET /session/:id/diff`.

use reqwest::Method;

use crate::error::{ClawdMuxError, Result};
use crate::opencode::types::{
    ContentPart, CreateSessionResponse, FileDiff, OpenCodeSession, SendMessageRequest,
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
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClawdMuxError::Api(body));
        }
        let created: CreateSessionResponse = resp.json().await?;
        Ok(created.0)
    }

    /// Sends a prompt to a session asynchronously.
    ///
    /// Sends `POST /session/{session_id}/message`. The agent name is forwarded so the
    /// server can route the request to the appropriate agent definition.
    /// Returns `Ok(())` on success; the server streams results via SSE.
    ///
    /// # Arguments
    ///
    /// * `session_id` - The ID of the target session.
    /// * `agent` - The pipeline agent to route the message to.
    /// * `prompt` - The text prompt to send.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Http`] on transport failure or [`ClawdMuxError::Api`]
    /// on a non-2xx response.
    pub async fn send_prompt_async(
        &self,
        session_id: &str,
        agent: &AgentKind,
        prompt: &str,
    ) -> Result<()> {
        let path = format!("/session/{session_id}/message");
        let body = SendMessageRequest {
            content: vec![ContentPart::Text {
                text: prompt.to_string(),
            }],
            agent: Some(agent.opencode_agent_name().to_string()),
        };
        let resp = self.request(Method::POST, &path).json(&body).send().await?;
        if !resp.status().is_success() {
            let error_body = resp.text().await.unwrap_or_default();
            return Err(ClawdMuxError::Api(error_body));
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
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClawdMuxError::Api(body));
        }
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
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClawdMuxError::Api(body));
        }
        let created: CreateSessionResponse = resp.json().await?;
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
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClawdMuxError::Api(body));
        }
        let diffs: Vec<FileDiff> = resp.json().await?;
        Ok(diffs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
            matches!(err, ClawdMuxError::Api(_)),
            "expected Api error, got: {err:?}"
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
            .with_body(r#"{"ok":true,"version":"1.0"}"#)
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
            .with_body(r#"{"ok":false,"version":"1.0"}"#)
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
    async fn test_send_prompt_async() {
        let mut server = Server::new_async().await;
        let mock = server
            .mock("POST", "/session/abc/message")
            .with_status(200)
            .create_async()
            .await;

        let client = make_client(&server.url());
        client
            .send_prompt_async("abc", &AgentKind::Implementation, "do the thing")
            .await
            .expect("should succeed");
        mock.assert_async().await;
    }
}
