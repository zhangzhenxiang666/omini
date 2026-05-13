use self::clipboard::copy_to_clipboard;
use self::selection::{
    selected_text, selection_point_from_mouse, update_text_selection_from_mouse,
};
use self::state::{AgentStatus, InteractionStep, TextSelection, UiState};
use crate::config::project::ProjectDir;
use crate::runtime::AgentRuntime;
use crate::tui::state::ModelSelectionEntry;
use crate::types::config::Settings;
use crate::types::config::ThinkingEffort;
use crate::types::events::InteractionRequest::*;
use crate::types::events::{RuntimeToUiEvent, ToolPauseKind, ToolPauseResponse, UiToRuntimeEvent};
use crate::types::message::Message;
use crossterm::cursor::Hide;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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

mod clipboard;
mod render;
mod selection;
mod state;
mod widgets;

fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    enable_raw_mode()?;
    execute!(stderr(), EnterAlternateScreen)?;
    execute!(
        stderr(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
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
    let _ = execute!(io::stderr(), LeaveAlternateScreen);
    let _ = execute!(io::stderr(), DisableMouseCapture);
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), PopKeyboardEnhancementFlags)?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    execute!(terminal.backend_mut(), DisableMouseCapture)?;
    terminal.show_cursor()?;
    Ok(())
}

/// 处理交互模式的键盘事件。
/// 返回 `true` = 事件已消费；`false` = 调用方应退出交互模式。
async fn handle_interaction_key(
    step: &mut InteractionStep,
    key: KeyCode,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) -> bool {
    match step {
        InteractionStep::ModelSelection {
            entries,
            selected,
            thinking_idx,
            ..
        } => {
            use ModelSelectionEntry as E;
            match key {
                KeyCode::Up | KeyCode::Char('k') => {
                    // Jump up, skip ProviderHeader
                    let mut new = selected.saturating_sub(1);
                    while new > 0 && matches!(&entries[new], E::ProviderHeader { .. }) {
                        new = new.saturating_sub(1);
                    }
                    if !matches!(&entries[new], E::ProviderHeader { .. }) {
                        *selected = new;
                    }
                    true
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    // Jump down, skip ProviderHeader
                    let max = entries.len().saturating_sub(1);
                    let mut new = (*selected + 1).min(max);
                    while new < max && matches!(&entries[new], E::ProviderHeader { .. }) {
                        new = (new + 1).min(max);
                    }
                    if !matches!(&entries[new], E::ProviderHeader { .. }) {
                        *selected = new;
                    }
                    true
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    // Adjust thinking effort (only if model supports it)
                    if let E::Model { model, .. } = &entries[*selected]
                        && model.thinking
                    {
                        *thinking_idx = thinking_idx.saturating_sub(1);
                    }
                    true
                }
                KeyCode::Right | KeyCode::Char('l') => {
                    if let E::Model { model, .. } = &entries[*selected]
                        && model.thinking
                    {
                        *thinking_idx = (*thinking_idx + 1).min(3);
                    }
                    true
                }
                KeyCode::Enter => {
                    if let E::Model {
                        provider_key,
                        model,
                    } = &entries[*selected]
                    {
                        let pkey = provider_key.clone();
                        let model_id = model.id.clone();
                        let te = match *thinking_idx {
                            1 => Some(crate::types::config::ThinkingEffort::Low),
                            2 => Some(crate::types::config::ThinkingEffort::Medium),
                            3 => Some(crate::types::config::ThinkingEffort::High),
                            _ => None,
                        };
                        let _ = request_tx
                            .send(UiToRuntimeEvent::ModelSelected {
                                provider: pkey,
                                model: model_id,
                                thinking_effort: te,
                            })
                            .await;
                    }
                    true
                }
                KeyCode::Esc => false,
                _ => true,
            }
        }
        InteractionStep::Session {
            sessions,
            all_sessions,
            search,
            selected,
        } => match key {
            KeyCode::Up => {
                *selected = selected.saturating_sub(1);
                true
            }
            KeyCode::Down => {
                *selected = (*selected + 1).min(sessions.len().saturating_sub(1));
                true
            }
            KeyCode::Enter => {
                if !sessions.is_empty() {
                    let session_id = sessions[*selected].id.clone();
                    let _ = request_tx
                        .send(UiToRuntimeEvent::SessionSelected { session_id })
                        .await;
                }
                true
            }
            KeyCode::Char(c) => {
                search.push(c);
                let lower = search.to_lowercase();
                let mut filtered: Vec<_> = all_sessions
                    .iter()
                    .filter(|s| s.title.to_lowercase().contains(&lower))
                    .cloned()
                    .collect();
                std::mem::swap(sessions, &mut filtered);
                *selected = 0;
                true
            }
            KeyCode::Backspace => {
                search.pop();
                let lower = search.to_lowercase();
                if lower.is_empty() {
                    *sessions = all_sessions.clone();
                } else {
                    let mut filtered: Vec<_> = all_sessions
                        .iter()
                        .filter(|s| s.title.to_lowercase().contains(&lower))
                        .cloned()
                        .collect();
                    std::mem::swap(sessions, &mut filtered);
                }
                *selected = 0;
                true
            }
            KeyCode::Esc => false,
            _ => true,
        },
    }
}

