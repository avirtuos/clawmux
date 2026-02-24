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

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use futures_util::StreamExt;
use tokio::sync::{mpsc, RwLock};
use tokio::time::MissedTickBehavior;
use tracing_subscriber::EnvFilter;

use crate::app::App;
use crate::config::AppConfig;
use crate::messages::AppMessage;
use crate::opencode::events::EventStreamConsumer;
use crate::opencode::server::OpenCodeServer;
use crate::opencode::types::{MessagePart, OpenCodeEvent};
use crate::opencode::OpenCodeClient;
use crate::tasks::models::TaskId;
use crate::tasks::TaskStore;
use crate::workflow::agents::AgentKind;

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
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(file_appender)
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

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
/// Initializes the terminal first so a loading screen can be displayed while
/// the opencode server starts up. Installs a panic hook after `ratatui::init()`
/// so it wraps ratatui's own hook. Drives the event loop until the user quits.
async fn run_tui() -> Result<(), Box<dyn std::error::Error>> {
    // 1. Init terminal first so we can draw a loading screen during startup.
    let mut terminal = ratatui::init();

    // 2. Install panic hook AFTER ratatui::init() so it wraps ratatui's hook.
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ratatui::restore();
        original_hook(info);
    }));

    // 3. Loading phase: tasks
    terminal.draw(|f| tui::draw_loading_screen(f, "Loading tasks..."))?;
    let mut task_store = TaskStore::new();
    let project_root = std::env::current_dir()?;
    match task_store.load_from_disk(&project_root) {
        Ok(count) => tracing::info!("Loaded {} tasks from disk", count),
        Err(e) => tracing::warn!("Could not load tasks from disk: {}", e),
    }

    // 4. Loading phase: config
    terminal.draw(|f| tui::draw_loading_screen(f, "Loading configuration..."))?;
    let config = AppConfig::load(&project_root)?;

    // 5. Loading phase: server (callback redraws loading screen on each status change).
    // Use a crossterm EventStream to detect Ctrl+C in raw mode (ratatui disables
    // the kernel ISIG flag, so tokio::signal::ctrl_c() is ineffective here).
    let mut startup_events = crossterm::event::EventStream::new();
    let mut server = tokio::select! {
        result = OpenCodeServer::ensure_running(&config, |status| {
            let _ = terminal.draw(|f| tui::draw_loading_screen(f, status));
        }) => {
            match result {
                Ok(s) => {
                    tracing::info!("OpenCode server at {}", s.base_url());
                    Some(s)
                }
                Err(e) => {
                    tracing::warn!("OpenCode server unavailable, continuing without it: {}", e);
                    let _ = terminal.draw(|f| {
                        tui::draw_loading_screen(f, "OpenCode server unavailable, starting without it");
                    });
                    None
                }
            }
        }
        _ = async {
            use crossterm::event::{Event, KeyCode, KeyModifiers};
            while let Some(Ok(event)) = startup_events.next().await {
                if let Event::Key(key) = event {
                    if key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        return;
                    }
                }
            }
        } => {
            tracing::info!("Ctrl+C received during startup, exiting");
            ratatui::restore();
            return Ok(());
        }
    };
    // Drop startup_events so the main loop's EventStream can take over stdin.
    drop(startup_events);

    // 6. Build the shared channel (64-slot), session map, and optional OpenCodeClient.
    let (async_tx, mut async_rx) = mpsc::channel::<AppMessage>(64);
    let session_map = Arc::new(RwLock::new(HashMap::new()));

    let opencode_client: Option<Arc<OpenCodeClient>> = server.as_ref().map(|s| {
        let base_url = s.base_url().to_string();
        let auth = if config.has_explicit_password() {
            Some(("clawdmux".to_string(), config.effective_opencode_password()))
        } else {
            None
        };
        Arc::new(OpenCodeClient::new(base_url, auth))
    });

    // 7. Spawn the EventStreamConsumer if the server is available.
    if let Some(ref s) = server {
        let base_url = s.base_url().to_string();
        let mut consumer = EventStreamConsumer::new(async_tx.clone(), Arc::clone(&session_map));
        tokio::spawn(async move {
            if let Err(e) = consumer.run(base_url).await {
                tracing::warn!("EventStreamConsumer exited: {}", e);
            }
        });
    }

    let mut app = App::new(
        task_store,
        opencode_client,
        Arc::clone(&session_map),
        async_tx.clone(),
    );
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
            Some(async_msg) = async_rx.recv() => {
                app.handle_message(async_msg)
            }
        };

        // Dispatch any follow-up messages produced by the handler,
        // including messages produced by follow-up handlers themselves.
        let mut queue = std::collections::VecDeque::from(messages);
        while let Some(msg) = queue.pop_front() {
            // Intercept RequestTaskFix to spawn an async fix task.
            if let AppMessage::RequestTaskFix { ref task_id } = msg {
                spawn_fix_request(task_id, &app, &server, &config, async_tx.clone());
            }
            queue.extend(app.handle_message(msg));
        }

        if app.should_quit {
            break;
        }
    }

    ratatui::restore();

    if let Some(ref mut s) = server {
        if let Err(e) = s.shutdown().await {
            tracing::warn!("OpenCode server shutdown error: {}", e);
        }
    }

    Ok(())
}

