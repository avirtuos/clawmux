//! `clawdmux init` command: dependency checks, provider setup, and project scaffold.
//!
//! Interactive terminal wizard for first-time project setup:
//! 1. Checks for (and optionally installs) the opencode binary.
//! 2. Configures LLM provider credentials in `~/.config/clawdmux/config.toml`.
//! 3. Scaffolds `.clawdmux/config.toml`, `.opencode/agents/clawdmux/`, and `tasks/`.
//! 4. Logs a success message.

use std::io::{BufRead, Write};
use std::path::Path;

use crate::config::providers::{GlobalConfig, ProviderConfig, ProviderSection};
use crate::error::{ClawdMuxError, Result};
use crate::workflow::agents::AgentKind;

// ---------------------------------------------------------------------------
// Agent definition file content (one static string per pipeline stage)
//
// Content is embedded at compile time from `src/config/agents/`.  Edit those
// files directly to adjust agent personalities, tool permissions, or prompts.
// ---------------------------------------------------------------------------

const INTAKE_AGENT: &str = include_str!("agents/intake.md");
const DESIGN_AGENT: &str = include_str!("agents/design.md");
const PLANNING_AGENT: &str = include_str!("agents/planning.md");
const IMPLEMENTATION_AGENT: &str = include_str!("agents/implementation.md");
const CODE_QUALITY_AGENT: &str = include_str!("agents/code-quality.md");
const SECURITY_REVIEW_AGENT: &str = include_str!("agents/security-review.md");
const CODE_REVIEW_AGENT: &str = include_str!("agents/code-review.md");

// ---------------------------------------------------------------------------
// Task template file content (embedded at compile time from `src/config/tasks/`)
// ---------------------------------------------------------------------------

const TASK_FORMAT_DOC: &str = include_str!("tasks/tasks.md");
const TASK_1_1: &str = include_str!("tasks/1.1.md");
const TASK_1_2: &str = include_str!("tasks/1.2.md");
const TASK_2_1: &str = include_str!("tasks/2.1.md");

/// Task template files written into `tasks/` during scaffolding.
const TASK_SEED_FILES: &[(&str, &str)] = &[
    ("tasks.md", TASK_FORMAT_DOC),
    ("1.1.md", TASK_1_1),
    ("1.2.md", TASK_1_2),
    ("2.1.md", TASK_2_1),
];

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Arguments for the `clawdmux init` subcommand.
#[derive(clap::Args, Debug)]
pub struct InitArgs {
    /// Regenerate agent definition files from built-in defaults, overwriting
    /// any local edits.
    #[arg(long)]
    pub reset_agents: bool,
}

/// Runs the interactive `clawdmux init` wizard.
///
/// Performs four steps:
/// 1. Checks for the opencode binary; offers to install it if missing.
/// 2. Checks for provider credentials in the global config; prompts if absent.
/// 3. Scaffolds project-local files (`.clawdmux/config.toml`,
///    `.opencode/agents/clawdmux/`, `tasks/`).
/// 4. Logs a success summary via `tracing::info!`.
///
/// # Errors
///
/// Returns [`ClawdMuxError::Internal`] if the platform config directory cannot
/// be resolved, the user declines to install opencode, or an invalid provider
/// choice is entered. Returns [`ClawdMuxError::Io`] for filesystem or stdin
/// failures.
pub fn run_init(project_root: &Path, args: &InitArgs) -> Result<()> {
    check_or_install_opencode()?;
    let global_config_dir = dirs::config_dir().ok_or_else(|| {
        ClawdMuxError::Internal("could not determine platform config directory".to_string())
    })?;
    let global_config_path = global_config_dir.join("clawdmux").join("config.toml");
    run_init_with_paths(&global_config_path, project_root, args)
}

// ---------------------------------------------------------------------------
// Internal helpers (pub(crate) so tests can call them with injected paths)
// ---------------------------------------------------------------------------

/// Inner implementation used by both [`run_init`] and tests.
///
/// Accepts an explicit `global_config_path` so tests can supply a
/// `TempDir`-based path without touching `~/.config/clawdmux/config.toml`.
/// The opencode binary check is intentionally excluded here; it lives only in
/// the interactive [`run_init`] entry point.
pub(crate) fn run_init_with_paths(
    global_config_path: &Path,
    project_root: &Path,
    args: &InitArgs,
) -> Result<()> {
    configure_provider(global_config_path)?;
    scaffold_project(project_root, args)?;
    tracing::info!("clawdmux init complete, run clawdmux to open the TUI");
    Ok(())
}

