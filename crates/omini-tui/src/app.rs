use super::client;
use super::render;
use super::state::UiState;
use super::update;
use crate::setup;
use crate::terminal;
use crate::types::events::{ActiveProfile, RuntimeToUiEvent};
use crossterm::event::{self, Event};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

const STREAMING_TICK_RATE: Duration = Duration::from_millis(100);
const IDLE_TICK_RATE: Duration = Duration::from_millis(50);

pub(crate) async fn run_ui_async(connection: client::StartupConnection) -> io::Result<()> {
    match connection {
        client::StartupConnection::Project(connection) => run_project_ui_async(*connection).await,
        client::StartupConnection::Configuration(connection) => {
            if let Some(connection) = setup::run(connection).await? {
                run_project_ui_async(connection).await
            } else {
                Ok(())
            }
        }
    }
}

async fn run_project_ui_async(connection: client::ProjectConnection) -> io::Result<()> {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        terminal::safe_restore();
        prev_hook(panic);
    }));

    let mut terminal = terminal::init()?;
    let mut state = UiState::new();
    let open = &connection.open;
    let cwd = std::path::PathBuf::from(&open.project.path);

    state.status_bar.model = open.model.clone();
    state.status_bar.thinking_effort = open
        .thinking_effort
        .map(client::thinking_effort_from_protocol);
    state.status_bar.active_provider = open.active_provider.clone();
    state.status_bar.cwd = cwd.clone();
    state.status_bar.git_branch = open.git_branch.clone();
    state.status_bar.active_profile = ActiveProfile::Main;
    state.startup_mcp_server_count = open.mcp_server_count;
    state.startup_has_project_instructions = open.has_project_instructions;
    state.show_thinking_blocks = open.show_thinking_blocks;
    state.status_bar.context_window = open.context_window;
    state.startup_recent_threads = open
        .threads
        .clone()
        .into_iter()
        .map(client::thread_summary_from_protocol)
        .filter(|thread| !thread.title.trim().is_empty())
        .take(6)
        .collect();
    state.autocomplete.all_commands = crate::command::commands_with_runtime_skills(
        open.skills
            .clone()
            .into_iter()
            .map(client::skill_command_summary)
            .collect(),
    );
    state.set_mention_context(
        cwd,
        crate::state::agent_summaries_to_mention_candidates(
            open.agents
                .clone()
                .into_iter()
                .map(client::agent_summary_from_protocol)
                .collect(),
        ),
    );

    let running = Arc::new(AtomicBool::new(true));
    let thread_running = running.clone();

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Event>();
    let input_handle = tokio::task::spawn_blocking(move || {
        while thread_running.load(Ordering::Relaxed) {
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                match event::read() {
                    Ok(event) => {
                        if input_tx.send(event).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        }
    });

    let (agent_tx, mut agent_rx) = mpsc::channel::<RuntimeToUiEvent>(256);
    let (request_tx, request_rx) = mpsc::channel::<client::ClientRequest>(256);

    state.runtime_handle = Some(client::spawn_project_client(
        connection,
        agent_tx.clone(),
        request_rx,
    ));

    terminal.draw(|frame| render::render(&mut state, frame))?;

    let mut tick_rate = IDLE_TICK_RATE;
    let mut last_tick = tokio::time::Instant::now();

    let result = loop {
        tokio::select! {
            Some(event) = input_rx.recv() => {
                let outcome = update::handle_input_event(&mut state, event, &request_tx).await;
                if outcome.exit {
                    break Ok(());
                }
                if outcome.redraw {
                    last_tick = tokio::time::Instant::now();
                    terminal.draw(|frame| render::render(&mut state, frame))?;
                }
            }
            Some(agent_evt) = agent_rx.recv() => {
                let outcome = update::handle_runtime_event(&mut state, agent_evt, &request_tx).await;
                if outcome.exit {
                    break Ok(());
                }
            }
            _ = tokio::time::sleep_until(last_tick + tick_rate) => {
                let outcome = update::drain_runtime_events(&mut state, &mut agent_rx, &request_tx).await;
                if outcome.exit {
                    break Ok(());
                }
                if state.auto_scroll {
                    state.scroll_offset = 0;
                }
                last_tick = tokio::time::Instant::now();
                terminal.draw(|frame| render::render(&mut state, frame))?;
                tick_rate = if state.pending_assistant.is_some() {
                    STREAMING_TICK_RATE
                } else {
                    IDLE_TICK_RATE
                };
            }
        }
    };

    running.store(false, Ordering::Relaxed);
    if let Some(handle) = state.runtime_handle.take() {
        handle.abort();
    }
    let _ = input_handle.await;
    terminal::restore(&mut terminal)?;
    let _ = std::panic::take_hook();
    result
}
