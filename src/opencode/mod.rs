//! OpenCode HTTP client and SSE event listener.
//!
//! The `OpenCodeClient` communicates with an opencode server over HTTP + SSE,
//! providing session management, prompt sending, diff retrieval, and health checks.

pub mod events;
pub mod server;
pub mod session;
pub mod types;

use crate::error::{ClawdMuxError, Result};
use crate::opencode::types::HealthResponse;
use reqwest::Method;

/// HTTP client for communicating with the opencode REST API.
///
/// Wraps a `reqwest::Client` with a configurable base URL and optional Basic Auth
/// credentials. Session operations are implemented in `src/opencode/session.rs`.
#[allow(dead_code)]
pub struct OpenCodeClient {
    /// The underlying async HTTP client.
    http: reqwest::Client,
    /// Base URL of the opencode server (e.g. `"http://localhost:4242"`).
    base_url: String,
    /// Optional Basic Auth credentials as `(username, password)`.
    auth: Option<(String, String)>,
}

#[allow(dead_code)]
impl OpenCodeClient {
    /// Creates a new `OpenCodeClient`.
    ///
    /// # Arguments
    ///
    /// * `base_url` - Base URL of the opencode server (without trailing slash).
    /// * `auth` - Optional Basic Auth credentials as `(username, password)`.
    pub fn new(base_url: String, auth: Option<(String, String)>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url,
            auth,
        }
    }

    /// Builds a `RequestBuilder` for the given HTTP method and path.
    ///
    /// Appends `path` to `base_url` and attaches Basic Auth headers when credentials
    /// are configured.
    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        let url = format!("{}{}", self.base_url, path);
        let builder = self.http.request(method, &url);
        if let Some((ref user, ref pass)) = self.auth {
            builder.basic_auth(user, Some(pass))
        } else {
            builder
        }
    }

    /// Checks the health of the opencode server.
    ///
    /// Sends `GET /global/health` and returns `true` if the server reports healthy.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Http`] on transport failure or [`ClawdMuxError::Api`]
    /// on a non-2xx response.
    pub async fn health(&self) -> Result<bool> {
        let resp = self.request(Method::GET, "/global/health").send().await?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(ClawdMuxError::Api(body));
        }
        let health: HealthResponse = resp.json().await?;
        Ok(health.ok)
    }
}
