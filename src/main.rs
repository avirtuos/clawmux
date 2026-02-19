//! Entry point for ClawdMux.
//!
//! Bootstraps logging, parses CLI arguments, and dispatches to the appropriate command.

mod app;
mod config;
mod error;
mod messages;
mod opencode;
mod tasks;
mod tui;
mod workflow;

use clap::{Parser, Subcommand};

/// ClawdMux: GenAI coding assistance multiplexer and task orchestrator.
#[derive(Parser, Debug)]
#[command(name = "clawdmux", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

/// Available CLI subcommands.
#[derive(Subcommand, Debug)]
enum Commands {
    /// Initialize the project for use with ClawdMux.
    Init(config::init::InitArgs),
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    tracing::info!("ClawdMux starting");

    match cli.command {
        Some(Commands::Init(args)) => {
            tracing::info!("ClawdMux init command invoked");
            let project_root = std::env::current_dir()?;
            config::init::run_init(&project_root, &args)?;
        }
        None => {
            //TODO: Task 4.1 -- launch TUI event loop with tokio::main
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_modules_accessible() {
        let _ = std::any::type_name::<app::App>();
        let _ = std::any::type_name::<error::ClawdMuxError>();
        let _ = std::any::type_name::<messages::AppMessage>();
        let _ = std::any::type_name::<workflow::agents::AgentKind>();
        assert!(true);
    }
}
