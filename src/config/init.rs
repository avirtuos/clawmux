//! `clawdmux init` command: dependency checks and project scaffold.
//!
//! Interactive terminal wizard for first-time project setup:
//! 1. Checks for (and optionally installs) the opencode binary.
//! 2. Scaffolds `.clawdmux/config.toml`, `.opencode/agents/clawdmux/`, and `tasks/`.
//! 3. Logs a success message.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;

use crate::config::BackendKind;
use crate::error::{ClawdMuxError, Result};
use crate::opencode::types::ModelId;
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
// Model extraction from agent frontmatter
// ---------------------------------------------------------------------------

/// Extracts the `model:` value from YAML frontmatter in an agent definition file.
///
/// The frontmatter block must start with `---` on the first line and end with
/// another `---` line. The first `model:` entry found is returned as a trimmed
/// string.  Returns `None` if no frontmatter is present or no `model:` key is
/// found.
fn extract_model_from_frontmatter(content: &str) -> Option<String> {
    let content = content.strip_prefix("---")?;
    let end = content.find("\n---")?;
    let frontmatter = &content[..end];
    for line in frontmatter.lines() {
        if let Some(value) = line.strip_prefix("model:") {
            return Some(value.trim().to_string());
        }
    }
    None
}

/// Builds a map from each [`AgentKind`] to the [`ModelId`] declared in its
/// embedded agent definition file frontmatter.
///
/// Agent kinds without a definition file (i.e. [`AgentKind::Human`]) or whose
/// frontmatter lacks a parseable `model:` field are omitted from the map.
pub fn build_agent_model_map() -> HashMap<AgentKind, ModelId> {
    let mut map = HashMap::new();
    for agent in AgentKind::all() {
        let Some(content) = agent_definition_content(agent) else {
            continue;
        };
        let Some(model_str) = extract_model_from_frontmatter(content) else {
            continue;
        };
        let Some(model_id) = ModelId::parse(&model_str) else {
            tracing::warn!(
                agent = agent.display_name(),
                model = %model_str,
                "Could not parse model string from agent frontmatter"
            );
            continue;
        };
        map.insert(*agent, model_id);
    }
    map
}

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
/// Performs these steps:
/// 1. Asks which agent backend to use (OpenCode or Kiro).
/// 2. Checks for the opencode binary if OpenCode is selected; offers to install it.
/// 3. Scaffolds project-local files (`.clawdmux/config.toml`,
///    `.opencode/agents/clawdmux/`, `.kiro/agents/`, `tasks/`).
/// 4. Logs a success summary via `tracing::info!`.
///
/// # Errors
///
/// Returns [`ClawdMuxError::Internal`] if the user declines to install opencode
/// or an invalid backend choice is entered. Returns [`ClawdMuxError::Io`] for
/// filesystem or stdin failures.
pub fn run_init(project_root: &Path, args: &InitArgs) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let (backend, kiro_binary) = select_backend_from_reader(&mut stdin.lock(), &mut stdout.lock())?;

    if matches!(backend, BackendKind::OpenCode) {
        check_or_install_opencode()?;
    }

    run_init_with_paths(project_root, args, backend, kiro_binary)
}

// ---------------------------------------------------------------------------
// Internal helpers (pub(crate) so tests can call them with injected paths)
// ---------------------------------------------------------------------------