/// Step 1: verify opencode is on `PATH`; offer to install if missing.
///
/// Uses `--no-modify-path` so the installer does not alter shell config files.
/// Because the binary directory is therefore not added to `PATH` automatically,
/// a post-install `opencode --version` check would always fail. Instead, after
/// a successful install the user is instructed to add the binary directory to
/// their `PATH` and re-run `clawdmux init`.
fn check_or_install_opencode() -> Result<()> {
    match which::which("opencode") {
        Ok(path) => {
            tracing::info!(path = %path.display(), "opencode binary found");
            Ok(())
        }
        Err(_) => {
            println!("Checking for opencode... not found.");
            println!("opencode is required. Install it now? [Y/n]");
            print!("> ");
            std::io::stdout().flush().map_err(ClawdMuxError::Io)?;

            let mut response = String::new();
            std::io::stdin()
                .read_line(&mut response)
                .map_err(ClawdMuxError::Io)?;

            if response.trim().eq_ignore_ascii_case("n")
                || response.trim().eq_ignore_ascii_case("no")
            {
                return Err(ClawdMuxError::Internal(
                    "opencode is required to use clawdmux".to_string(),
                ));
            }

            let status = std::process::Command::new("bash")
                .arg("-c")
                .arg("curl -fsSL https://opencode.ai/install | bash -s -- --no-modify-path")
                .status()
                .map_err(ClawdMuxError::Io)?;

            if !status.success() {
                return Err(ClawdMuxError::Internal(
                    "opencode installation failed".to_string(),
                ));
            }

            tracing::info!("opencode installed, PATH update required before use");
            println!(
                "opencode installed. Add the opencode binary directory to your PATH, \
                 then re-run clawdmux init."
            );
            Ok(())
        }
    }
}

/// Step 2: ensure the global config has a configured LLM provider.
///
/// Thin wrapper around [`configure_provider_from_reader`] that supplies the
/// real stdin and stdout.
fn configure_provider(global_config_path: &Path) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    configure_provider_from_reader(global_config_path, &mut stdin.lock(), &mut stdout.lock())
}

/// Core provider-configuration logic, accepting injected I/O for testability.
///
/// If `provider.default` is already set in the global config this is a no-op.
/// Otherwise the user is prompted to choose a provider, enter a default model,
/// and enter an API key. All inputs are read from `reader`; all prompts are
/// written to `writer`.
///
/// Note: because all input (including the API key) is read via `reader`, the
/// production wrapper [`configure_provider`] passes the real stdin, which means
/// the API key is visible while being typed. If hidden entry is needed, wire a
/// `rpassword`-backed reader at the call site.
pub(crate) fn configure_provider_from_reader<R: BufRead, W: Write>(
    global_config_path: &Path,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    let mut config = match GlobalConfig::load(global_config_path) {
        Ok(cfg) => cfg,
        Err(ClawdMuxError::Io(ref e)) if e.kind() == std::io::ErrorKind::NotFound => {
            GlobalConfig::default()
        }
        Err(e) => return Err(e),
    };

    if !config.provider.default.is_empty() {
        tracing::info!(
            provider = %config.provider.default,
            "LLM provider already configured"
        );
        return Ok(());
    }

    writeln!(writer, "No LLM provider configured. Let us set one up.")
        .map_err(ClawdMuxError::Io)?;
    writeln!(
        writer,
        "Provider: [1] Anthropic  [2] OpenAI  [3] Google  [4] OpenRouter"
    )
    .map_err(ClawdMuxError::Io)?;
    write!(writer, "> ").map_err(ClawdMuxError::Io)?;
    writer.flush().map_err(ClawdMuxError::Io)?;

    let mut choice = String::new();
    reader.read_line(&mut choice).map_err(ClawdMuxError::Io)?;

    let (provider_name, default_model_hint) = match choice.trim() {
        "1" => ("anthropic", "claude-sonnet-4-5"),
        "2" => ("openai", "gpt-4o"),
        "3" => ("google", "gemini-2.0-flash"),
        "4" => ("openrouter", "openrouter/openrouter/auto"),
        other => {
            return Err(ClawdMuxError::Internal(format!(
                "invalid provider choice: {}",
                other
            )))
        }
    };

    write!(writer, "Default model [{}]: ", default_model_hint).map_err(ClawdMuxError::Io)?;
    writer.flush().map_err(ClawdMuxError::Io)?;

    let mut model_input = String::new();
    reader
        .read_line(&mut model_input)
        .map_err(ClawdMuxError::Io)?;
    let model = {
        let trimmed = model_input.trim();
        if trimmed.is_empty() {
            default_model_hint.to_string()
        } else {
            trimmed.to_string()
        }
    };

    write!(writer, "API key: ").map_err(ClawdMuxError::Io)?;
    writer.flush().map_err(ClawdMuxError::Io)?;

    let mut api_key_input = String::new();
    reader
        .read_line(&mut api_key_input)
        .map_err(ClawdMuxError::Io)?;
    let api_key = api_key_input.trim().to_string();

    if api_key.is_empty() {
        return Err(ClawdMuxError::Internal(
            "API key must not be empty".to_string(),
        ));
    }

    let provider_config = ProviderConfig {
        api_key,
        default_model: model,
    };

    config.provider = ProviderSection {
        default: provider_name.to_string(),
        anthropic: if provider_name == "anthropic" {
            Some(provider_config.clone())
        } else {
            None
        },
        openai: if provider_name == "openai" {
            Some(provider_config.clone())
        } else {
            None
        },
        google: if provider_name == "google" {
            Some(provider_config.clone())
        } else {
            None
        },
        openrouter: if provider_name == "openrouter" {
            Some(provider_config)
        } else {
            None
        },
    };

    config.save(global_config_path)?;
    tracing::info!(path = %global_config_path.display(), "global config saved");
    Ok(())
}

