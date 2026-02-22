//! OpenCode server lifecycle: spawn, health check, and shutdown.
//!
//! Manages the opencode server as a child process. On startup, checks for a
//! running server via `GET /global/health`; if absent, spawns
//! `opencode serve --port <port> --hostname <hostname>` with LLM provider
//! credentials injected as environment variables. Sends SIGTERM on shutdown.

use std::process::Stdio;
use std::time::Duration;

use tokio::time::{sleep, timeout};

use crate::config::{AppConfig, ServerMode};
use crate::error::{ClawdMuxError, Result};
use crate::opencode::OpenCodeClient;

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
        let client = OpenCodeClient::new(base_url.clone(), None);

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
            ServerMode::Auto => Self::spawn_and_wait(config, base_url, on_status).await,
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
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        for (key, val) in &env_vars {
            cmd.env(key, val);
        }

        on_status("Starting opencode server...");
        let mut child = cmd
            .spawn()
            .map_err(|e| ClawdMuxError::Server(format!("failed to spawn opencode: {}", e)))?;

        tracing::info!(
            "Spawned opencode server (pid={:?}), waiting for health at {}",
            child.id(),
            base_url
        );

        let client = OpenCodeClient::new(base_url.clone(), None);
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
        let healthy = loop {
            attempt += 1;
            on_status(&format!(
                "Waiting for opencode server (attempt {} of {})...",
                attempt, max_attempts
            ));
            sleep(delay).await;
            if client.health().await.unwrap_or(false) {
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
            Ok(Self {
                child: Some(child),
                base_url,
            })
        } else {
            tracing::warn!("OpenCode server did not become healthy within 30s, killing");
            let _ = child.kill().await;
            Err(ClawdMuxError::Server(
                "opencode server did not become healthy within 30 seconds".to_string(),
            ))
        }
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
    /// Returns [`ClawdMuxError::Io`] for any I/O error encountered while
    /// signalling or waiting for the child process.
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
            },
            opencode: OpenCodeConfig {
                mode,
                hostname: "127.0.0.1".to_string(),
                port,
                password: None,
            },
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
}
