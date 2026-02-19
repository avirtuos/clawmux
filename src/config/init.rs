//! `clawdmux init` command: dependency checks, provider setup, and project scaffold.
//!
//! Interactive terminal wizard for first-time project setup:
//! 1. Checks for (and optionally installs) the opencode binary.
//! 2. Configures LLM provider credentials in `~/.config/clawdmux/config.toml`.
//! 3. Scaffolds `.clawdmux/config.toml`, `.opencode/agents/clawdmux/`, and `tasks/`.
//! 4. Prints a success message.

use std::io::Write;
use std::path::Path;

use crate::config::providers::{GlobalConfig, ProviderConfig, ProviderSection};
use crate::error::{ClawdMuxError, Result};
use crate::workflow::agents::AgentKind;

// ---------------------------------------------------------------------------
// Agent definition file content (one static string per pipeline stage)
// ---------------------------------------------------------------------------

const INTAKE_AGENT: &str = r#"---
description: Reviews the task file and clarifies requirements before work begins
mode: subagent
model: anthropic/claude-sonnet-4-5
steps: 20
tools:
  read: true
  write: false
  edit: false
  bash: false
---
You are the Intake Agent in the ClawdMux pipeline. Your job is to review the
task file and ensure all required fields are present and unambiguous.

Check for: a clear description, measurable acceptance criteria, and any missing
context that later agents will need. Prompt the human for anything you cannot
infer from the existing task content.

When finished, respond with a JSON object and nothing else:
{"action":"complete","summary":"<one sentence describing what you reviewed>"}

If you have a blocking question for the human, respond with:
{"action":"question","question":"<question text>","context":"<why you need to know>"}
"#;

const DESIGN_AGENT: &str = r#"---
description: Reviews the task and proposes design implications
mode: subagent
model: anthropic/claude-sonnet-4-5
steps: 30
tools:
  read: true
  write: false
  edit: false
  bash: false
---
You are the Design Agent in the ClawdMux pipeline. Your job is to review the
task and the existing state of the project to propose any relevant design
implications required to complete this task.

Examine existing modules, data structures, and interfaces. Identify the minimal
changes required and document your findings in the Design section of the task.

When finished, respond with a JSON object and nothing else:
{"action":"complete","summary":"<one sentence>","updates":{"design":"<design content>"}}

If you have a blocking question for the human, respond with:
{"action":"question","question":"<question text>","context":"<why you need to know>"}
"#;

const PLANNING_AGENT: &str = r#"---
description: Creates a step-by-step implementation plan from the task and design
mode: subagent
model: anthropic/claude-sonnet-4-5
steps: 30
tools:
  read: true
  write: false
  edit: false
  bash: true
permission:
  bash:
    "cargo check": allow
    "cargo build": allow
    "*": deny
---
You are the Planning Agent in the ClawdMux pipeline. Your job is to create a
step-by-step implementation plan based on the task description and the Design
Agent's findings.

The plan must be concrete enough for the Implementation Agent to follow without
ambiguity. List files to modify, functions to add or change, and tests to write.

When finished, respond with a JSON object and nothing else:
{"action":"complete","summary":"<one sentence>","updates":{"implementation_plan":"<plan content>"}}

If you have a blocking question for the human, respond with:
{"action":"question","question":"<question text>","context":"<why you need to know>"}
"#;

const IMPLEMENTATION_AGENT: &str = r#"---
description: Implements code changes according to the implementation plan
mode: subagent
model: anthropic/claude-sonnet-4-5
steps: 50
tools:
  read: true
  write: true
  edit: true
  bash: true
permission:
  bash:
    "cargo *": allow
    "git diff *": allow
    "git status": allow
    "*": ask
---
You are the Implementation Agent in the ClawdMux pipeline. Your job is to
implement the code changes described in the task's implementation plan.

Follow the plan precisely. Prefer editing existing files over creating new ones.
Write idiomatic, well-tested code. Do not refactor code outside the scope of
the task.

When finished, respond with a JSON object and nothing else:
{"action":"complete","summary":"<one sentence>"}

If you have a blocking question for the human, respond with:
{"action":"question","question":"<question text>","context":"<why you need to know>"}
"#;

const CODE_QUALITY_AGENT: &str = r#"---
description: Ensures adequate test coverage and adherence to coding standards
mode: subagent
model: anthropic/claude-sonnet-4-5
steps: 30
tools:
  read: true
  write: false
  edit: true
  bash: true
permission:
  bash:
    "cargo fmt *": allow
    "cargo clippy *": allow
    "cargo test *": allow
    "cargo build *": allow
    "*": deny
---
You are the Code Quality Agent in the ClawdMux pipeline. Your job is to ensure
the code has adequate test coverage, builds without errors, and follows the
project's coding standards.