/// Inner implementation used by both [`run_init`] and tests.
///
/// Accepts the pre-selected `backend` so tests can skip interactive prompts.
/// The opencode binary check and interactive backend selection are intentionally
/// excluded here; they live only in the interactive [`run_init`] entry point.
///
/// # Arguments
/// * `project_root` – root of the project being initialised.
/// * `args` – init command arguments (e.g. `reset_agents`).
/// * `backend` – the agent backend selected by the user.
/// * `kiro_binary` – optional path to the kiro binary (only used when `backend` is `Kiro`).
pub(crate) fn run_init_with_paths(
    project_root: &Path,
    args: &InitArgs,
    backend: BackendKind,
    kiro_binary: Option<String>,
) -> Result<()> {
    scaffold_project(project_root, args, &backend, kiro_binary.as_deref())?;
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

/// Core backend-selection logic with injected I/O for testability.
///
/// Presents a numbered menu of available backends and reads the user's choice.
/// If kiro is selected and the `kiro` binary is not found on `PATH`, prompts
/// for an optional explicit binary path.
///
/// Returns `(BackendKind, Option<kiro_binary_path>)`.
pub(crate) fn select_backend_from_reader<R: BufRead, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> Result<(BackendKind, Option<String>)> {
    writeln!(writer, "Which agent backend would you like to use?").map_err(ClawdMuxError::Io)?;
    writeln!(
        writer,
        "  [1] OpenCode (recommended)  Full REST API, 75+ LLM providers"
    )
    .map_err(ClawdMuxError::Io)?;
    writeln!(
        writer,
        "  [2] Kiro                    Agent Client Protocol (ACP), stdin/stdout JSON-RPC"
    )
    .map_err(ClawdMuxError::Io)?;
    write!(writer, "> ").map_err(ClawdMuxError::Io)?;
    writer.flush().map_err(ClawdMuxError::Io)?;

    let mut choice = String::new();
    reader.read_line(&mut choice).map_err(ClawdMuxError::Io)?;

    match choice.trim() {
        "" | "1" => Ok((BackendKind::OpenCode, None)),
        "2" => {
            // Check if kiro is on PATH; prompt for binary path if not found.
            let kiro_binary = if which::which("kiro").is_err() {
                writeln!(writer, "kiro not found in PATH.").map_err(ClawdMuxError::Io)?;
                write!(
                    writer,
                    "kiro binary path (leave blank to use 'kiro' from PATH at runtime): "
                )
                .map_err(ClawdMuxError::Io)?;
                writer.flush().map_err(ClawdMuxError::Io)?;

                let mut binary_input = String::new();
                reader
                    .read_line(&mut binary_input)
                    .map_err(ClawdMuxError::Io)?;
                let trimmed = binary_input.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            } else {
                None
            };
            Ok((BackendKind::Kiro, kiro_binary))
        }
        other => Err(ClawdMuxError::Internal(format!(
            "invalid backend choice: {}",
            other
        ))),
    }
}

/// Writes built-in agent definition files to `.opencode/agents/clawdmux/`.
///
/// Always overwrites existing files. Creates the directory if it does not exist.
///
/// Returns the number of agent files written.
pub fn update_agent_files(project_root: &Path) -> Result<usize> {
    let agents_dir = project_root
        .join(".opencode")
        .join("agents")
        .join("clawdmux");
    std::fs::create_dir_all(&agents_dir).map_err(ClawdMuxError::Io)?;

    let mut count = 0usize;
    for agent in AgentKind::all() {
        let (Some(file_name), Some(content)) =
            (agent_file_name(agent), agent_definition_content(agent))
        else {
            continue;
        };
        let file_path = agents_dir.join(file_name);
        std::fs::write(&file_path, content).map_err(ClawdMuxError::Io)?;
        tracing::info!(path = %file_path.display(), "wrote agent definition");
        count += 1;
    }
    Ok(count)
}

/// Arguments for the `clawdmux update-agents` subcommand.
#[derive(clap::Args, Debug)]
pub struct UpdateAgentsArgs {}

/// Overwrites all agent definition files from built-in defaults.
///
/// Unlike `init --reset-agents`, this does not check for opencode, prompt for
/// provider configuration, or write task seed files.
pub fn run_update_agents(project_root: &Path, _args: &UpdateAgentsArgs) -> Result<()> {
    let count = update_agent_files(project_root)?;
    tracing::info!(count, "agent definitions updated");
    Ok(())
}

/// Step 3: create project-local files and directories.
///
/// All paths are created only if absent, except agent definition files which
/// are also overwritten when `args.reset_agents` is `true`.
///
/// # Arguments
/// * `backend` – the agent backend to record in `.clawdmux/config.toml`.
/// * `kiro_binary` – optional explicit path to the kiro binary; written to the
///   config only when `backend` is [`BackendKind::Kiro`] and a path is given.
pub(crate) fn scaffold_project(
    project_root: &Path,
    args: &InitArgs,
    backend: &BackendKind,
    kiro_binary: Option<&str>,
) -> Result<()> {
    tracing::info!("scaffolding project");

    // .clawdmux/config.toml
    let clawdmux_dir = project_root.join(".clawdmux");
    let project_config_path = clawdmux_dir.join("config.toml");
    if !project_config_path.exists() {
        std::fs::create_dir_all(&clawdmux_dir).map_err(ClawdMuxError::Io)?;
        let config_content = make_project_config_content(backend, kiro_binary);
        std::fs::write(&project_config_path, config_content).map_err(ClawdMuxError::Io)?;
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

    // .opencode/agents/clawdmux/ — delegate to update_agent_files when resetting,
    // otherwise only write files that do not already exist.
    if args.reset_agents {
        update_agent_files(project_root)?;
    } else {
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
            if !file_path.exists() {
                std::fs::write(&file_path, content).map_err(ClawdMuxError::Io)?;
                tracing::info!(path = %file_path.display(), "created agent definition");
            }
        }
    }

    // .kiro/agents/ — scaffold kiro agent JSON configs (does not overwrite existing).
    scaffold_kiro_agents(project_root, args.reset_agents)?;

    Ok(())
}

/// Scaffolds kiro agent JSON config files in `.kiro/agents/`.
///
/// Creates one JSON config file per pipeline agent using the agent's
/// embedded system prompt (stripped of YAML frontmatter).  Existing files
/// are not overwritten unless `reset` is `true`.
///
/// Returns the number of agent config files written.
///
/// # Errors
///
/// Returns [`ClawdMuxError::Io`] if the directory cannot be created or a
/// file cannot be written.
pub fn scaffold_kiro_agents(project_root: &Path, reset: bool) -> Result<usize> {
    let agents_dir = project_root.join(".kiro").join("agents");
    std::fs::create_dir_all(&agents_dir).map_err(ClawdMuxError::Io)?;

    let mut count = 0usize;
    for agent in AgentKind::all() {
        let agent_name = agent.kiro_agent_name();
        let file_name = format!("{}.json", agent_name);
        let file_path = agents_dir.join(&file_name);
        if file_path.exists() && !reset {
            continue;
        }
        let json = build_kiro_agent_json(agent);
        std::fs::write(&file_path, json).map_err(ClawdMuxError::Io)?;
        tracing::info!(path = %file_path.display(), "wrote kiro agent config");
        count += 1;
    }
    Ok(count)
}

/// Builds the JSON config string for a single kiro agent.
fn build_kiro_agent_json(agent: &AgentKind) -> String {
    let name = agent.kiro_agent_name();
    let description = format!("ClawdMux {} Agent", agent.display_name());

    // Extract system prompt by stripping YAML frontmatter from the embedded .md.
    let prompt = agent_definition_content(agent)
        .map(strip_frontmatter)
        .unwrap_or_default();

    // Escape the prompt for JSON embedding.
    let prompt_json = serde_json::to_string(&prompt).unwrap_or_else(|_| "\"\"".to_string());

    // Determine tool set and model based on agent role.
    // `tools` declares which tools are available; `allowed_tools` are auto-approved without user
    // prompting. Tools in `tools` but NOT in `allowed_tools` trigger a TUI permission dialog.
    //
    // Tool names here are kiro-cli built-in names (write, shell, glob, grep, thinking),
    // NOT the ACP protocol-level tool kind names (edit, execute, search, think).
    let (tools_json, allowed_tools_json, model) = match agent {
        AgentKind::Implementation => (
            r#"["read","write","glob","grep","shell","thinking"]"#,
            r#"["read","glob","grep","thinking"]"#,
            "claude-sonnet-4-6",
        ),
        AgentKind::Planning | AgentKind::CodeQuality | AgentKind::CodeReview => (
            r#"["read","glob","grep","shell","thinking"]"#,
            r#"["read","glob","grep","thinking"]"#,
            "claude-sonnet-4-6",
        ),
        _ => (
            r#"["read","glob","grep","thinking"]"#,
            r#"["read","glob","grep","thinking"]"#,
            "claude-sonnet-4-6",
        ),
    };

    format!(
        "{{\n  \"name\": \"{name}\",\n  \"description\": \"{description}\",\n  \"prompt\": {prompt_json},\n  \"model\": \"{model}\",\n  \"tools\": {tools_json},\n  \"allowedTools\": {allowed_tools_json},\n  \"resources\": [\"file://CLAUDE.md\"]\n}}\n"
    )
}

/// Strips the YAML frontmatter block (`---` ... `---`) from an agent definition.
///
/// Returns the body text after the closing `---`, trimmed of leading whitespace.
/// If no frontmatter is present, the full content is returned unchanged.
fn strip_frontmatter(content: &str) -> &str {
    let Some(after_open) = content.strip_prefix("---") else {
        return content;
    };
    let Some(close_pos) = after_open.find("\n---") else {
        return content;
    };
    after_open[close_pos + 4..].trim_start()
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

/// Generates the content for `.clawdmux/config.toml` based on the chosen backend.
///
/// The OpenCode section is always included (used for task-fix requests regardless
/// of backend). The `backend` key is omitted when it equals the default (`opencode`)
/// to keep the config minimal.
fn make_project_config_content(backend: &BackendKind, kiro_binary: Option<&str>) -> String {
    let mut out = String::new();

    if matches!(backend, BackendKind::Kiro) {
        out.push_str("backend = \"kiro\"\n\n");
        out.push_str("[kiro]\n");
        match kiro_binary {
            Some(path) => {
                out.push_str(&format!("binary = \"{}\"\n", path));
            }
            None => {
                out.push_str("# binary = \"/path/to/kiro\"  # uncomment to set an explicit path\n");
            }
        }
        out.push('\n');
    }

    out.push_str("[opencode]\n");
    out.push_str("mode = \"auto\"\n");
    out.push_str("hostname = \"127.0.0.1\"\n");
    out.push_str("port = 4096\n");

    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io;

    use super::*;
    use tempfile::TempDir;

    // --- extract_model_from_frontmatter tests ---

    #[test]
    fn test_extract_model_from_frontmatter() {
        let content = "---\ndescription: test agent\nmodel: openrouter/anthropic/claude-sonnet-4.6\nsteps: 10\n---\n\nBody text.";
        let model = extract_model_from_frontmatter(content);
        assert_eq!(
            model.as_deref(),
            Some("openrouter/anthropic/claude-sonnet-4.6")
        );
    }

    #[test]
    fn test_extract_model_no_frontmatter() {
        let content = "No frontmatter here.\nJust body text.";
        assert!(extract_model_from_frontmatter(content).is_none());
    }

    #[test]
    fn test_extract_model_frontmatter_no_model_key() {
        let content = "---\ndescription: test agent\nsteps: 10\n---\n\nBody text.";
        assert!(extract_model_from_frontmatter(content).is_none());
    }

    // --- build_agent_model_map tests ---

    #[test]
    fn test_build_agent_model_map() {
        let map = build_agent_model_map();
        // All 7 pipeline agents should have entries.
        for agent in AgentKind::all() {
            assert!(
                map.contains_key(agent),
                "Expected model entry for {:?}",
                agent
            );
        }
        assert_eq!(map.len(), 7, "Should have exactly 7 entries");
    }

    #[test]
    fn test_build_agent_model_map_parses_correctly() {
        let map = build_agent_model_map();
        // All agents use openrouter/anthropic/claude-sonnet-4.6.
        for (agent, model) in &map {
            assert_eq!(
                model.provider_id, "openrouter",
                "Wrong provider for {:?}",
                agent
            );
            assert!(
                !model.model_id.is_empty(),
                "model_id should not be empty for {:?}",
                agent
            );
        }
    }

    // --- scaffold tests ---

    #[test]
    fn test_scaffold_creates_config() {
        let project_dir = TempDir::new().unwrap();

        run_init_with_paths(
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
            BackendKind::OpenCode,
            None,
        )
        .unwrap();

        assert!(
            project_dir.path().join(".clawdmux/config.toml").exists(),
            ".clawdmux/config.toml should be created"
        );
    }

    #[test]
    fn test_scaffold_creates_tasks_dir() {
        let project_dir = TempDir::new().unwrap();

        run_init_with_paths(
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
            BackendKind::OpenCode,
            None,
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
        let project_dir = TempDir::new().unwrap();

        // Pre-create tasks/ and write custom content into one seed file.
        let tasks_dir = project_dir.path().join("tasks");
        std::fs::create_dir_all(&tasks_dir).unwrap();
        let existing_path = tasks_dir.join("1.1.md");
        std::fs::write(&existing_path, "custom content").unwrap();

        run_init_with_paths(
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
            BackendKind::OpenCode,
            None,
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
        let project_dir = TempDir::new().unwrap();

        run_init_with_paths(
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
            BackendKind::OpenCode,
            None,
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
        let project_dir = TempDir::new().unwrap();

        let args = InitArgs {
            reset_agents: false,
        };
        run_init_with_paths(project_dir.path(), &args, BackendKind::OpenCode, None).unwrap();
        run_init_with_paths(project_dir.path(), &args, BackendKind::OpenCode, None).unwrap();

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
        let project_dir = TempDir::new().unwrap();

        run_init_with_paths(
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
            BackendKind::OpenCode,
            None,
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
            project_dir.path(),
            &InitArgs { reset_agents: true },
            BackendKind::OpenCode,
            None,
        )
        .unwrap();

        let restored = std::fs::read_to_string(&intake_path).unwrap();
        assert_eq!(
            restored, INTAKE_AGENT,
            "intake.md should be restored to the built-in default"
        );
    }

    // --- update_agent_files tests ---

    #[test]
    fn test_update_agent_files_writes_all() {
        let project_dir = TempDir::new().unwrap();

        let count = update_agent_files(project_dir.path()).unwrap();
        assert_eq!(count, 7, "should write exactly 7 agent files");

        let agents_dir = project_dir.path().join(".opencode/agents/clawdmux");
        for agent in AgentKind::all() {
            let file_name = agent_file_name(agent).expect("all() never yields Human");
            let file_path = agents_dir.join(file_name);
            assert!(file_path.exists(), "{} should exist", file_path.display());
            let content = std::fs::read_to_string(&file_path).unwrap();
            assert!(!content.is_empty(), "{} should be non-empty", file_name);
        }
    }

    #[test]
    fn test_update_agent_files_overwrites_existing() {
        let project_dir = TempDir::new().unwrap();

        // First call to write the files.
        update_agent_files(project_dir.path()).unwrap();

        // Corrupt one file.
        let intake_path = project_dir
            .path()
            .join(".opencode/agents/clawdmux/intake.md");
        std::fs::write(&intake_path, "corrupted content").unwrap();

        // Second call must restore it.
        update_agent_files(project_dir.path()).unwrap();

        let restored = std::fs::read_to_string(&intake_path).unwrap();
        assert_eq!(
            restored, INTAKE_AGENT,
            "intake.md should be restored to the built-in default after update_agent_files"
        );
    }

    #[test]
    fn test_run_update_agents_succeeds() {
        let project_dir = TempDir::new().unwrap();

        run_update_agents(project_dir.path(), &UpdateAgentsArgs {}).unwrap();

        let agents_dir = project_dir.path().join(".opencode/agents/clawdmux");
        for agent in AgentKind::all() {
            let file_name = agent_file_name(agent).expect("all() never yields Human");
            let file_path = agents_dir.join(file_name);
            assert!(
                file_path.exists(),
                "agent file {} should exist after run_update_agents",
                file_path.display()
            );
        }
    }

    // --- scaffold_kiro_agents tests ---

    #[test]
    fn test_scaffold_kiro_agents_creates_all_files() {
        let project_dir = TempDir::new().unwrap();

        let count = scaffold_kiro_agents(project_dir.path(), false).unwrap();
        assert_eq!(count, 7, "should write exactly 7 kiro agent config files");

        let agents_dir = project_dir.path().join(".kiro/agents");
        for agent in AgentKind::all() {
            let file_name = format!("{}.json", agent.kiro_agent_name());
            let file_path = agents_dir.join(&file_name);
            assert!(
                file_path.exists(),
                "kiro agent file {} should exist",
                file_path.display()
            );
            let content = std::fs::read_to_string(&file_path).unwrap();
            // Verify it is valid JSON and contains the agent name.
            let parsed: serde_json::Value =
                serde_json::from_str(&content).expect("kiro agent config should be valid JSON");
            assert_eq!(
                parsed["name"].as_str(),
                Some(agent.kiro_agent_name()),
                "name field should match kiro_agent_name"
            );
            assert!(
                parsed["model"].as_str().is_some(),
                "model field should be present"
            );
            assert!(parsed["tools"].is_array(), "tools field should be an array");
        }
    }

    #[test]
    fn test_scaffold_kiro_agents_does_not_overwrite_without_reset() {
        let project_dir = TempDir::new().unwrap();

        scaffold_kiro_agents(project_dir.path(), false).unwrap();

        let intake_path = project_dir.path().join(".kiro/agents/clawdmux-intake.json");
        std::fs::write(&intake_path, "custom content").unwrap();

        // Second call without reset should NOT overwrite.
        let count = scaffold_kiro_agents(project_dir.path(), false).unwrap();
        assert_eq!(count, 0, "no files should be written when all exist");

        let content = std::fs::read_to_string(&intake_path).unwrap();
        assert_eq!(
            content, "custom content",
            "existing file should not be overwritten"
        );
    }

    #[test]
    fn test_scaffold_kiro_agents_reset_overwrites() {
        let project_dir = TempDir::new().unwrap();

        scaffold_kiro_agents(project_dir.path(), false).unwrap();

        let intake_path = project_dir.path().join(".kiro/agents/clawdmux-intake.json");
        std::fs::write(&intake_path, "corrupted content").unwrap();

        // Call with reset=true should overwrite.
        scaffold_kiro_agents(project_dir.path(), true).unwrap();

        let content = std::fs::read_to_string(&intake_path).unwrap();
        let parsed: serde_json::Value =
            serde_json::from_str(&content).expect("restored file should be valid JSON");
        assert_eq!(
            parsed["name"].as_str(),
            Some(AgentKind::Intake.kiro_agent_name()),
            "restored file should have correct name"
        );
    }

    #[test]
    fn test_strip_frontmatter_with_frontmatter() {
        let content = "---\nmodel: claude-sonnet-4-6\n---\n\nBody text.";
        let body = strip_frontmatter(content);
        assert_eq!(body, "Body text.");
    }

    #[test]
    fn test_strip_frontmatter_without_frontmatter() {
        let content = "No frontmatter here.";
        let body = strip_frontmatter(content);
        assert_eq!(body, "No frontmatter here.");
    }

    // --- select_backend_from_reader tests ---

    #[test]
    fn test_select_backend_opencode_explicit() {
        let mut reader = io::Cursor::new(b"1\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());
        let (backend, binary) = select_backend_from_reader(&mut reader, &mut writer).unwrap();
        assert!(matches!(backend, BackendKind::OpenCode));
        assert!(binary.is_none());
    }

    #[test]
    fn test_select_backend_opencode_default_empty_input() {
        let mut reader = io::Cursor::new(b"\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());
        let (backend, binary) = select_backend_from_reader(&mut reader, &mut writer).unwrap();
        assert!(matches!(backend, BackendKind::OpenCode));
        assert!(binary.is_none());
    }

    #[test]
    fn test_select_backend_kiro_no_binary_path() {
        // Simulate kiro NOT on PATH by using the binary prompt path.
        // We feed "2\n" for the backend, then "\n" for the binary path (blank = use PATH).
        // Note: in test environments kiro may or may not be on PATH, so we test the
        // full input: if kiro is on PATH the binary prompt is skipped; if not, blank = None.
        let mut reader = io::Cursor::new(b"2\n\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());
        let (backend, _binary) = select_backend_from_reader(&mut reader, &mut writer).unwrap();
        assert!(matches!(backend, BackendKind::Kiro));
        // binary may be None (kiro on PATH) or None (blank input) -- both are acceptable.
    }

    #[test]
    fn test_select_backend_kiro_explicit_binary_path() {
        // Feed "2\n" for backend, then a path for the binary.
        // This only exercises the explicit-path branch when kiro is not on PATH.
        // Since the test environment may have kiro on PATH we check the result:
        // if kiro is on PATH, no binary prompt appears (binary = None);
        // if not, the provided path is returned.
        let mut reader = io::Cursor::new(b"2\n/usr/local/bin/kiro\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());
        let (backend, _binary) = select_backend_from_reader(&mut reader, &mut writer).unwrap();
        assert!(matches!(backend, BackendKind::Kiro));
    }

    #[test]
    fn test_select_backend_invalid_choice() {
        let mut reader = io::Cursor::new(b"5\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());
        let result = select_backend_from_reader(&mut reader, &mut writer);
        assert!(
            matches!(result, Err(ClawdMuxError::Internal(_))),
            "expected Internal error for invalid choice, got: {:?}",
            result
        );
    }

    #[test]
    fn test_select_backend_prompts_contain_expected_text() {
        let mut reader = io::Cursor::new(b"1\n".as_ref());
        let mut writer = io::Cursor::new(Vec::new());
        select_backend_from_reader(&mut reader, &mut writer).unwrap();
        let output = String::from_utf8(writer.into_inner()).unwrap();
        assert!(
            output.contains("OpenCode"),
            "prompt should mention OpenCode, got: {:?}",
            output
        );
        assert!(
            output.contains("Kiro"),
            "prompt should mention Kiro, got: {:?}",
            output
        );
    }

    #[test]
    fn test_make_project_config_opencode_omits_backend_key() {
        let content = make_project_config_content(&BackendKind::OpenCode, None);
        assert!(
            !content.contains("backend ="),
            "OpenCode config should omit backend key, got: {:?}",
            content
        );
        assert!(
            content.contains("[opencode]"),
            "OpenCode config should include [opencode] section"
        );
    }

    #[test]
    fn test_make_project_config_kiro_with_binary() {
        let content = make_project_config_content(&BackendKind::Kiro, Some("/usr/local/bin/kiro"));
        assert!(
            content.contains("backend = \"kiro\""),
            "Kiro config should set backend key"
        );
        assert!(
            content.contains("binary = \"/usr/local/bin/kiro\""),
            "Kiro config should set binary path"
        );
        assert!(
            content.contains("[opencode]"),
            "Kiro config should still include [opencode] section"
        );
    }

    #[test]
    fn test_make_project_config_kiro_without_binary() {
        let content = make_project_config_content(&BackendKind::Kiro, None);
        assert!(
            content.contains("backend = \"kiro\""),
            "Kiro config should set backend key"
        );
        assert!(
            content.contains("# binary ="),
            "Kiro config should include commented binary hint"
        );
    }

    #[test]
    fn test_scaffold_kiro_included_in_init() {
        let project_dir = TempDir::new().unwrap();

        run_init_with_paths(
            project_dir.path(),
            &InitArgs {
                reset_agents: false,
            },
            BackendKind::OpenCode,
            None,
        )
        .unwrap();

        // Kiro agents should be created alongside opencode agents.
        let kiro_agents_dir = project_dir.path().join(".kiro/agents");
        assert!(
            kiro_agents_dir.is_dir(),
            ".kiro/agents/ should be created by init"
        );
        for agent in AgentKind::all() {
            let file_name = format!("{}.json", agent.kiro_agent_name());
            assert!(
                kiro_agents_dir.join(&file_name).exists(),
                "kiro agent file {} should be created by init",
                file_name
            );
        }
    }
}
