//! App configuration loading and opencode agent definition management.
//!
//! Loads the global config (`~/.config/clawdmux/config.toml`) and the
//! project-level config (`.clawdmux/config.toml`), and exposes the merged
//! [`AppConfig`] used throughout the application.

pub mod init;
pub mod providers;

pub use providers::GlobalConfig;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ClawdMuxError, Result};

/// Whether clawdmux manages the opencode server process itself or connects to
/// an already-running external instance.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServerMode {
    /// ClawdMux launches and manages the opencode server automatically.
    #[default]
    Auto,
    /// ClawdMux connects to an externally managed opencode server.
    External,
}

/// Which agent backend ClawdMux uses for session operations.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// Use the OpenCode HTTP REST + SSE backend (default).
    #[default]
    OpenCode,
    /// Use the kiro-cli ACP (JSON-RPC 2.0 over stdin/stdout) backend.
    Kiro,
}

/// Configuration for the kiro-cli backend.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct KiroConfig {
    /// Optional path to the kiro binary.
    ///
    /// When `None`, ClawdMux searches `PATH` for `kiro`.
    pub binary: Option<String>,
}

/// Project-level opencode connection settings from `.clawdmux/config.toml`.
#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct OpenCodeConfig {
    /// How the opencode server is managed.
    pub mode: ServerMode,
    /// Hostname or IP address of the opencode server.
    pub hostname: String,
    /// TCP port the opencode server listens on.
    pub port: u16,
    /// Optional password for the opencode server's API.
    pub password: Option<String>,
}

impl Default for OpenCodeConfig {
    fn default() -> Self {
        Self {
            mode: ServerMode::Auto,
            hostname: "127.0.0.1".to_string(),
            port: 4096,
            password: None,
        }
    }
}

/// Workflow behavior settings from `.clawdmux/config.toml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct WorkflowConfig {
    /// When `true`, require human approval before starting the next agent.
    ///
    /// The human presses `n` on the Team Status tab (Tab 5) to approve.
    /// Defaults to `true` so intermediate results can be inspected.
    pub approval_gate: bool,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            approval_gate: true,
        }
    }
}

/// Private wrapper for deserializing the project-level TOML, which uses an
/// `[opencode]` table rather than bare top-level keys.
#[derive(Debug, Deserialize, Default)]
struct ProjectConfigFile {
    #[serde(default)]
    backend: BackendKind,
    #[serde(default)]
    opencode: OpenCodeConfig,
    #[serde(default)]
    kiro: KiroConfig,
    #[serde(default)]
    workflow: WorkflowConfig,
}

/// Merged application configuration combining global and project-level settings.
///
/// Constructed by [`AppConfig::load`] and passed throughout the application.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct AppConfig {
    /// Global LLM provider credentials.
    pub global: GlobalConfig,
    /// Which agent backend to use for session operations.
    pub backend: BackendKind,
    /// Opencode server connection settings for the current project.
    pub opencode: OpenCodeConfig,
    /// Kiro-cli backend settings for the current project.
    pub kiro: KiroConfig,
    /// Workflow behavior settings.
    pub workflow: WorkflowConfig,
}

#[allow(dead_code)]
impl AppConfig {
    /// Resolve the effective opencode server password.
    ///
    /// Precedence (highest to lowest):
    /// 1. `global.opencode_password` — set in `~/.config/clawdmux/config.toml`
    /// 2. `opencode.password` — set in `.clawdmux/config.toml`
    /// 3. Hardcoded default: `"clawdmux-default-pw"`
    pub fn effective_opencode_password(&self) -> String {
        self.global
            .opencode_password
            .clone()
            .or_else(|| self.opencode.password.clone())
            .unwrap_or_else(|| "clawdmux-default-pw".to_string())
    }

    /// Returns `true` if a password was explicitly configured by the user.
    ///
    /// When `false`, no password should be injected into the spawned opencode
    /// process and no auth credentials should be sent on API requests, preserving
    /// the same behaviour as a vanilla (no-password) opencode server.
    pub fn has_explicit_password(&self) -> bool {
        self.global.opencode_password.is_some() || self.opencode.password.is_some()
    }

    /// Load the merged application config.
    ///
    /// Reads the global config from `~/.config/clawdmux/config.toml` and the
    /// project config from `{project_root}/.clawdmux/config.toml`. Missing
    /// config files are treated as an empty config (defaults are used). Other
    /// IO errors or TOML parse errors are propagated.
    ///
    /// # Errors
    ///
    /// Returns [`ClawdMuxError::Internal`] if the platform config directory
    /// cannot be resolved. Returns [`ClawdMuxError::Io`] for unexpected IO
    /// failures and [`ClawdMuxError::Config`] for malformed TOML.
    pub fn load(project_root: &Path) -> Result<Self> {
        let global_config_dir = dirs::config_dir().ok_or_else(|| {
            ClawdMuxError::Internal("could not determine platform config directory".to_string())
        })?;
        let global_path = global_config_dir.join("clawdmux").join("config.toml");
        Self::load_from(&global_path, project_root)
    }