Run cargo fmt, cargo clippy, and cargo test. Fix formatting and trivial lint
issues directly. If you find non-trivial issues that you cannot address
yourself, kick the task back to the Implementation Agent with specific details.

When finished with no issues, respond with a JSON object and nothing else:
{"action":"complete","summary":"<one sentence>"}

To kick back, respond with:
{"action":"kickback","target_agent":"implementation","reason":"<specific issues found>"}

If you have a blocking question for the human, respond with:
{"action":"question","question":"<question text>","context":"<why you need to know>"}
"#;

const SECURITY_REVIEW_AGENT: &str = r#"---
description: Audits code for security vulnerabilities and credential exposure
mode: subagent
model: anthropic/claude-sonnet-4-5
steps: 20
tools:
  read: true
  write: false
  edit: false
  bash: false
---
You are the Security Review Agent in the ClawdMux pipeline. Your job is to
audit the code produced so far for security concerns such as injection
vulnerabilities, credential exposure, insecure defaults, and missing input
validation.

If you find actionable security issues, kick the task back to the appropriate
agent with specific findings. Minor observations that do not require code
changes may be noted in your summary.

When finished with no blocking issues, respond with a JSON object and nothing else:
{"action":"complete","summary":"<one sentence>"}

To kick back to a prior agent, respond with:
{"action":"kickback","target_agent":"implementation","reason":"<specific security issue>"}

If you have a blocking question for the human, respond with:
{"action":"question","question":"<question text>","context":"<why you need to know>"}
"#;

const CODE_REVIEW_AGENT: &str = r#"---
description: Reviews code for bugs and maintainability, then prepares commit message
mode: subagent
model: anthropic/claude-sonnet-4-5
steps: 30
tools:
  read: true
  write: false
  edit: false
  bash: true
permission:
  bash:
    "git diff *": allow
    "git status": allow
    "git log *": allow
    "*": deny
---
You are the Code Review Agent in the ClawdMux pipeline. You have two jobs:
1. Independently review the code for bugs, maintainability concerns, and
   adherence to project standards.
2. Once your own review passes, ensure any human reviewer feedback is also
   addressed via kickbacks to earlier agents.

If no issues remain and the human approves, prepare a commit message.

When finished with no issues, respond with a JSON object and nothing else:
{"action":"complete","summary":"<one sentence>","commit_message":"<conventional commit message>"}

To kick back to an earlier agent, respond with:
{"action":"kickback","target_agent":"implementation","reason":"<specific issue>"}

If you have a blocking question for the human, respond with:
{"action":"question","question":"<question text>","context":"<why you need to know>"}
"#;

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
/// 4. Prints a success summary.
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
    println!("clawdmux is ready. Run clawdmux to open the TUI.");
    Ok(())
}

/// Step 1: verify opencode is on `PATH`; offer to install if missing.
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

            let verify = std::process::Command::new("opencode")
                .arg("--version")
                .output()
                .map_err(ClawdMuxError::Io)?;

            if !verify.status.success() {
                return Err(ClawdMuxError::Internal(
                    "opencode installed but failed to run".to_string(),
                ));
            }

            let version = String::from_utf8_lossy(&verify.stdout);
            tracing::info!(version = %version.trim(), "opencode installed successfully");
            Ok(())
        }
    }
}

/// Step 2: ensure the global config has a configured LLM provider.
///
/// If `provider.default` is already set, this is a no-op. Otherwise the user
/// is prompted to choose a provider and enter credentials.
fn configure_provider(global_config_path: &Path) -> Result<()> {
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

    println!("No LLM provider configured. Let us set one up.");
    println!("Provider: [1] Anthropic  [2] OpenAI  [3] Google");
    print!("> ");
    std::io::stdout().flush().map_err(ClawdMuxError::Io)?;

    let mut choice = String::new();
    std::io::stdin()
        .read_line(&mut choice)
        .map_err(ClawdMuxError::Io)?;

    let (provider_name, default_model_hint) = match choice.trim() {
        "1" => ("anthropic", "claude-sonnet-4-5"),
        "2" => ("openai", "gpt-4o"),
        "3" => ("google", "gemini-2.0-flash"),
        other => {
            return Err(ClawdMuxError::Internal(format!(
                "invalid provider choice: {}",
                other
            )))
        }
    };

    print!("Default model [{}]: ", default_model_hint);
    std::io::stdout().flush().map_err(ClawdMuxError::Io)?;

    let mut model_input = String::new();
    std::io::stdin()
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

    let api_key = rpassword::prompt_password("API key: ").map_err(ClawdMuxError::Io)?;

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
            Some(provider_config)
        } else {
            None
        },
    };

    config.save(global_config_path)?;
    println!("Credentials saved to {}", global_config_path.display());
    tracing::info!(path = %global_config_path.display(), "global config saved");
    Ok(())
}

