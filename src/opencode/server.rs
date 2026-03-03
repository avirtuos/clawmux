//! OpenCode server lifecycle: spawn, health check, and shutdown.
//!
//! Manages the opencode server as a child process. On startup, checks for a
//! running server via `GET /global/health`; if absent, spawns
//! `opencode serve --port <port> --hostname <hostname>` with LLM provider
//! credentials injected as environment variables. Sends SIGTERM on shutdown.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::time::{sleep, timeout};

use crate::config::{AppConfig, ServerMode};
use crate::error::{ClawdMuxError, Result};
use crate::opencode::OpenCodeClient;

/// RAII guard that kills a child process by PID on drop.
///
/// Prevents orphaned child processes when `spawn_and_wait` is cancelled
/// (e.g. via `tokio::select!` on Ctrl+C during startup). Call [`ChildGuard::disarm`]
/// before returning to transfer ownership to [`OpenCodeServer`] or after explicitly
/// killing the process so the drop becomes a no-op.
struct ChildGuard {
    pid: Option<u32>,
}

impl ChildGuard {
    fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    /// Disarm the guard so the drop no longer sends SIGKILL.
    fn disarm(&mut self) {
        self.pid = None;
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            tracing::info!(
                "ChildGuard: killing orphaned opencode process (pid={})",
                pid
            );
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
            #[cfg(not(unix))]
            {
                //TODO: Task 5.4 - non-unix support gap: no portable SIGKILL-by-pid API.
                // On non-unix platforms the orphaned process is logged but not killed.
                // Consider storing the Child handle in ChildGuard instead of the raw PID.
                let _ = pid;
            }
        }
    }
}

/// Manages the opencode server process lifecycle.
///
/// In Auto mode, spawns `opencode serve` as a child process and polls health
/// until the server is ready. In External mode, verifies an already-running
/// server is reachable. On shutdown, sends SIGTERM and waits up to 5 seconds
/// for a clean exit before force-killing.
#[derive(Debug)]
pub struct OpenCodeServer {
    /// Child process handle, present only when this instance spawned the server.
    child: Option<tokio::process::Child>,
    /// Cached base URL in the form `"http://{hostname}:{port}"`.
    base_url: String,
}

impl OpenCodeServer {
    /// Build the base URL string from hostname and port components.
    fn build_base_url(hostname: &str, port: u16) -> String {
        format!("http://{}:{}", hostname, port)
    }

    /// Build Basic Auth credentials for `OpenCodeClient::new`.
    ///
    /// The opencode server uses empty-username Basic Auth; only the password
    /// matters.
    fn build_auth(password: &str) -> Option<(String, String)> {
        Some((String::new(), password.to_string()))
    }