async fn resolve_active_tool_pause(
    state: &mut UiState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    let Some(req) = state.active_tool_pause().cloned() else {
        return;
    };

    let response = match req.kind {
        ToolPauseKind::Permission(_) => ToolPauseResponse::Permission {
            approved: state.permission_selected == 0,
        },
        ToolPauseKind::UserInput(_) => {
            if state.permission_selected == 0 {
                return;
            }
            ToolPauseResponse::Cancelled
        }
    };

    let _ = request_tx
        .send(UiToRuntimeEvent::ResolveToolPause {
            tool_use_id: req.tool_use_id.clone(),
            response,
        })
        .await;
    state.pending_tool_previews.remove(&req.tool_use_id);
    if state.pending_tool_previews.is_empty() {
        state.reset_permission_drawer();
    }
}

async fn flush_queued_user_inputs(
    state: &mut UiState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    let Some(msg) = state.take_queued_user_message() else {
        return;
    };

    state.messages.push(msg.clone());
    state.scroll_offset = 0;
    state.auto_scroll = true;
    state.agent_status = AgentStatus::Working;
    let _ = request_tx.send(UiToRuntimeEvent::SendMessage(msg)).await;
}

async fn submit_queued_intervention(
    state: &mut UiState,
    request_tx: &mpsc::Sender<UiToRuntimeEvent>,
) {
    if state.is_run_active()
        && state.pending_intervention_inputs.is_empty()
        && let Some(msg) = state.take_queued_user_message_for_intervention()
    {
        let _ = request_tx
            .send(UiToRuntimeEvent::InterveneMessage(msg))
            .await;
    }
}

fn is_intervention_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    if !modifiers.contains(KeyModifiers::ALT) {
        return false;
    }

    matches!(code, KeyCode::Enter)
}