    /// Internal loader that accepts an explicit global config path.
    ///
    /// Separated from [`Self::load`] so tests can supply a temporary directory
    /// path without touching the real `~/.config/clawdmux/config.toml`.
    fn load_from(global_config_path: &Path, project_root: &Path) -> Result<Self> {
        // --- Global config ---
        let global = match GlobalConfig::load(global_config_path) {
            Ok(cfg) => cfg,
            Err(ClawdMuxError::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::info!(
                    path = %global_config_path.display(),
                    "global config not found, using defaults"
                );
                GlobalConfig::default()
            }
            Err(e) => return Err(e),
        };

        // --- Project config ---
        let project_config_path = project_root.join(".clawdmux").join("config.toml");

        let (backend, opencode, kiro, workflow) =
            match std::fs::read_to_string(&project_config_path) {
                Ok(contents) => {
                    let file: ProjectConfigFile = toml::from_str(&contents)?;
                    (file.backend, file.opencode, file.kiro, file.workflow)
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    tracing::info!(
                        path = %project_config_path.display(),
                        "project config not found, using defaults"
                    );
                    (
                        BackendKind::default(),
                        OpenCodeConfig::default(),
                        KiroConfig::default(),
                        WorkflowConfig::default(),
                    )
                }
                Err(e) => return Err(ClawdMuxError::Io(e)),
            };

        Ok(AppConfig {
            global,
            backend,
            opencode,
            kiro,
            workflow,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    #[test]
    fn test_opencode_config_defaults() {
        let config: OpenCodeConfig = toml::from_str("").unwrap();
        assert_eq!(config.mode, ServerMode::Auto);
        assert_eq!(config.hostname, "127.0.0.1");
        assert_eq!(config.port, 4096);
        assert!(config.password.is_none());
    }

    #[test]
    fn test_opencode_config_custom() {
        let toml = r#"
mode = "external"
hostname = "10.0.0.1"
port = 8080
password = "s3cr3t"
"#;
        let config: OpenCodeConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.mode, ServerMode::External);
        assert_eq!(config.hostname, "10.0.0.1");
        assert_eq!(config.port, 8080);
        assert_eq!(config.password.as_deref(), Some("s3cr3t"));
    }

    #[test]
    fn test_server_mode_serde() {
        #[derive(Debug, Deserialize, Serialize, PartialEq)]
        struct Wrapper {
            mode: ServerMode,
        }

        let auto_toml = "mode = \"auto\"\n";
        let ext_toml = "mode = \"external\"\n";

        let auto: Wrapper = toml::from_str(auto_toml).unwrap();
        assert_eq!(auto.mode, ServerMode::Auto);

        let ext: Wrapper = toml::from_str(ext_toml).unwrap();
        assert_eq!(ext.mode, ServerMode::External);

        // Round-trip
        let serialized = toml::to_string_pretty(&auto).unwrap();
        assert!(serialized.contains("auto"));
        let serialized_ext = toml::to_string_pretty(&ext).unwrap();
        assert!(serialized_ext.contains("external"));
    }

    #[test]
    fn test_app_config_load_missing_project() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        // Neither global nor project config exists; both should use defaults.
        let global_path = global_dir.path().join("config.toml");
        let config = AppConfig::load_from(&global_path, project_dir.path()).unwrap();
        assert_eq!(config.opencode.hostname, "127.0.0.1");
        assert_eq!(config.opencode.port, 4096);
        assert_eq!(config.opencode.mode, ServerMode::Auto);
        assert!(config.opencode.password.is_none());
    }

    #[test]
    fn test_app_config_load_with_project_config() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        let clawdmux_dir = project_dir.path().join(".clawdmux");
        std::fs::create_dir_all(&clawdmux_dir).unwrap();
        let config_path = clawdmux_dir.join("config.toml");
        std::fs::write(
            &config_path,
            r#"
[opencode]
mode = "external"
hostname = "192.168.1.50"
port = 9000
password = "mypassword"
"#,
        )
        .unwrap();

        let global_path = global_dir.path().join("config.toml");
        let config = AppConfig::load_from(&global_path, project_dir.path()).unwrap();
        assert_eq!(config.opencode.mode, ServerMode::External);
        assert_eq!(config.opencode.hostname, "192.168.1.50");
        assert_eq!(config.opencode.port, 9000);
        assert_eq!(config.opencode.password.as_deref(), Some("mypassword"));
    }

    #[test]
    fn test_server_mode_invalid() {
        let toml = r#"mode = "bogus""#;
        let result: std::result::Result<OpenCodeConfig, _> = toml::from_str(toml);
        // toml::from_str returns a toml::de::Error; map through ClawdMuxError::Config
        // by mimicking what AppConfig::load_from does with the project config.
        let err = result.map_err(ClawdMuxError::from);
        assert!(
            matches!(err, Err(ClawdMuxError::Config(_))),
            "expected Config error for unknown server mode, got: {:?}",
            err
        );
    }