    /// Start or verify the opencode server, returning a ready `OpenCodeServer`.
    ///
    /// Behavior depends on the configured [`ServerMode`]:
    ///
    /// - **External**: Performs a single health check. Returns `Ok` on success
    ///   or [`ClawdMuxError::Server`] if the server is unreachable.
    /// - **Auto**: Attempts a health check first; if the server is already running,
    ///   returns immediately with no child process. Otherwise spawns
    ///   `opencode serve --port <port> --hostname <hostname>`, polls health with
    ///   exponential backoff (100 ms initial, 2x multiplier, 2 s cap) for up to
    ///   30 seconds, then returns.
    ///
    /// The `on_status` callback is invoked at each phase boundary with a
    /// human-readable status string suitable for display in a loading screen.
    ///
    /// # Errors
    ///
    /// - [`ClawdMuxError::Server`] if External mode and server is unreachable.
    /// - [`ClawdMuxError::Server`] if Auto mode and `opencode` binary is not found
    ///   or the server fails to become healthy within 30 seconds.
    pub async fn ensure_running<F>(config: &AppConfig, mut on_status: F) -> Result<Self>
    where
        F: FnMut(&str),
    {
        let base_url = Self::build_base_url(&config.opencode.hostname, config.opencode.port);
        // Only attach credentials when the user has explicitly configured a password.
        // Without an explicit password, opencode is started unauthenticated and
        // requests are sent without an Authorization header, matching pre-password behaviour.
        let auth = if config.has_explicit_password() {
            Self::build_auth(&config.effective_opencode_password())
        } else {
            None
        };
        let client = OpenCodeClient::new(base_url.clone(), auth.clone());

        on_status("Checking for running opencode server...");
        let already_healthy = client.health().await.unwrap_or(false);
        if already_healthy {
            tracing::info!("OpenCode server already running at {}", base_url);
            on_status("Connected to opencode server");
            return Ok(Self {
                child: None,
                base_url,
            });
        }

        match config.opencode.mode {
            ServerMode::External => {
                tracing::warn!(
                    "External mode: opencode server not reachable at {}",
                    base_url
                );
                Err(ClawdMuxError::Server(format!(
                    "opencode server not reachable at {} (external mode)",
                    base_url
                )))
            }
            ServerMode::Auto => {
                if Self::is_port_occupied(&config.opencode.hostname, config.opencode.port).await {
                    on_status("Detected existing process on configured port...");
                    tracing::info!(
                        "Port {} is already in use, waiting for existing server to become healthy",
                        config.opencode.port
                    );
                    Self::wait_for_existing_server(base_url, auth, on_status).await
                } else {
                    Self::spawn_and_wait(config, base_url, on_status).await
                }
            }
        }
    }

    /// Probe whether something is already listening on the given address.
    ///
    /// Attempts a TCP connection with a 500 ms timeout. Returns `true` if a
    /// listener is detected, `false` if the connection fails or times out.
    async fn is_port_occupied(hostname: &str, port: u16) -> bool {
        tokio::time::timeout(
            Duration::from_millis(500),
            tokio::net::TcpStream::connect(format!("{}:{}", hostname, port)),
        )
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
    }

