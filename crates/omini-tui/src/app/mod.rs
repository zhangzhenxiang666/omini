// 样式图鉴（tui-debug）仅供开发调试，release 构建整体不参与编译。
#[cfg(debug_assertions)]
pub mod debug;
pub mod effect;
pub mod event;
pub mod events;
pub mod focus;
pub mod mouse;
pub mod state;
pub mod update;
use crate::app::state::AppState;
use crate::client;
use crate::ui::view as render;
pub mod setup;
use crate::app::event::{ActiveProfile, RuntimeToUiEvent};
use crate::platform::terminal;
use crossterm::event::{self as terminal_events, Event};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

const STREAMING_TICK_RATE: Duration = Duration::from_millis(100);
const IDLE_TICK_RATE: Duration = Duration::from_millis(50);

pub async fn run_ui_async(connection: client::StartupConnection) -> io::Result<()> {
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

    let terminal_guard = terminal::RestoreGuard::new();
    let mut terminal = terminal::init()?;
    let mut state = AppState::new();
    let open = &connection.open;
    let cwd = std::path::PathBuf::from(&open.project.path);

    state.project.status_bar.model = open.model.clone();
    state.project.status_bar.thinking_effort = open
        .thinking_effort
        .map(client::thinking_effort_from_protocol);
    state.project.status_bar.active_provider = open.active_provider.clone();
    state.project.status_bar.cwd = cwd.clone();
    state.project.status_bar.git_branch = open.git_branch.clone();
    state.project.status_bar.active_profile = ActiveProfile::Main;
    state.start.startup_mcp_server_count = open.mcp_server_count;
    state.start.startup_has_project_instructions = open.has_project_instructions;
    state.project.status_bar.context_window = open.context_window;
    state.start.startup_recent_threads = open
        .threads
        .clone()
        .into_iter()
        .map(client::thread_summary_from_protocol)
        .filter(|thread| !thread.title.trim().is_empty())
        .take(6)
        .collect();
    state.composer.autocomplete.all_commands =
        crate::features::commands::commands_with_runtime_skills(
            open.skills
                .clone()
                .into_iter()
                .map(client::skill_command_summary)
                .collect(),
        );
    state.composer.set_mention_context(
        cwd,
        crate::app::state::agent_summaries_to_mention_candidates(
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
            if terminal_events::poll(Duration::from_millis(50)).unwrap_or(false) {
                match terminal_events::read() {
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

    let (local_tx, mut local_rx) = mpsc::unbounded_channel();
    let (agent_tx, mut agent_rx) = mpsc::channel::<RuntimeToUiEvent>(256);
    let (request_tx, request_rx) = mpsc::channel::<client::ClientRequest>(256);

    let runtime_handle = client::spawn_project_client(connection, agent_tx.clone(), request_rx);

    terminal.draw(|frame| draw(&mut state, frame))?;

    let mut tick_rate = IDLE_TICK_RATE;
    let mut last_tick = tokio::time::Instant::now();

    let result = async {
        loop {
            tokio::select! {
                Some(event) = local_rx.recv() => {
                    state.composer.apply_local(event);
                    terminal.draw(|frame| draw(&mut state, frame))?;
                }
                Some(event) = input_rx.recv() => {
                    let mut effects = effect::Effects::default();
                    let outcome = update::handle_input_event(&mut state, event, &mut effects);
                    execute_effects(&mut state, effects, &request_tx, &local_tx).await?;
                    if outcome.exit {
                        break Ok(());
                    }
                    if outcome.redraw {
                        last_tick = tokio::time::Instant::now();
                        terminal.draw(|frame| draw(&mut state, frame))?;
                    }
                }
                Some(agent_evt) = agent_rx.recv() => {
                    let mut effects = effect::Effects::default();
                    let outcome = update::handle_runtime_event(&mut state, agent_evt, &mut effects);
                    execute_effects(&mut state, effects, &request_tx, &local_tx).await?;
                    if outcome.exit {
                        break Ok(());
                    }
                }
                _ = tokio::time::sleep_until(last_tick + tick_rate) => {
                    let mut shutdown = false;
                    while let Ok(event) = agent_rx.try_recv() {
                        let mut effects = effect::Effects::default();
                        let outcome = update::handle_runtime_event(&mut state, event, &mut effects);
                        execute_effects(&mut state, effects, &request_tx, &local_tx).await?;
                        shutdown |= outcome.exit;
                    }
                    if shutdown { break Ok(()); }
                    if state.sessions.views["main"].auto_scroll {
                        state.sessions.views["main"].scroll_offset = 0;
                    }
                    last_tick = tokio::time::Instant::now();
                    terminal.draw(|frame| draw(&mut state, frame))?;
                    tick_rate = if state.sessions.views["main"].pending_assistant.is_some() {
                        STREAMING_TICK_RATE
                    } else {
                        IDLE_TICK_RATE
                    };
                }
            }
        }
    }
    .await;

    running.store(false, Ordering::Relaxed);
    runtime_handle.abort();
    let _ = input_handle.await;
    terminal::restore(&mut terminal)?;
    drop(terminal_guard);
    result
}

/// 在渲染前准备编辑器宽度，渲染后只应用布局反馈。
pub fn draw(state: &mut AppState, frame: &mut ratatui::Frame) {
    state
        .composer
        .set_input_wrap_width(frame.area().width as usize);
    if let Some(crate::app::state::InteractionStep::Agents(manager)) =
        &mut state.dialogs.interaction_step
    {
        manager.set_draft_wrap_width(crate::features::agents::view::input_box_inner_width(
            frame.area().width.saturating_sub(4) as usize,
        ));
    }
    let output = render::render(state, frame);
    state.apply_frame(output);
}

async fn execute_effects(
    state: &mut AppState,
    effects: effect::Effects,
    request_tx: &mpsc::Sender<client::ClientRequest>,
    local_tx: &mpsc::UnboundedSender<crate::platform::files::LocalEvent>,
) -> io::Result<()> {
    for request in effects.local {
        match request {
            // 图片粘贴必须先完成路径校验和替换，下一次 Enter 才能提交附件语义。
            image @ crate::platform::files::LocalRequest::Image { .. } => {
                let event =
                    tokio::task::spawn_blocking(move || crate::platform::files::execute(image))
                        .await
                        .map_err(io::Error::other)?;
                state.composer.apply_local(event);
            }
            directory => {
                let tx = local_tx.clone();
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(crate::platform::files::execute(directory));
                });
            }
        }
    }
    for text in effects.clipboard {
        crate::platform::clipboard::copy_to_clipboard(&text);
    }
    for request in effects.requests {
        request_tx
            .send(request)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "服务端请求通道已关闭"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pasted_image_is_ready_before_next_input_event() {
        let path = std::env::temp_dir().join(format!("omini-tui-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"image fixture").unwrap();
        let mut state = AppState::new();
        let mut effects = effect::Effects::default();
        let paste = Event::Paste(path.to_string_lossy().into_owned());
        update::handle_input_event(&mut state, paste, &mut effects);
        let (request_tx, _) = mpsc::channel(1);
        let (local_tx, mut local_rx) = mpsc::unbounded_channel();
        execute_effects(&mut state, effects, &request_tx, &local_tx)
            .await
            .unwrap();
        assert_eq!(state.composer.input_images.len(), 1);
        assert!(local_rx.try_recv().is_err());
        std::fs::remove_file(path).unwrap();
    }
}