/// Step 3: create project-local files and directories.
///
/// All paths are created only if absent, except agent definition files which
/// are also overwritten when `args.reset_agents` is `true`.
pub(crate) fn scaffold_project(project_root: &Path, args: &InitArgs) -> Result<()> {
    tracing::info!("scaffolding project");

    // .clawdmux/config.toml
    let clawdmux_dir = project_root.join(".clawdmux");
    let project_config_path = clawdmux_dir.join("config.toml");
    if !project_config_path.exists() {
        std::fs::create_dir_all(&clawdmux_dir).map_err(ClawdMuxError::Io)?;
        std::fs::write(&project_config_path, DEFAULT_PROJECT_CONFIG).map_err(ClawdMuxError::Io)?;
        tracing::info!(path = %project_config_path.display(), "created project config");
    }

    // tasks/ with seed files
    let tasks_dir = project_root.join("tasks");
    std::fs::create_dir_all(&tasks_dir).map_err(ClawdMuxError::Io)?;

    for &(file_name, content) in TASK_SEED_FILES {
        let file_path = tasks_dir.join(file_name);
        if !file_path.exists() {
            std::fs::write(&file_path, content).map_err(ClawdMuxError::Io)?;
            tracing::info!(path = %file_path.display(), "created task seed file");
        }
    }

    // .opencode/agents/clawdmux/
    let agents_dir = project_root
        .join(".opencode")
        .join("agents")
        .join("clawdmux");
    std::fs::create_dir_all(&agents_dir).map_err(ClawdMuxError::Io)?;

    for agent in AgentKind::all() {
        let (Some(file_name), Some(content)) =
            (agent_file_name(agent), agent_definition_content(agent))
        else {
            continue;
        };
        let file_path = agents_dir.join(file_name);
        if !file_path.exists() || args.reset_agents {
            std::fs::write(&file_path, content).map_err(ClawdMuxError::Io)?;
            tracing::info!(path = %file_path.display(), "created agent definition");
        }
    }

    Ok(())
}

/// Returns the filename (e.g. `"code-quality.md"`) for an agent definition,
/// or `None` for [`AgentKind::Human`] which has no definition file.
fn agent_file_name(agent: &AgentKind) -> Option<&'static str> {
    match agent {
        AgentKind::Intake => Some("intake.md"),
        AgentKind::Design => Some("design.md"),
        AgentKind::Planning => Some("planning.md"),
        AgentKind::Implementation => Some("implementation.md"),
        AgentKind::CodeQuality => Some("code-quality.md"),
        AgentKind::SecurityReview => Some("security-review.md"),
        AgentKind::CodeReview => Some("code-review.md"),
        AgentKind::Human => None,
    }
}

/// Returns the default YAML-frontmatter + system-prompt content for an agent,
/// or `None` for [`AgentKind::Human`] which has no definition file.
fn agent_definition_content(agent: &AgentKind) -> Option<&'static str> {
    match agent {
        AgentKind::Intake => Some(INTAKE_AGENT),
        AgentKind::Design => Some(DESIGN_AGENT),
        AgentKind::Planning => Some(PLANNING_AGENT),
        AgentKind::Implementation => Some(IMPLEMENTATION_AGENT),
        AgentKind::CodeQuality => Some(CODE_QUALITY_AGENT),
        AgentKind::SecurityReview => Some(SECURITY_REVIEW_AGENT),
        AgentKind::CodeReview => Some(CODE_REVIEW_AGENT),
        AgentKind::Human => None,
    }
}