    fn make_app_config(global_pw: Option<&str>, project_pw: Option<&str>) -> AppConfig {
        use crate::config::providers::{GlobalConfig, ProviderSection};
        AppConfig {
            global: GlobalConfig {
                provider: ProviderSection::default(),
                opencode_password: global_pw.map(str::to_string),
            },
            backend: BackendKind::default(),
            opencode: OpenCodeConfig {
                mode: ServerMode::Auto,
                hostname: "127.0.0.1".to_string(),
                port: 4096,
                password: project_pw.map(str::to_string),
            },
            kiro: KiroConfig::default(),
            workflow: WorkflowConfig::default(),
        }
    }

    #[test]
    fn test_workflow_config_defaults_approval_gate_true() {
        let config: WorkflowConfig = toml::from_str("").unwrap();
        assert!(config.approval_gate, "approval_gate should default to true");
    }

    #[test]
    fn test_workflow_config_explicit_false() {
        let toml = "approval_gate = false\n";
        let config: WorkflowConfig = toml::from_str(toml).unwrap();
        assert!(!config.approval_gate);
    }

    #[test]
    fn test_effective_password_hardcoded_default() {
        let config = make_app_config(None, None);
        assert_eq!(config.effective_opencode_password(), "clawdmux-default-pw");
    }

    #[test]
    fn test_effective_password_project_override() {
        let config = make_app_config(None, Some("project-pw"));
        assert_eq!(config.effective_opencode_password(), "project-pw");
    }

    #[test]
    fn test_effective_password_global_override() {
        let config = make_app_config(Some("global-pw"), Some("project-pw"));
        assert_eq!(config.effective_opencode_password(), "global-pw");
    }

    #[test]
    fn test_has_explicit_password_false_by_default() {
        let config = make_app_config(None, None);
        assert!(!config.has_explicit_password());
    }

    #[test]
    fn test_has_explicit_password_true_when_project_set() {
        let config = make_app_config(None, Some("project-pw"));
        assert!(config.has_explicit_password());
    }

    #[test]
    fn test_has_explicit_password_true_when_global_set() {
        let config = make_app_config(Some("global-pw"), None);
        assert!(config.has_explicit_password());
    }

    #[test]
    fn test_backend_kind_default_is_opencode() {
        assert_eq!(BackendKind::default(), BackendKind::OpenCode);
    }

    #[test]
    fn test_backend_kind_serde() {
        #[derive(Debug, Deserialize, Serialize, PartialEq)]
        struct Wrapper {
            #[serde(default)]
            backend: BackendKind,
        }

        let opencode_toml = r#"backend = "opencode""#;
        let kiro_toml = r#"backend = "kiro""#;
        let empty_toml = "";

        let w: Wrapper = toml::from_str(opencode_toml).unwrap();
        assert_eq!(w.backend, BackendKind::OpenCode);

        let w: Wrapper = toml::from_str(kiro_toml).unwrap();
        assert_eq!(w.backend, BackendKind::Kiro);

        // Empty TOML falls back to default (opencode) via #[serde(default)].
        let w: Wrapper = toml::from_str(empty_toml).unwrap();
        assert_eq!(w.backend, BackendKind::OpenCode);

        // Round-trip serialization.
        let serialized = toml::to_string_pretty(&Wrapper {
            backend: BackendKind::Kiro,
        })
        .unwrap();
        assert!(serialized.contains("kiro"), "serialized: {serialized}");
    }

    #[test]
    fn test_kiro_config_defaults() {
        let config: KiroConfig = toml::from_str("").unwrap();
        assert!(config.binary.is_none());
    }

    #[test]
    fn test_kiro_config_binary() {
        let toml = r#"binary = "/usr/local/bin/kiro""#;
        let config: KiroConfig = toml::from_str(toml).unwrap();
        assert_eq!(config.binary.as_deref(), Some("/usr/local/bin/kiro"));
    }

    #[test]
    fn test_app_config_includes_backend_and_kiro_defaults() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        let global_path = global_dir.path().join("config.toml");
        let config = AppConfig::load_from(&global_path, project_dir.path()).unwrap();
        assert_eq!(config.backend, BackendKind::OpenCode);
        assert!(config.kiro.binary.is_none());
    }

    #[test]
    fn test_app_config_kiro_backend_from_project_config() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        let clawdmux_dir = project_dir.path().join(".clawdmux");
        std::fs::create_dir_all(&clawdmux_dir).unwrap();
        std::fs::write(
            clawdmux_dir.join("config.toml"),
            r#"backend = "kiro"

[kiro]
binary = "/opt/kiro/bin/kiro"
"#,
        )
        .unwrap();
        let global_path = global_dir.path().join("config.toml");
        let config = AppConfig::load_from(&global_path, project_dir.path()).unwrap();
        assert_eq!(config.backend, BackendKind::Kiro);
        assert_eq!(config.kiro.binary.as_deref(), Some("/opt/kiro/bin/kiro"));
    }
}