/// Step 3: create project-local files and directories.
///
/// All paths are created only if absent, except agent definition files which
/// are also overwritten when `args.reset_agents` is `true`.
fn scaffold_project(project_root: &Path, args: &InitArgs) -> Result<()> {
    println!("Scaffolding project...");

    // .clawdmux/config.toml
    let clawdmux_dir = project_root.join(".clawdmux");
    let project_config_path = clawdmux_dir.join("config.toml");
    if !project_config_path.exists() {
        std::fs::create_dir_all(&clawdmux_dir).map_err(ClawdMuxError::Io)?;
        std::fs::write(&project_config_path, DEFAULT_PROJECT_CONFIG).map_err(ClawdMuxError::Io)?;
        tracing::info!(path = %project_config_path.display(), "created project config");
        println!("  created .clawdmux/config.toml");
    }

    // tasks/
    let tasks_dir = project_root.join("tasks");
    std::fs::create_dir_all(&tasks_dir).map_err(ClawdMuxError::Io)?;
    tracing::info!(path = %tasks_dir.display(), "created tasks directory");
    println!("  created tasks/  (task file directory)");

    // .opencode/agents/clawdmux/
    let agents_dir = project_root
        .join(".opencode")
        .join("agents")
        .join("clawdmux");
    std::fs::create_dir_all(&agents_dir).map_err(ClawdMuxError::Io)?;

    for agent in AgentKind::all() {
        let file_name = agent_file_name(agent);
        let file_path = agents_dir.join(file_name);
        if !file_path.exists() || args.reset_agents {
            std::fs::write(&file_path, agent_definition_content(agent))
                .map_err(ClawdMuxError::Io)?;
            tracing::info!(path = %file_path.display(), "created agent definition");
            println!("  created .opencode/agents/clawdmux/{}", file_name);
        }
    }

    Ok(())
}

/// Returns the filename (e.g. `"code-quality.md"`) for an agent definition.
fn agent_file_name(agent: &AgentKind) -> &'static str {
    match agent {
        AgentKind::Intake => "intake.md",
        AgentKind::Design => "design.md",
        AgentKind::Planning => "planning.md",
        AgentKind::Implementation => "implementation.md",
        AgentKind::CodeQuality => "code-quality.md",
        AgentKind::SecurityReview => "security-review.md",
        AgentKind::CodeReview => "code-review.md",
        AgentKind::Human => unreachable!("Human is not a pipeline agent"),
    }
}

/// Returns the default YAML-frontmatter + system-prompt content for an agent.
fn agent_definition_content(agent: &AgentKind) -> &'static str {
    match agent {
        AgentKind::Intake => INTAKE_AGENT,
        AgentKind::Design => DESIGN_AGENT,
        AgentKind::Planning => PLANNING_AGENT,
        AgentKind::Implementation => IMPLEMENTATION_AGENT,
        AgentKind::CodeQuality => CODE_QUALITY_AGENT,
        AgentKind::SecurityReview => SECURITY_REVIEW_AGENT,
        AgentKind::CodeReview => CODE_REVIEW_AGENT,
        AgentKind::Human => unreachable!("Human is not a pipeline agent"),
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
    use super::*;
    use crate::config::providers::ProviderConfig;
    use tempfile::TempDir;

    /// Write a minimal pre-configured global config to `path` so that
    /// `configure_provider` returns immediately (no interactive prompt).
    fn write_global_config(path: &Path) {
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).unwrap();
        let config = GlobalConfig {
            provider: crate::config::providers::ProviderSection {
                default: "anthropic".to_string(),
                anthropic: Some(ProviderConfig {
                    api_key: "sk-ant-test".to_string(),
                    default_model: "claude-sonnet-4-5".to_string(),
                }),
                openai: None,
                google: None,
            },
        };
        config.save(path).unwrap();
    }

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

        assert!(
            project_dir.path().join("tasks").is_dir(),
            "tasks/ directory should be created"
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
            let file_path = agents_dir.join(agent_file_name(agent));
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
        // Second call must succeed without errors or duplicate files.
        run_init_with_paths(&global_path, project_dir.path(), &args).unwrap();

        // Verify no duplication: each agent file is a regular file, not a dir.
        let agents_dir = project_dir.path().join(".opencode/agents/clawdmux");
        for agent in AgentKind::all() {
            let file_path = agents_dir.join(agent_file_name(agent));
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

        // Initial scaffold
        run_init_with_paths(
            &global_path,
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
        )
        .unwrap();

        // Corrupt the intake agent file
        let intake_path = project_dir
            .path()
            .join(".opencode/agents/clawdmux/intake.md");
        std::fs::write(&intake_path, "corrupted content").unwrap();
        assert_eq!(
            std::fs::read_to_string(&intake_path).unwrap(),
            "corrupted content"
        );

        // Re-run with reset_agents = true
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