/// Default content written to `.clawdmux/config.toml` during scaffold.
const DEFAULT_PROJECT_CONFIG: &str = r#"[opencode]
mode = "auto"
hostname = "127.0.0.1"
port = 4096
"#;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use crate::config::providers::{ProviderConfig, ProviderSection};
    use tempfile::TempDir;

    /// Write a minimal pre-configured global config to `path` so that
    /// `configure_provider_from_reader` returns immediately (no prompts).
    fn write_global_config(path: &Path) {
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let config = GlobalConfig {
            provider: ProviderSection {
                default: "anthropic".to_string(),
                anthropic: Some(ProviderConfig {
                    api_key: "sk-ant-test".to_string(),
                    default_model: "claude-sonnet-4-5".to_string(),
                }),
                openai: None,
                google: None,
                openrouter: None,
            },
            opencode_password: None,
        };
        config.save(path).unwrap();
    }

    // --- configure_provider_from_reader tests ---

    #[test]
    fn test_configure_provider_invalid_choice() {
        let dir = TempDir::new().unwrap();
        let global_path = dir.path().join("config.toml");
        // No existing config; provider.default is empty.
        let mut reader = io::Cursor::new(b"5\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());

        let result = configure_provider_from_reader(&global_path, &mut reader, &mut writer);
        assert!(
            matches!(result, Err(ClawdMuxError::Internal(_))),
            "expected Internal error for invalid choice, got: {:?}",
            result
        );
    }

    #[test]
    fn test_configure_provider_empty_api_key() {
        let dir = TempDir::new().unwrap();
        let global_path = dir.path().join("config.toml");
        // provider=1 (anthropic), model=empty (accept default), api_key=empty
        let mut reader = io::Cursor::new(b"1\n\n\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());

        let result = configure_provider_from_reader(&global_path, &mut reader, &mut writer);
        assert!(
            matches!(result, Err(ClawdMuxError::Internal(_))),
            "expected Internal error for empty API key, got: {:?}",
            result
        );
    }

    #[test]
    fn test_configure_provider_happy_path_anthropic() {
        let dir = TempDir::new().unwrap();
        let global_path = dir.path().join("config.toml");
        // provider=1, model override, api_key set
        let mut reader = io::Cursor::new(b"1\nclaude-opus-4-6\nsk-ant-test-key\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());

        configure_provider_from_reader(&global_path, &mut reader, &mut writer).unwrap();

        let config = GlobalConfig::load(&global_path).unwrap();
        assert_eq!(config.provider.default, "anthropic");
        let ant = config.provider.anthropic.unwrap();
        assert_eq!(ant.api_key, "sk-ant-test-key");
        assert_eq!(ant.default_model, "claude-opus-4-6");
        assert!(config.provider.openai.is_none());
        assert!(config.provider.google.is_none());
    }

    #[test]
    fn test_configure_provider_happy_path_uses_default_model() {
        let dir = TempDir::new().unwrap();
        let global_path = dir.path().join("config.toml");
        // provider=2 (openai), empty model line -> should use "gpt-4o"
        let mut reader = io::Cursor::new(b"2\n\nsk-openai-key\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());

        configure_provider_from_reader(&global_path, &mut reader, &mut writer).unwrap();

        let config = GlobalConfig::load(&global_path).unwrap();
        assert_eq!(config.provider.default, "openai");
        let oai = config.provider.openai.unwrap();
        assert_eq!(oai.default_model, "gpt-4o");
        assert_eq!(oai.api_key, "sk-openai-key");
    }

    #[test]
    fn test_configure_provider_happy_path_openrouter() {
        let dir = TempDir::new().unwrap();
        let global_path = dir.path().join("config.toml");
        // provider=4 (openrouter), empty model line -> should use default, api_key set
        let mut reader =
            io::Cursor::new(b"4\nopenrouter/openrouter/auto\nsk-or-test-key\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());

        configure_provider_from_reader(&global_path, &mut reader, &mut writer).unwrap();

        let config = GlobalConfig::load(&global_path).unwrap();
        assert_eq!(config.provider.default, "openrouter");
        let or_cfg = config.provider.openrouter.unwrap();
        assert_eq!(or_cfg.api_key, "sk-or-test-key");
        assert_eq!(or_cfg.default_model, "openrouter/openrouter/auto");
        assert!(config.provider.anthropic.is_none());
        assert!(config.provider.openai.is_none());
        assert!(config.provider.google.is_none());
    }

    #[test]
    fn test_configure_provider_already_configured() {
        let dir = TempDir::new().unwrap();
        let global_path = dir.path().join("config.toml");
        write_global_config(&global_path);

        // Empty reader: the function must return without reading anything.
        let mut reader = io::Cursor::new(b"".as_ref());
        let mut writer = io::Cursor::new(Vec::new());

        configure_provider_from_reader(&global_path, &mut reader, &mut writer).unwrap();

        // Config must be unchanged.
        let config = GlobalConfig::load(&global_path).unwrap();
        assert_eq!(config.provider.default, "anthropic");
    }

    // --- scaffold tests ---

    #[test]
    fn test_scaffold_creates_config() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        let global_path = global_dir.path().join("config.toml");
        write_global_config(&global_path);

        run_init_with_paths(
            &global_path,
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
        )
        .unwrap();

        assert!(
            project_dir.path().join(".clawdmux/config.toml").exists(),
            ".clawdmux/config.toml should be created"
        );
    }

    #[test]
    fn test_scaffold_creates_tasks_dir() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        let global_path = global_dir.path().join("config.toml");
        write_global_config(&global_path);

        run_init_with_paths(
            &global_path,
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
        )
        .unwrap();

        let tasks_dir = project_dir.path().join("tasks");
        assert!(tasks_dir.is_dir(), "tasks/ directory should be created");

        for &(file_name, _) in TASK_SEED_FILES {
            let file_path = tasks_dir.join(file_name);
            assert!(
                file_path.exists(),
                "task seed file {} should be created",
                file_name
            );
            let content = std::fs::read_to_string(&file_path).unwrap();
            assert!(
                !content.is_empty(),
                "task seed file {} should have non-empty content",
                file_name
            );
        }
    }

    #[test]
    fn test_scaffold_does_not_overwrite_existing_task_files() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        let global_path = global_dir.path().join("config.toml");
        write_global_config(&global_path);

        // Pre-create tasks/ and write custom content into one seed file.
        let tasks_dir = project_dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        let existing_path = tasks_dir.join("1.1.md");
        std::fs::write(&existing_path, "custom content").unwrap();

        run_init_with_paths(
            &global_path,
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
        )
        .unwrap();

        let content = std::fs::read_to_string(&existing_path).unwrap();
        assert_eq!(
            content, "custom content",
            "existing task file should not be overwritten by scaffold"
        );
    }

    #[test]
    fn test_scaffold_creates_all_agent_files() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        let global_path = global_dir.path().join("config.toml");
        write_global_config(&global_path);

        run_init_with_paths(
            &global_path,
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
        )
        .unwrap();

        let agents_dir = project_dir.path().join(".opencode/agents/clawdmux");

        for agent in AgentKind::all() {
            let file_name = agent_file_name(agent).expect("all() never yields Human");
            let file_path = agents_dir.join(file_name);
            assert!(
                file_path.exists(),
                "agent file {} should exist",
                file_path.display()
            );
        }
        assert_eq!(
            AgentKind::all().len(),
            7,
            "there should be exactly 7 pipeline agents"
        );
    }

    #[test]
    fn test_scaffold_idempotent() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        let global_path = global_dir.path().join("config.toml");
        write_global_config(&global_path);

        let args = InitArgs {
            reset_agents: false,
        };
        run_init_with_paths(&global_path, project_dir.path(), &args).unwrap();
        run_init_with_paths(&global_path, project_dir.path(), &args).unwrap();

        let agents_dir = project_dir.path().join(".opencode/agents/clawdmux");
        for agent in AgentKind::all() {
            let file_path = agents_dir.join(agent_file_name(agent).unwrap());
            assert!(
                file_path.is_file(),
                "{} should remain a regular file",
                file_path.display()
            );
        }
    }

    #[test]
    fn test_reset_agents_overwrites() {
        let global_dir = TempDir::new().unwrap();
        let project_dir = TempDir::new().unwrap();
        let global_path = global_dir.path().join("config.toml");
        write_global_config(&global_path);

        run_init_with_paths(
            &global_path,
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
        )
        .unwrap();

        let intake_path = project_dir
            .path()
            .join(".opencode/agents/clawdmux/intake.md");
        std::fs::write(&intake_path, "corrupted content").unwrap();
        assert_eq!(
            std::fs::read_to_string(&intake_path).unwrap(),
            "corrupted content"
        );

        run_init_with_paths(
            &global_path,
            project_dir.path(),
            &InitArgs { reset_agents: true },
        )
        .unwrap();

        let restored = std::fs::read_to_string(&intake_path).unwrap();
        assert_eq!(
            restored, INTAKE_AGENT,
            "intake.md should be restored to the built-in default"
        );
    }
}
