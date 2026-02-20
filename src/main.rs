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

use std::time::Duration;

use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use tokio::time::MissedTickBehavior;

use crate::app::App;
use crate::messages::AppMessage;
use crate::tasks::TaskStore;

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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Log to a file so that log output does not corrupt the TUI display.
    // Prefer `.clawdmux/` (created by `clawdmux init`); fall back to the
    // platform data-local directory so we never pollute the project root.
    let log_dir = {
        let cwd = std::env::current_dir()?;
        let local_dir = cwd.join(".clawdmux");
        if local_dir.exists() {
            local_dir
        } else {
            let fallback = dirs::data_local_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join("clawdmux");
            std::fs::create_dir_all(&fallback)?;
            fallback
        }
    };
    let file_appender = tracing_appender::rolling::never(log_dir, "clawdmux.log");
    tracing_subscriber::fmt().with_writer(file_appender).init();

    let cli = Cli::parse();

    tracing::info!("ClawdMux starting");

    match cli.command {
        Some(Commands::Init(args)) => {
            tracing::info!("ClawdMux init command invoked");
            let project_root = std::env::current_dir()?;
            config::init::run_init(&project_root, &args)?;
        }
        None => {
            run_tui().await?;
        }
    }

    Ok(())
}

/// Runs the full-screen TUI event loop.
///
/// Loads tasks from disk, initializes the ratatui terminal, installs a panic
/// hook that restores the terminal before printing, then drives the event loop
/// until the user quits.
async fn run_tui() -> Result<(), Box<dyn std::error::Error>> {
    // Load tasks from the current working directory.
    let mut task_store = TaskStore::new();
    let project_root = std::env::current_dir()?;
    match task_store.load_from_disk(&project_root) {
        Ok(count) => tracing::info!("Loaded {} tasks from disk", count),
        Err(e) => tracing::warn!("Could not load tasks from disk: {}", e),
    }

    let mut app = App::new(task_store);

    // Install a panic hook that restores the terminal before printing.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original_hook(info);
    }));

    let mut terminal = ratatui::init();
    let mut event_stream = crossterm::event::EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(250));
    tick_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    loop {
        terminal.draw(|frame| tui::draw(frame, &app))?;

        let messages: Vec<AppMessage> = tokio::select! {
            maybe_event = event_stream.next() => {
                if let Some(Ok(event)) = maybe_event {
                    app.handle_message(AppMessage::TerminalEvent(event))
                } else {
                    vec![]
                }
            }
            _ = tick_interval.tick() => {
                app.handle_message(AppMessage::Tick)
            }
            _ = tokio::signal::ctrl_c() => {
                app.handle_message(AppMessage::Shutdown)
            }
        };

        // Dispatch any follow-up messages produced by the handler,
        // including messages produced by follow-up handlers themselves.
        let mut queue = std::collections::VecDeque::from(messages);
        while let Some(msg) = queue.pop_front() {
            queue.extend(app.handle_message(msg));
        }

        if app.should_quit {
            break;
        }
    }

    ratatui::restore();
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
