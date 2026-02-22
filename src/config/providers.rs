//! LLM provider configuration loaded from `~/.config/clawdmux/config.toml`.
//!
//! Provides API keys and model defaults for the active LLM provider, which are
//! injected into the opencode server process as environment variables.
//! Credentials are never written to opencode's own config files.

use std::fmt;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ClawdMuxError, Result};

/// Configuration for a single LLM provider (API key and default model).
///
/// Always wrapped in `Option` -- a provider with no API key would be invalid.
#[allow(dead_code)]
#[derive(Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    /// The API key for this provider.
    pub api_key: String,
    /// The default model to use for this provider (e.g., `"claude-opus-4-6"`).
    pub default_model: String,
}

impl fmt::Debug for ProviderConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderConfig")
            .field("api_key", &"<redacted>")
            .field("default_model", &self.default_model)
            .finish()
    }
}

/// Provider selection and per-provider credentials.
///
/// The `default` field names the active provider; the remaining fields hold
/// optional credentials for each supported provider.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ProviderSection {
    /// Name of the active provider (e.g., `"anthropic"`, `"openai"`, `"google"`).
    #[serde(default)]
    pub default: String,
    /// Anthropic provider credentials.
    pub anthropic: Option<ProviderConfig>,
    /// OpenAI provider credentials.
    pub openai: Option<ProviderConfig>,
    /// Google provider credentials.
    pub google: Option<ProviderConfig>,
}

/// Global ClawdMux configuration stored in `~/.config/clawdmux/config.toml`.
///
/// Contains LLM provider credentials used to configure opencode server processes.
#[allow(dead_code)]
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct GlobalConfig {
    /// LLM provider section.
    #[serde(default)]
    pub provider: ProviderSection,
    /// Optional override for the opencode server password.
    ///
    /// When set, takes precedence over the project-level password and the
    /// hardcoded default. The value is injected as `OPENCODE_SERVER_PASSWORD`
    /// into the spawned opencode process.
    #[serde(default)]
    pub opencode_password: Option<String>,
}