    /// Poll health with exponential backoff until an existing server is ready,
    /// without spawning a child process.
    ///
    /// Uses the same backoff schedule as [`spawn_and_wait`] (100 ms initial,
    /// 2x multiplier, 2 s cap, 30 s total). Returns `Ok(Self { child: None, .. })`
    /// when the server reports healthy.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Server`] if the server does not become healthy
    /// within 30 seconds.
    async fn wait_for_existing_server<F>(
        base_url: String,
        auth: Option<(String, String)>,
        mut on_status: F,
    ) -> Result<Self>
    where
        F: FnMut(&str),
    {
        let client = OpenCodeClient::new(base_url.clone(), auth);
        const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
        const INITIAL_DELAY: Duration = Duration::from_millis(100);
        const MAX_DELAY: Duration = Duration::from_millis(2000);

        let max_attempts: u32 = {
            let mut count = 0u32;
            let mut elapsed = Duration::ZERO;
            let mut d = INITIAL_DELAY;
            loop {
                count += 1;
                elapsed += d;
                if elapsed >= TOTAL_TIMEOUT {
                    break;
                }
                d = (d * 2).min(MAX_DELAY);
            }
            count
        };

        let deadline = tokio::time::Instant::now() + TOTAL_TIMEOUT;
        let mut delay = INITIAL_DELAY;
        let mut attempt: u32 = 0;
        let mut last_error: Option<String> = None;
        let healthy = loop {
            attempt += 1;
            let status_msg = match &last_error {
                Some(hint) => format!(
                    "Waiting for existing opencode server (attempt {} of {}; last error: {})...",
                    attempt, max_attempts, hint
                ),
                None => format!(
                    "Waiting for existing opencode server (attempt {} of {})...",
                    attempt, max_attempts
                ),
            };
            on_status(&status_msg);
            sleep(delay).await;

            let is_healthy = match client.health().await {
                Ok(true) => {
                    tracing::info!("Health check attempt {}: ok", attempt);
                    true
                }
                Ok(false) => {
                    tracing::info!("Health check attempt {}: not healthy", attempt);
                    false
                }
                Err(e) => {
                    tracing::info!("Health check attempt {}: {}", attempt, e);
                    last_error = Some(Self::error_hint(&e));
                    false
                }
            };
            if is_healthy {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            delay = (delay * 2).min(MAX_DELAY);
        };

        if healthy {
            tracing::info!("Connected to existing opencode server at {}", base_url);
            on_status("Connected to existing opencode server");
            Ok(Self {
                child: None,
                base_url,
            })
        } else {
            let port = base_url.rsplit(':').next().unwrap_or("unknown").to_string();
            Err(ClawdMuxError::Server(format!(
                "existing process on port {} did not become healthy within 30 seconds (last error: {})",
                port,
                last_error.as_deref().unwrap_or("unknown")
            )))
        }
    }

    /// Convert a health-check error into a short human-readable hint for the loading screen.
    fn error_hint(e: &ClawdMuxError) -> String {
        match e {
            ClawdMuxError::Http(req_err) => {
                if req_err.is_connect() {
                    "connection failed".to_string()
                } else if req_err.is_timeout() {
                    "request timed out".to_string()
                } else if req_err.is_decode() {
                    "unexpected response format".to_string()
                } else {
                    format!("HTTP error: {}", req_err)
                }
            }
            ClawdMuxError::Api { status, .. } => format!("HTTP {}", status),
            _ => e.to_string(),
        }
    }

    /// Spawn `opencode serve` and poll health with exponential backoff until
    /// the server is ready or the 30-second timeout expires.
    ///
    /// Calls `on_status` at each phase boundary and on every health-poll attempt.
    async fn spawn_and_wait<F>(
        config: &AppConfig,
        base_url: String,
        mut on_status: F,
    ) -> Result<Self>
    where
        F: FnMut(&str),
    {
        on_status("Locating opencode binary...");
        let opencode_bin = which::which("opencode")
            .map_err(|e| ClawdMuxError::Server(format!("opencode binary not found: {}", e)))?;

        let env_vars = config.global.env_vars_for_opencode();

        let mut cmd = tokio::process::Command::new(&opencode_bin);
        cmd.arg("serve")
            .arg("--port")
            .arg(config.opencode.port.to_string())
            .arg("--hostname")
            .arg(&config.opencode.hostname)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        for (key, val) in &env_vars {
            cmd.env(key, val);
        }
        // Only inject the password env var when the user has explicitly configured one.
        // Without this guard, spawning opencode with a default password forces auth on
        // all endpoints (including health checks), causing 401 failures at startup.
        let auth = if config.has_explicit_password() {
            let password = config.effective_opencode_password();
            cmd.env("OPENCODE_SERVER_PASSWORD", &password);
            Self::build_auth(&password)
        } else {
            None
        };

        tracing::info!(
            "Spawning opencode: {:?} serve --port {} --hostname {}",
            opencode_bin,
            config.opencode.port,
            config.opencode.hostname
        );
        on_status("Starting opencode server...");
        let mut child = cmd
            .spawn()
            .map_err(|e| ClawdMuxError::Server(format!("failed to spawn opencode: {}", e)))?;

        tracing::info!(
            "Spawned opencode server (pid={:?}), waiting for health at {}",
            child.id(),
            base_url
        );

        // Guard sends SIGKILL on drop if this function is cancelled (e.g. Ctrl+C
        // during startup via tokio::select!), preventing an orphaned process that
        // would hold the port and block the next run.
        let mut guard = ChildGuard::new(child.id());

        // Drain child stdout and stderr in the background so the pipe buffers
        // never fill up and block the child process.
        let child_stdout = child.stdout.take();
        let stdout_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let _stdout_task = {
            let buf = stdout_buf.clone();
            tokio::spawn(async move {
                if let Some(mut stdout) = child_stdout {
                    let mut output = Vec::new();
                    let mut tmp = [0u8; 4096];
                    loop {
                        match AsyncReadExt::read(&mut stdout, &mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if output.len() < 8192 {
                                    let remaining = 8192 - output.len();
                                    output.extend_from_slice(&tmp[..n.min(remaining)]);
                                }
                            }
                        }
                    }
                    if let Ok(text) = String::from_utf8(output) {
                        if let Ok(mut b) = buf.lock() {
                            *b = text;
                        }
                    }
                }
            })
        };

        let child_stderr = child.stderr.take();
        let stderr_buf: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let _stderr_task = {
            let buf = stderr_buf.clone();
            tokio::spawn(async move {
                if let Some(mut stderr) = child_stderr {
                    let mut output = Vec::new();
                    let mut tmp = [0u8; 4096];
                    loop {
                        match AsyncReadExt::read(&mut stderr, &mut tmp).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if output.len() < 8192 {
                                    let remaining = 8192 - output.len();
                                    output.extend_from_slice(&tmp[..n.min(remaining)]);
                                }
                            }
                        }
                    }
                    if let Ok(text) = String::from_utf8(output) {
                        if let Ok(mut b) = buf.lock() {
                            *b = text;
                        }
                    }
                }
            })
        };

        let client = OpenCodeClient::new(base_url.clone(), auth);
        const TOTAL_TIMEOUT: Duration = Duration::from_secs(30);
        const INITIAL_DELAY: Duration = Duration::from_millis(100);
        const MAX_DELAY: Duration = Duration::from_millis(2000);

        // Pre-compute total attempts by simulating the delay schedule.
        let max_attempts: u32 = {
            let mut count = 0u32;
            let mut elapsed = Duration::ZERO;
            let mut d = INITIAL_DELAY;
            loop {
                count += 1;
                elapsed += d;
                if elapsed >= TOTAL_TIMEOUT {
                    break;
                }
                d = (d * 2).min(MAX_DELAY);
            }
            count
        };

        let deadline = tokio::time::Instant::now() + TOTAL_TIMEOUT;
        let mut delay = INITIAL_DELAY;
        let mut attempt: u32 = 0;
        let mut last_error: Option<String> = None;
        let healthy = loop {
            attempt += 1;
            let status_msg = match &last_error {
                Some(hint) => format!(
                    "Waiting for opencode server (attempt {} of {}; last error: {})...",
                    attempt, max_attempts, hint
                ),
                None => format!(
                    "Waiting for opencode server (attempt {} of {})...",
                    attempt, max_attempts
                ),
            };
            on_status(&status_msg);
            sleep(delay).await;

            // Detect if the child process exited before it could become healthy.
            if let Some(exit_status) = child.try_wait().map_err(ClawdMuxError::Io)? {
                let stdout_output = Self::collected_output(&stdout_buf);
                let stderr_output = Self::collected_output(&stderr_buf);
                tracing::warn!("opencode server exited early with status: {}", exit_status);
                tracing::info!("opencode stdout (captured): {:?}", stdout_output.trim());
                tracing::info!("opencode stderr (captured): {:?}", stderr_output.trim());
                // Process already exited on its own — nothing left to kill.
                guard.disarm();
                return Err(ClawdMuxError::Server(format!(
                    "opencode server exited before becoming healthy (status: {}){}",
                    exit_status,
                    if stderr_output.is_empty() {
                        String::new()
                    } else {
                        format!(": {}", stderr_output.trim())
                    }
                )));
            }

            let is_healthy = match client.health().await {
                Ok(true) => {
                    tracing::info!("Health check attempt {}: ok", attempt);
                    true
                }
                Ok(false) => {
                    tracing::info!("Health check attempt {}: not healthy", attempt);
                    false
                }
                Err(e) => {
                    tracing::info!("Health check attempt {}: {}", attempt, e);
                    last_error = Some(Self::error_hint(&e));
                    false
                }
            };
            if is_healthy {
                break true;
            }
            if tokio::time::Instant::now() >= deadline {
                break false;
            }
            delay = (delay * 2).min(MAX_DELAY);
        };

        if healthy {
            tracing::info!("OpenCode server is healthy at {}", base_url);
            on_status("Connected to opencode server");
            // Disarm before moving child into Self — OpenCodeServer::shutdown owns cleanup now.
            guard.disarm();
            Ok(Self {
                child: Some(child),
                base_url,
            })
        } else {
            tracing::warn!(
                "OpenCode server did not become healthy within 30s (last error: {}), killing",
                last_error.as_deref().unwrap_or("unknown")
            );
            let _ = child.kill().await;
            // Disarm: we just killed the process explicitly.
            guard.disarm();
            // Give the drain tasks a moment to flush the final pipe output.
            sleep(Duration::from_millis(100)).await;
            let stdout_output = Self::collected_output(&stdout_buf);
            let stderr_output = Self::collected_output(&stderr_buf);
            tracing::info!("opencode stdout (captured): {:?}", stdout_output.trim());
            tracing::info!("opencode stderr (captured): {:?}", stderr_output.trim());
            Err(ClawdMuxError::Server(format!(
                "opencode server did not become healthy within 30 seconds (last error: {}){}",
                last_error.as_deref().unwrap_or("unknown"),
                if stderr_output.is_empty() {
                    String::new()
                } else {
                    format!("; stderr: {}", stderr_output.trim())
                }
            )))
        }
    }

    /// Read the text collected so far from a background stdout/stderr drain task.
    fn collected_output(buf: &Arc<Mutex<String>>) -> String {
        buf.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Returns the base URL of the managed opencode server.
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Gracefully shut down the opencode server.
    ///
    /// In External mode (no child process), this is a no-op.
    ///
    /// In Auto mode, sends SIGTERM to the child process and waits up to 5
    /// seconds for it to exit cleanly. If the process has not exited within
    /// the deadline, it is force-killed via [`tokio::process::Child::kill`].
    ///
    /// # Errors
    ///
    /// Shutdown is best-effort: I/O errors encountered while signalling or
    /// waiting for the child process are logged and then discarded. This
    /// function always returns `Ok(())`.
    pub async fn shutdown(&mut self) -> Result<()> {
        let Some(child) = self.child.as_mut() else {
            return Ok(());
        };

        #[cfg(unix)]
        {
            if let Some(pid) = child.id() {
                // Safety: `pid` is a valid OS PID from our spawned child process.
                let _ = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
                tracing::info!("Sent SIGTERM to opencode server (pid={})", pid);
            }
        }

        #[cfg(not(unix))]
        {
            let _ = child.kill().await;
        }

        match timeout(Duration::from_secs(5), child.wait()).await {
            Ok(Ok(status)) => {
                tracing::info!("OpenCode server exited with status: {}", status);
            }
            Ok(Err(e)) => {
                tracing::warn!("Error waiting for opencode server: {}", e);
            }
            Err(_elapsed) => {
                tracing::warn!("OpenCode server did not exit within 5s, force killing");
                let _ = child.kill().await;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::providers::{GlobalConfig, ProviderSection};
    use crate::config::{AppConfig, OpenCodeConfig, ServerMode};

    fn make_config(mode: ServerMode, port: u16) -> AppConfig {
        AppConfig {
            global: GlobalConfig {
                provider: ProviderSection::default(),
                opencode_password: None,
            },
            backend: crate::config::BackendKind::default(),
            opencode: OpenCodeConfig {
                mode,
                hostname: "127.0.0.1".to_string(),
                port,
                password: None,
            },
            kiro: crate::config::KiroConfig::default(),
            workflow: crate::config::WorkflowConfig::default(),
        }
    }

    #[test]
    fn test_base_url_format() {
        assert_eq!(
            OpenCodeServer::build_base_url("127.0.0.1", 4096),
            "http://127.0.0.1:4096"
        );
        assert_eq!(
            OpenCodeServer::build_base_url("localhost", 8080),
            "http://localhost:8080"
        );
        assert_eq!(
            OpenCodeServer::build_base_url("0.0.0.0", 1234),
            "http://0.0.0.0:1234"
        );
    }

    #[tokio::test]
    async fn test_external_mode_health_fail() {
        // Start a mockito server but register no routes -- any request returns 501.
        let server = mockito::Server::new_async().await;
        let port = server.socket_address().port();

        let config = make_config(ServerMode::External, port);
        let result = OpenCodeServer::ensure_running(&config, |_| {}).await;
        assert!(
            matches!(result, Err(ClawdMuxError::Server(_))),
            "Expected Server error in external mode with unhealthy server, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_external_mode_health_ok() {
        let mut server = mockito::Server::new_async().await;
        let port = server.socket_address().port();
        let _m = server
            .mock("GET", "/global/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"ok":true,"version":"1.0"}"#)
            .create_async()
            .await;

        let config = make_config(ServerMode::External, port);
        let result = OpenCodeServer::ensure_running(&config, |_| {}).await;
        assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
        let srv = result.unwrap();
        assert!(
            srv.child.is_none(),
            "External mode should not spawn a child process"
        );
        assert_eq!(srv.base_url(), format!("http://127.0.0.1:{}", port));
    }

    #[test]
    fn test_error_hint_format() {
        // Api errors show the HTTP status code.
        let api_err = ClawdMuxError::Api {
            status: 500,
            body: "internal server error".to_string(),
        };
        assert_eq!(OpenCodeServer::error_hint(&api_err), "HTTP 500");

        // Non-Http/Api errors show their Display output.
        let server_err = ClawdMuxError::Server("process crashed".to_string());
        let hint = OpenCodeServer::error_hint(&server_err);
        assert!(
            hint.contains("process crashed"),
            "expected hint to contain 'process crashed', got: {hint}"
        );

        let sse_err = ClawdMuxError::Sse("stream broken".to_string());
        let hint = OpenCodeServer::error_hint(&sse_err);
        assert!(
            hint.contains("stream broken"),
            "expected hint to contain 'stream broken', got: {hint}"
        );
    }

    #[tokio::test]
    async fn test_error_hint_http_connection_failed() {
        // Bind to a free port then immediately drop the listener so the port is
        // unreachable; the resulting reqwest error should have is_connect() == true.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            listener.local_addr().expect("local_addr").port()
            // listener drops here, closing the port
        };
        let err = reqwest::Client::new()
            .get(format!("http://127.0.0.1:{}/test", port))
            .timeout(std::time::Duration::from_millis(500))
            .send()
            .await
            .expect_err("should fail to connect");
        let claw_err = ClawdMuxError::Http(err);
        assert_eq!(OpenCodeServer::error_hint(&claw_err), "connection failed");
    }

    #[tokio::test]
    async fn test_external_mode_health_unparseable_body_succeeds() {
        // A 200 response with a non-standard body should still succeed in External
        // mode — the server is reachable so health() treats 200 as healthy.
        let mut server = mockito::Server::new_async().await;
        let port = server.socket_address().port();
        let _m = server
            .mock("GET", "/global/health")
            .with_status(200)
            .with_header("content-type", "text/plain")
            .with_body("ok")
            .create_async()
            .await;

        let config = make_config(ServerMode::External, port);
        let result = OpenCodeServer::ensure_running(&config, |_| {}).await;
        assert!(
            result.is_ok(),
            "Expected Ok even with unparseable health body, got: {:?}",
            result
        );
    }

    #[tokio::test]
    async fn test_ensure_running_reports_status() {
        use std::cell::RefCell;

        let mut server = mockito::Server::new_async().await;
        let port = server.socket_address().port();
        let _m = server
            .mock("GET", "/global/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"ok":true,"version":"1.0"}"#)
            .create_async()
            .await;

        let config = make_config(ServerMode::External, port);
        let statuses: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let result = OpenCodeServer::ensure_running(&config, |s| {
            statuses.borrow_mut().push(s.to_string());
        })
        .await;

        assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
        let collected = statuses.into_inner();
        let checking_pos = collected
            .iter()
            .position(|s| s == "Checking for running opencode server...");
        let connected_pos = collected
            .iter()
            .position(|s| s == "Connected to opencode server");
        assert!(
            checking_pos.is_some(),
            "Expected 'Checking for running opencode server...' in statuses: {:?}",
            collected
        );
        assert!(
            connected_pos.is_some(),
            "Expected 'Connected to opencode server' in statuses: {:?}",
            collected
        );
        assert!(
            checking_pos.unwrap() < connected_pos.unwrap(),
            "Expected 'Checking...' before 'Connected' in statuses: {:?}",
            collected
        );
    }

    #[tokio::test]
    async fn test_is_port_occupied_detects_listener() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind ephemeral port");
        let port = listener.local_addr().expect("local_addr").port();
        // Listener is still bound — port should be occupied.
        assert!(
            OpenCodeServer::is_port_occupied("127.0.0.1", port).await,
            "Expected is_port_occupied to return true while listener is active"
        );
    }

    #[tokio::test]
    async fn test_is_port_occupied_returns_false_for_free_port() {
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            listener.local_addr().expect("local_addr").port()
            // listener drops here, releasing the port
        };
        assert!(
            !OpenCodeServer::is_port_occupied("127.0.0.1", port).await,
            "Expected is_port_occupied to return false for a released port"
        );
    }

    #[tokio::test]
    async fn test_auto_mode_spawn_binary_not_found() {
        // Use a port that is not in use so ensure_running proceeds to spawn_and_wait.
        let port = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap().port()
            // drops here, releasing the port
        };
        let config = make_config(ServerMode::Auto, port);

        // Temporarily clear PATH so which::which("opencode") cannot find the binary.
        // NOTE: this mutates a process-global variable; tests that depend on PATH
        // must not run concurrently with this one. In practice cargo test runs each
        // #[tokio::test] on its own thread but within the same process, so we
        // restore PATH immediately after the call.
        let original_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", "");

        let result = OpenCodeServer::ensure_running(&config, |_| {}).await;

        std::env::set_var("PATH", &original_path);

        assert!(
            matches!(result, Err(ClawdMuxError::Server(_))),
            "Expected Server error when binary not found, got: {:?}",
            result
        );
        if let Err(ClawdMuxError::Server(ref msg)) = result {
            assert!(
                msg.contains("binary not found"),
                "Expected 'binary not found' in error message, got: {:?}",
                msg
            );
        }
    }

    #[tokio::test]
    async fn test_auto_mode_reuses_existing_server_on_occupied_port() {
        use std::cell::RefCell;

        let mut server = mockito::Server::new_async().await;
        let port = server.socket_address().port();

        // First health check (initial probe in ensure_running) returns 503.
        let _m1 = server
            .mock("GET", "/global/health")
            .with_status(503)
            .with_header("content-type", "application/json")
            .with_body(r#"{"ok":false}"#)
            .expect(1)
            .create_async()
            .await;

        // Second health check (inside wait_for_existing_server) returns 200.
        let _m2 = server
            .mock("GET", "/global/health")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"ok":true,"version":"1.0"}"#)
            .expect(1)
            .create_async()
            .await;

        let config = make_config(ServerMode::Auto, port);
        let statuses: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let result = OpenCodeServer::ensure_running(&config, |s| {
            statuses.borrow_mut().push(s.to_string());
        })
        .await;

        assert!(result.is_ok(), "Expected Ok, got: {:?}", result);
        let srv = result.unwrap();
        assert!(
            srv.child.is_none(),
            "Auto mode with occupied port should not spawn a child process"
        );

        let collected = statuses.into_inner();
        let detected = collected
            .iter()
            .any(|s| s.contains("Detected existing process"));
        let connected = collected
            .iter()
            .any(|s| s == "Connected to existing opencode server");
        assert!(
            detected,
            "Expected 'Detected existing process' status message, got: {:?}",
            collected
        );
        assert!(
            connected,
            "Expected 'Connected to existing opencode server' status message, got: {:?}",
            collected
        );
    }
}
