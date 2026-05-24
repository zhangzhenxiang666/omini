use super::render;
use super::state::UiState;
use super::update;
use crate::config::project::ProjectDir;
use crate::runtime::AgentRuntime;
use crate::types::config::Settings;
use crate::types::events::{ActiveProfile, RuntimeToUiEvent, UiToRuntimeEvent};
use crossterm::cursor::Hide;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use std::io::{self, stderr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    enable_raw_mode()?;
    execute!(stderr(), EnterAlternateScreen)?;
    execute!(stderr(), EnableBracketedPaste)?;
    execute!(
        stderr(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES,
        )
    )?;
    execute!(stderr(), EnableMouseCapture)?;
    execute!(stderr(), Hide)?;
    Terminal::new(CrosstermBackend::new(stderr()))
}

fn safe_restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), PopKeyboardEnhancementFlags);
    let _ = execute!(io::stderr(), DisableBracketedPaste);
    let _ = execute!(io::stderr(), LeaveAlternateScreen);
    let _ = execute!(io::stderr(), DisableMouseCapture);
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    execute!(terminal.backend_mut(), DisableBracketedPaste)?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

pub async fn run_ui(settings: Settings, project: ProjectDir) -> io::Result<()> {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        safe_restore_terminal();
        prev_hook(panic);
    }));

    let mut terminal = init_terminal()?;
    let mut state = UiState::new();

    state.status_bar.model = settings.model.clone();
    state.status_bar.thinking_effort = settings.thinking_effort;
    state.status_bar.active_provider = settings.active_provider.clone();
    state.status_bar.cwd = settings.cwd.clone();
    state.status_bar.active_profile = ActiveProfile::Main;
    state.show_thinking_blocks = project
        .load_state()
        .map(|state| state.show_thinking_blocks)
        .unwrap_or(true);
    state.status_bar.context_window =
        settings
            .providers
            .get(&settings.active_provider)
            .and_then(|provider| {
                provider
                    .models
                    .iter()
                    .find(|model| model.id == settings.model)
                    .map(|model| model.limit)
            });
    state.set_mention_context(settings.cwd.clone(), Vec::new());

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
    let (request_tx, request_rx) = mpsc::channel::<UiToRuntimeEvent>(256);

    let runtime = AgentRuntime::new(agent_tx.clone(), request_rx, settings, project);
    state.runtime_handle = Some(runtime.run());

    terminal.draw(|frame| render::render(&mut state, frame))?;

    let tick_rate = Duration::from_millis(50);
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
            }
        }
    };

    running.store(false, Ordering::Relaxed);
    if let Some(handle) = state.runtime_handle.take() {
        handle.abort();
    }
    let _ = input_handle.await;
    restore_terminal(&mut terminal)?;
    let _ = std::panic::take_hook();
    result
}