#[allow(dead_code)]
impl GlobalConfig {
    /// Load a [`GlobalConfig`] from a TOML file at `path`.
    ///
    /// Returns `Err(ClawdMuxError::Io)` if the file cannot be read (including
    /// `NotFound`) and `Err(ClawdMuxError::Config)` if the TOML is malformed.
    pub fn load(path: &Path) -> Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: GlobalConfig = toml::from_str(&contents)?;
        Ok(config)
    }

    /// Serialize this config to TOML and write it to `path`.
    ///
    /// Creates any missing parent directories. Maps TOML serialization errors
    /// to [`ClawdMuxError::Encode`].
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let contents =
            toml::to_string_pretty(self).map_err(|e| ClawdMuxError::Encode(e.to_string()))?;
        std::fs::write(path, contents)?;
        Ok(())
    }

    /// Return environment variable pairs for the active LLM provider.
    ///
    /// The pairs are suitable for injecting into the opencode server process
    /// (e.g., `("ANTHROPIC_API_KEY", "<key>")`). Returns an empty `Vec` if no
    /// provider is configured or the default name is unrecognized.
    pub fn env_vars_for_opencode(&self) -> Vec<(String, String)> {
        let Some(provider) = self.active_provider() else {
            return Vec::new();
        };
        let (key_var, model_var) = match self.provider.default.as_str() {
            "anthropic" => ("ANTHROPIC_API_KEY", "ANTHROPIC_DEFAULT_MODEL"),
            "openai" => ("OPENAI_API_KEY", "OPENAI_DEFAULT_MODEL"),
            "google" => ("GOOGLE_GENERATIVE_AI_API_KEY", "GOOGLE_DEFAULT_MODEL"),
            _ => unreachable!("active_provider() only returns Some for known providers"),
        };
        vec![
            (key_var.to_string(), provider.api_key.clone()),
            (model_var.to_string(), provider.default_model.clone()),
        ]
    }

    /// Return a reference to the [`ProviderConfig`] for the currently active provider.
    ///
    /// Returns `None` if `provider.default` is empty, unrecognized, or the
    /// corresponding provider block is not set.
    fn active_provider(&self) -> Option<&ProviderConfig> {
        match self.provider.default.as_str() {
            "anthropic" => self.provider.anthropic.as_ref(),
            "openai" => self.provider.openai.as_ref(),
            "google" => self.provider.google.as_ref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    fn anthropic_config() -> GlobalConfig {
        GlobalConfig {
            provider: ProviderSection {
                default: "anthropic".to_string(),
                anthropic: Some(ProviderConfig {
                    api_key: "sk-ant-test".to_string(),
                    default_model: "claude-opus-4-6".to_string(),
                }),
                openai: None,
                google: None,
            },
            opencode_password: None,
        }
    }

    #[test]
    fn test_global_config_load() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let toml = r#"
[provider]
default = "anthropic"

[provider.anthropic]
api_key = "sk-ant-test"
default_model = "claude-opus-4-6"
"#;
        std::fs::write(&path, toml).unwrap();

        let config = GlobalConfig::load(&path).unwrap();
        assert_eq!(config.provider.default, "anthropic");
        let anthropic = config.provider.anthropic.unwrap();
        assert_eq!(anthropic.api_key, "sk-ant-test");
        assert_eq!(anthropic.default_model, "claude-opus-4-6");
    }

    #[test]
    fn test_global_config_load_nonexistent() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("does_not_exist.toml");

        let result = GlobalConfig::load(&path);
        assert!(matches!(result, Err(ClawdMuxError::Io(_))));
    }

    #[test]
    fn test_global_config_save_and_reload() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let original = anthropic_config();

        original.save(&path).unwrap();
        let loaded = GlobalConfig::load(&path).unwrap();

        assert_eq!(loaded.provider.default, original.provider.default);
        let orig_ant = original.provider.anthropic.unwrap();
        let loaded_ant = loaded.provider.anthropic.unwrap();
        assert_eq!(loaded_ant.api_key, orig_ant.api_key);
        assert_eq!(loaded_ant.default_model, orig_ant.default_model);
    }

    #[test]
    fn test_global_config_save_creates_parents() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a").join("b").join("config.toml");
        let config = anthropic_config();

        config.save(&path).unwrap();
        assert!(path.exists(), "config file should have been created");
    }

    #[test]
    fn test_global_config_invalid_toml() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "not valid toml ][[[").unwrap();

        let result = GlobalConfig::load(&path);
        assert!(matches!(result, Err(ClawdMuxError::Config(_))));
    }

    #[test]
    fn test_env_vars_anthropic() {
        let config = anthropic_config();
        let vars = config.env_vars_for_opencode();

        assert_eq!(vars.len(), 2);
        assert!(vars.contains(&("ANTHROPIC_API_KEY".to_string(), "sk-ant-test".to_string())));
        assert!(vars.contains(&(
            "ANTHROPIC_DEFAULT_MODEL".to_string(),
            "claude-opus-4-6".to_string()
        )));
    }

    #[test]
    fn test_provider_config_debug_redacts_key() {
        let provider = ProviderConfig {
            api_key: "sk-secret-key".to_string(),
            default_model: "claude-opus-4-6".to_string(),
        };
        let debug = format!("{:?}", provider);
        assert!(
            debug.contains("<redacted>"),
            "debug should contain <redacted>"
        );
        assert!(
            !debug.contains("sk-secret-key"),
            "debug must not expose api_key"
        );
        assert!(
            debug.contains("claude-opus-4-6"),
            "debug should include default_model"
        );
    }

    #[test]
    fn test_env_vars_openai() {
        let config = GlobalConfig {
            provider: ProviderSection {
                default: "openai".to_string(),
                anthropic: None,
                openai: Some(ProviderConfig {
                    api_key: "sk-openai-test".to_string(),
                    default_model: "gpt-4o".to_string(),
                }),
                google: None,
            },
            opencode_password: None,
        };
        let vars = config.env_vars_for_opencode();
        assert_eq!(vars.len(), 2);
        assert!(vars.contains(&("OPENAI_API_KEY".to_string(), "sk-openai-test".to_string())));
        assert!(vars.contains(&("OPENAI_DEFAULT_MODEL".to_string(), "gpt-4o".to_string())));
    }

    #[test]
    fn test_env_vars_google() {
        let config = GlobalConfig {
            provider: ProviderSection {
                default: "google".to_string(),
                anthropic: None,
                openai: None,
                google: Some(ProviderConfig {
                    api_key: "google-api-key".to_string(),
                    default_model: "gemini-pro".to_string(),
                }),
            },
            opencode_password: None,
        };
        let vars = config.env_vars_for_opencode();
        assert_eq!(vars.len(), 2);
        assert!(vars.contains(&(
            "GOOGLE_GENERATIVE_AI_API_KEY".to_string(),
            "google-api-key".to_string()
        )));
        assert!(vars.contains(&("GOOGLE_DEFAULT_MODEL".to_string(), "gemini-pro".to_string())));
    }

    #[test]
    fn test_env_vars_no_provider() {
        let config = GlobalConfig::default();
        let vars = config.env_vars_for_opencode();
        assert!(vars.is_empty());
    }

    #[test]
    fn test_env_vars_unknown_provider() {
        let config = GlobalConfig {
            provider: ProviderSection {
                default: "unknown_provider".to_string(),
                anthropic: None,
                openai: None,
                google: None,
            },
            opencode_password: None,
        };
        let vars = config.env_vars_for_opencode();
        assert!(vars.is_empty());
    }

    #[test]
    fn test_global_config_opencode_password() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.toml");
        let toml = r#"
opencode_password = "my-global-pw"

[provider]
default = "anthropic"
"#;
        std::fs::write(&path, toml).unwrap();

        let config = GlobalConfig::load(&path).unwrap();
        assert_eq!(config.opencode_password.as_deref(), Some("my-global-pw"));

        // Round-trip: save and reload preserves the password.
        config.save(&path).unwrap();
        let reloaded = GlobalConfig::load(&path).unwrap();
        assert_eq!(reloaded.opencode_password.as_deref(), Some("my-global-pw"));
    }

    #[test]
    fn test_global_config_opencode_password_absent() {
        // When the field is absent, it defaults to None.
        let config: GlobalConfig = toml::from_str("").unwrap();
        assert!(config.opencode_password.is_none());
    }
}