/// Spawns an async task to request an AI-generated fix for a malformed task file.
///
/// Extracts the raw content and error from the task store, then spawns a tokio
/// task that creates an OpenCode session, sends a fix prompt, collects the
/// response via SSE, and sends the result back through `async_tx`.
///
/// If the OpenCode server is unavailable or the task data cannot be found,
/// sends a [`AppMessage::TaskFixFailed`] immediately.
fn spawn_fix_request(
    task_id: &TaskId,
    app: &App,
    server: &Option<OpenCodeServer>,
    config: &AppConfig,
    async_tx: tokio::sync::mpsc::Sender<AppMessage>,
) {
    let base_url = match server {
        Some(s) => s.base_url().to_string(),
        None => {
            let task_id = task_id.clone();
            tokio::spawn(async move {
                let _ = async_tx
                    .send(AppMessage::TaskFixFailed {
                        task_id,
                        error: "OpenCode server unavailable".to_string(),
                    })
                    .await;
            });
            return;
        }
    };

    let (raw_content, error_message) = match app.task_store.get(task_id) {
        Some(t) => match t.parse_error.as_ref() {
            Some(e) => (e.raw_content.clone(), e.error_message.clone()),
            None => return,
        },
        None => return,
    };

    let task_id = task_id.clone();
    let auth = if config.has_explicit_password() {
        Some(("clawdmux".to_string(), config.effective_opencode_password()))
    } else {
        None
    };

    tokio::spawn(async move {
        let client = OpenCodeClient::new(base_url.clone(), auth);

        let session = match client.create_session().await {
            Ok(s) => s,
            Err(e) => {
                let _ = async_tx
                    .send(AppMessage::TaskFixFailed {
                        task_id,
                        error: format!("Failed to create session: {}", e),
                    })
                    .await;
                return;
            }
        };

        let prompt = build_fix_prompt(&error_message, &raw_content);
        if let Err(e) = client
            .send_prompt_async(&session.id, &AgentKind::Implementation, &prompt)
            .await
        {
            let _ = async_tx
                .send(AppMessage::TaskFixFailed {
                    task_id,
                    error: format!("Failed to send fix prompt: {}", e),
                })
                .await;
            return;
        }

        // Subscribe to the global SSE stream and collect text for our session.
        let url = format!("{}/global/event", base_url);
        let request = reqwest::Client::new().get(&url);
        let mut es = match reqwest_eventsource::EventSource::new(request) {
            Ok(es) => es,
            Err(e) => {
                let _ = async_tx
                    .send(AppMessage::TaskFixFailed {
                        task_id,
                        error: format!("Failed to open SSE stream: {}", e),
                    })
                    .await;
                return;
            }
        };

        let mut collected_text = String::new();
        while let Some(event) = es.next().await {
            match event {
                Ok(reqwest_eventsource::Event::Message(msg)) => {
                    match serde_json::from_str::<OpenCodeEvent>(&msg.data) {
                        Ok(OpenCodeEvent::MessageUpdated {
                            session_id, parts, ..
                        }) if session_id == session.id => {
                            for part in &parts {
                                if let MessagePart::Text { text } = part {
                                    collected_text = text.clone();
                                }
                            }
                        }
                        Ok(OpenCodeEvent::SessionCompleted { session_id })
                            if session_id == session.id =>
                        {
                            break;
                        }
                        Ok(OpenCodeEvent::SessionError { session_id, error })
                            if session_id == session.id =>
                        {
                            let _ = async_tx
                                .send(AppMessage::TaskFixFailed {
                                    task_id,
                                    error: format!("Session error: {}", error),
                                })
                                .await;
                            return;
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    let _ = async_tx
                        .send(AppMessage::TaskFixFailed {
                            task_id,
                            error: format!("SSE stream error: {}", e),
                        })
                        .await;
                    return;
                }
                _ => {}
            }
        }

        if collected_text.is_empty() {
            let _ = async_tx
                .send(AppMessage::TaskFixFailed {
                    task_id,
                    error: "No fix content received from AI".to_string(),
                })
                .await;
            return;
        }

        let _ = async_tx
            .send(AppMessage::TaskFixReady {
                task_id,
                corrected_content: collected_text,
                explanation: "AI-generated fix suggestion".to_string(),
            })
            .await;
    });
}

/// Builds the fix prompt to send to the AI for a malformed task file.
fn build_fix_prompt(error_message: &str, raw_content: &str) -> String {
    format!(
        "The following task markdown file failed to parse.\n\n\
         ERROR: {error_message}\n\n\
         Expected format:\n\
         Story: <story name>\n\
         Task: <task name>\n\
         Status: OPEN | IN_PROGRESS | PENDING_REVIEW | COMPLETED | ABANDONED\n\
         Assigned To: [<Agent Name>]  (optional)\n\n\
         ## Description\n\
         <text>\n\n\
         Raw file content:\n\
         {raw_content}\n\n\
         Output ONLY the corrected markdown. No code fences, no explanation.\n\
         Preserve as much original content as possible."
    )
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