pub async fn run_ui(settings: Settings, project: ProjectDir) -> io::Result<()> {
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        safe_restore_terminal();
        prev_hook(panic);
    }));

    let mut terminal = init_terminal()?;
    let mut state = UiState::new();

    // 从 settings 加载当前配置到 UI 状态
    state.status_bar.model = settings.model.clone();
    state.status_bar.thinking_effort = settings.thinking_effort;
    state.status_bar.active_provider = settings.active_provider.clone();
    state.status_bar.cwd = settings.cwd.clone();

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

    let tick_rate = std::time::Duration::from_millis(50);
    let mut last_tick = tokio::time::Instant::now();

    let result = loop {
        tokio::select! {
            Some(event) = input_rx.recv() => {
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        // 交互模式: 键盘事件由交互步骤处理
                        if let Some(ref mut step) = state.interaction_step {
                            let consumed = handle_interaction_key(step, key.code, &request_tx).await;
                            if consumed {
                                // 如果 Enter 确认后交互完成，step 被消费
                                // (handle_interaction_key 内部发送了 UiToRuntimeEvent)
                                // 但我们需要检测 step 是否因为 Enter 而提交完成
                            } else {
                                // Esc → 退出交互
                                state.interaction_step = None;
                                state.interaction_request = None;
                            }
                            last_tick = tokio::time::Instant::now();
                            terminal.draw(|frame| render::render(&mut state, frame))?;
                            continue;
                        }

                        if key.code == KeyCode::Esc
                            && matches!(
                                state.agent_status,
                                AgentStatus::Working | AgentStatus::Thinking
                            )
                        {
                            let _ = request_tx.send(UiToRuntimeEvent::CancelRun).await;
                            last_tick = tokio::time::Instant::now();
                            terminal.draw(|frame| render::render(&mut state, frame))?;
                            continue;
                        }

                        // 工具暂停, 权限抽屉模式
                        if state.active_tool_pause().is_some() {
                            match key.code {
                                KeyCode::Up | KeyCode::Char('k') => {
                                    state.permission_select_prev();
                                }
                                KeyCode::Down | KeyCode::Char('j') => {
                                    state.permission_select_next();
                                }
                                KeyCode::PageUp => {
                                    let page = 1.max(state.permission_drawer_body_area.height as usize / 2);
                                    state.permission_scroll_up(page);
                                }
                                KeyCode::PageDown => {
                                    let page = 1.max(state.permission_drawer_body_area.height as usize / 2);
                                    state.permission_scroll_down(page);
                                }
                                KeyCode::Char('y') | KeyCode::Char('Y') => {
                                    state.permission_selected = 0;
                                    resolve_active_tool_pause(&mut state, &request_tx).await;
                                }
                                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                                    state.permission_selected = 1;
                                    resolve_active_tool_pause(&mut state, &request_tx).await;
                                }
                                KeyCode::Enter => {
                                    resolve_active_tool_pause(&mut state, &request_tx).await;
                                }
                                _ => {}
                            }
                            last_tick = tokio::time::Instant::now();
                            terminal.draw(|frame| render::render(&mut state, frame))?;
                            continue;
                        }

                        // 自动补全模式
                        if state.autocomplete.visible {
                            match key.code {
                                KeyCode::Enter => {
                                    if let Some(cmd) = state.autocomplete.selected_command().cloned() {
                                        if cmd.has_args {
                                            // 只补全命令名 + 空格
                                            state.input = format!("/{} ", cmd.name);
                                            state.cursor_char = state.input.chars().count();
                                        } else {
                                            // 先用完整命令名替换输入，再发送
                                            state.input = format!("/{}", cmd.name);
                                            let msg = std::mem::take(&mut state.input);
                                            state.cursor_char = 0;
                                            state.autocomplete.visible = false;
                                            if !msg.is_empty() {
                                                // 命令：不添加消息，不切换工作模式，仅发送到 runtime
                                                let _ = request_tx.send(UiToRuntimeEvent::SendCommand(msg)).await;
                                            }
                                        }
                                    }
                                    state.autocomplete.visible = false;
                                }
                                KeyCode::Tab => {
                                    // Tab → 无参命令直接执行，有参命令补全 + 空格
                                    if let Some(cmd) = state.autocomplete.selected_command().cloned() {
                                        if cmd.has_args {
                                            state.input = format!("/{} ", cmd.name);
                                            state.cursor_char = state.input.chars().count();
                                            state.autocomplete.visible = false;
                                        } else {
                                            state.autocomplete.visible = false;
                                            state.input = format!("/{}", cmd.name);
                                            let msg = std::mem::take(&mut state.input);
                                            state.cursor_char = 0;
                                            if !msg.is_empty() {
                                                // 命令：不添加消息，不切换工作模式，仅发送到 runtime
                                                let _ = request_tx.send(UiToRuntimeEvent::SendCommand(msg)).await;
                                            }
                                        }
                                    }
                                }
                                KeyCode::Down => {
                                    state.autocomplete.select_next();
                                }
                                KeyCode::Up => {
                                    state.autocomplete.select_prev();
                                }
                                KeyCode::Esc => {
                                    state.autocomplete.visible = false;
                                }
                                KeyCode::Backspace => {
                                    state.delete_before();
                                    state.autocomplete.update(&state.input);
                                }
                                KeyCode::Delete => {
                                    state.delete_after();
                                    state.autocomplete.update(&state.input);
                                }
                                KeyCode::Char(c) => {
                                    state.insert_char(c);
                                    state.autocomplete.update(&state.input);
                                }
                                KeyCode::Left => state.cursor_left(),
                                KeyCode::Right => state.cursor_right(),
                                KeyCode::Home => state.cursor_home(),
                                KeyCode::End => state.cursor_end(),
                                _ => {}
                            }
                            last_tick = tokio::time::Instant::now();
                            terminal.draw(|frame| render::render(&mut state, frame))?;
                            continue;
                        }

                        //  普通输入模式
                        let page_amt = 1.max(state.messages_area.height as usize / 2);
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => break Ok(()),
                            (KeyCode::Char('\x03'), _) => break Ok(()),
                            (KeyCode::Up, _) => state.scroll_up(1),
                            (KeyCode::Down, _) => state.scroll_down(1),
                            (KeyCode::PageUp, _) => {
                                state.update_scroll_step(tokio::time::Instant::now());
                                state.scroll_up(state.scroll_step.max(page_amt));
                            }
                            (KeyCode::PageDown, _) => {
                                state.update_scroll_step(tokio::time::Instant::now());
                                state.scroll_down(state.scroll_step.max(page_amt));
                            }
                            (code, modifiers) if is_intervention_key(code, modifiers) => {
                                submit_queued_intervention(&mut state, &request_tx).await;
                            }
                            (KeyCode::Enter, _) => {
                                if !state.pending_intervention_inputs.is_empty() {
                                    last_tick = tokio::time::Instant::now();
                                    terminal.draw(|frame| render::render(&mut state, frame))?;
                                    continue;
                                }

                                let msg = std::mem::take(&mut state.input);
                                state.cursor_char = 0;
                                if !msg.is_empty() {
                                    if msg.starts_with('/') {
                                        // 命令：不添加消息，不切换工作模式，仅发送到 runtime
                                        let _ = request_tx.send(UiToRuntimeEvent::SendCommand(msg)).await;
                                    } else if state.is_run_active() {
                                        state.queued_user_inputs.push_back(msg);
                                    } else {
                                        let msg = Message::from_user_text(msg);
                                        state.messages.push(msg.clone());
                                        state.scroll_offset = 0;
                                        state.auto_scroll = true;
                                        state.agent_status = AgentStatus::Working;
                                        let _ = request_tx.send(UiToRuntimeEvent::SendMessage(msg)).await;
                                    }
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                state.delete_before();
                                state.autocomplete.update(&state.input);
                            }
                            (KeyCode::Delete, _) => {
                                state.delete_after();
                                state.autocomplete.update(&state.input);
                            }
                            (KeyCode::Char(c), _) => {
                                state.insert_char(c);
                                state.autocomplete.update(&state.input);
                            }
                            (KeyCode::Left, _) => state.cursor_left(),
                            (KeyCode::Right, _) => state.cursor_right(),
                            (KeyCode::Home, KeyModifiers::CONTROL) => state.scroll_to_top(),
                            (KeyCode::End, KeyModifiers::CONTROL) => state.scroll_to_bottom(),
                            (KeyCode::Home, _) => state.cursor_home(),
                            (KeyCode::End, _) => state.cursor_end(),
                            _ => {}
                        }
                    }
                    Event::Resize(_, _) => {}
                    Event::Mouse(mouse) => {
                        if state.is_selecting_text {
                            match mouse.kind {
                                MouseEventKind::Drag(MouseButton::Left) => {
                                    update_text_selection_from_mouse(
                                        &mut state,
                                        mouse.row,
                                        mouse.column,
                                    );
                                }
                                MouseEventKind::Up(MouseButton::Left) => {
                                    update_text_selection_from_mouse(
                                        &mut state,
                                        mouse.row,
                                        mouse.column,
                                    );
                                    state.is_selecting_text = false;
                                    if let Some(text) = selected_text(&state) {
                                        copy_to_clipboard(&text);
                                    }
                                    state.text_selection = None;
                                }
                                _ => {}
                            }
                            last_tick = tokio::time::Instant::now();
                            terminal.draw(|frame| render::render(&mut state, frame))?;
                            continue;
                        }

                        if state.active_tool_pause().is_some() {
                            let drawer = state.permission_drawer_area;
                            let body = state.permission_drawer_body_area;
                            let in_drawer = mouse.row >= drawer.top() && mouse.row < drawer.bottom()
                                && mouse.column >= drawer.left() && mouse.column < drawer.right();
                            let in_body = mouse.row >= body.top() && mouse.row < body.bottom()
                                && mouse.column >= body.left() && mouse.column < body.right();

                            match mouse.kind {
                                MouseEventKind::Down(MouseButton::Left) if in_drawer => {
                                    if mouse.row == drawer.bottom().saturating_sub(2) {
                                        state.permission_selected = 0;
                                    } else if mouse.row == drawer.bottom().saturating_sub(1) {
                                        state.permission_selected = 1;
                                    }
                                }
                                MouseEventKind::ScrollUp if in_body => {
                                    state.update_scroll_step(tokio::time::Instant::now());
                                    state.permission_scroll_up(state.scroll_step);
                                }
                                MouseEventKind::ScrollDown if in_body => {
                                    state.update_scroll_step(tokio::time::Instant::now());
                                    state.permission_scroll_down(state.scroll_step);
                                }
                                _ => {}
                            }
                            last_tick = tokio::time::Instant::now();
                            terminal.draw(|frame| render::render(&mut state, frame))?;
                            continue;
                        }

                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                if let Some(point) =
                                    selection_point_from_mouse(&state, mouse.row, mouse.column)
                                {
                                    state.text_selection = Some(TextSelection {
                                        start: point,
                                        end: point,
                                    });
                                    state.is_selecting_text = true;
                                }
                            }
                            MouseEventKind::Drag(MouseButton::Left) => {
                                update_text_selection_from_mouse(
                                    &mut state,
                                    mouse.row,
                                    mouse.column,
                                );
                            }
                            MouseEventKind::Up(MouseButton::Left) => {
                                update_text_selection_from_mouse(
                                    &mut state,
                                    mouse.row,
                                    mouse.column,
                                );
                                state.is_selecting_text = false;
                                if let Some(text) = selected_text(&state) {
                                    copy_to_clipboard(&text);
                                }
                                state.text_selection = None;
                            }
                            MouseEventKind::ScrollUp => {
                                state.update_scroll_step(tokio::time::Instant::now());
                                state.scroll_up(state.scroll_step);
                            }
                            MouseEventKind::ScrollDown => {
                                state.update_scroll_step(tokio::time::Instant::now());
                                state.scroll_down(state.scroll_step);
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
                last_tick = tokio::time::Instant::now();
                terminal.draw(|frame| render::render(&mut state, frame))?;
            }
            Some(agent_evt) = agent_rx.recv() => {
                if let RuntimeToUiEvent::InteractionRequest(ref req) = agent_evt {
                    state.interaction_step = match req {
                        ModelSelection { providers, current_provider, current_model } => {
                            let mut entries: Vec<ModelSelectionEntry> = Vec::new();
                            let mut selected = 0;
                            let default_thinking = match state.status_bar.thinking_effort {
                                Some(ThinkingEffort::Low) => 1,
                                Some(ThinkingEffort::Medium) => 2,
                                Some(ThinkingEffort::High) => 3,
                                Some(ThinkingEffort::None) | None => 0,
                            };
                            // 按 provider key 排序
                            let mut sorted: Vec<_> = providers.clone().into_iter().collect();
                            sorted.sort_by(|a, b| a.0.cmp(&b.0));
                            for (pkey, profile) in &sorted {
                                entries.push(ModelSelectionEntry::ProviderHeader {
                                    name: profile.name.clone(),
                                });
                                for model in &profile.models {
                                    // 如果是当前使用的 provider + model，标记为选中
                                    if *pkey == *current_provider && model.id == *current_model {
                                        selected = entries.len(); // 即将 push 的 model 的索引
                                    }
                                    entries.push(ModelSelectionEntry::Model {
                                        provider_key: pkey.clone(),
                                        model: model.clone(),
                                    });
                                }
                            }
                            // 如果没有任何匹配（或列表为空），selected 保持 0（第一个 Model 条目）
                            Some(InteractionStep::ModelSelection {
                                entries,
                                selected,
                                thinking_idx: default_thinking,
                                active_provider: current_provider.clone(),
                                active_model: current_model.clone(),
                            })
                        }
                        SessionSelection { sessions } => {
                            let mut sorted = sessions.clone();
                            sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                            let all_cloned = sorted.clone();
                            Some(InteractionStep::Session {
                                sessions: sorted,
                                all_sessions: all_cloned,
                                search: String::new(),
                                selected: 0,
                            })
                        }
                    };
                }

                // 检查是否需要退出
                if matches!(agent_evt, RuntimeToUiEvent::Shutdown) {
                    break Ok(());
                }

                // SessionChanged → 清空消息区并关闭交互
                if let RuntimeToUiEvent::SessionChanged { session_id, messages } = agent_evt {
                    state.current_session_id = session_id;
                    state.messages = messages;
                    state.pending_assistant = None;
                    state.queued_user_inputs.clear();
                    state.interaction_step = None;
                    state.interaction_request = None;
                    state.scroll_to_bottom();
                } else {
                    let should_flush_queue = matches!(agent_evt, RuntimeToUiEvent::RunFinished);
                    state.apply_event(agent_evt);
                    if should_flush_queue {
                        flush_queued_user_inputs(&mut state, &request_tx).await;
                    }
                }
            }

            _ = tokio::time::sleep_until(last_tick + tick_rate) => {
                // tick 分支：检查待处理事件并重绘
                let mut shutdown = false;
                while let Ok(evt) = agent_rx.try_recv() {
                    if matches!(evt, RuntimeToUiEvent::Shutdown) {
                        shutdown = true;
                        break;
                    }
                    if let RuntimeToUiEvent::SessionChanged { session_id, messages } = evt {
                        state.current_session_id = session_id;
                        state.messages = messages;
                        state.pending_assistant = None;
                        state.queued_user_inputs.clear();
                        state.interaction_step = None;
                        state.interaction_request = None;
                        state.scroll_to_bottom();
                    } else {
                        let should_flush_queue = matches!(evt, RuntimeToUiEvent::RunFinished);
                        state.apply_event(evt);
                        if should_flush_queue {
                            flush_queued_user_inputs(&mut state, &request_tx).await;
                        }
                    }
                }
                if shutdown {
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
