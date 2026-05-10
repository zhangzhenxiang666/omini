use self::state::{AgentStatus, InteractionStep, UiState};
use crate::config::project::ProjectDir;
use crate::runtime::AgentRuntime;
use crate::tui::state::ModelSelectionEntry;
use crate::types::config::Settings;
use crate::types::config::ThinkingEffort;
use crate::types::events::InteractionRequest::*;
use crate::types::events::{RuntimeEvent, UiRequest};
use crate::types::message::Message;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::event::MouseButton;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
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

mod render;
mod state;
mod widgets;

fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    enable_raw_mode()?;
    execute!(stderr(), EnterAlternateScreen)?;
    execute!(stderr(), EnableMouseCapture)?;
    Terminal::new(CrosstermBackend::new(stderr()))
}

fn safe_restore_terminal() {
    let _ = disable_raw_mode();
    let _ = execute!(io::stderr(), LeaveAlternateScreen);
    let _ = execute!(io::stderr(), DisableMouseCapture);
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stderr>>) -> io::Result<()> {
    disable_raw_mode()?;
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
    request_tx: &mpsc::Sender<UiRequest>,
) -> bool {
    match step {
        InteractionStep::ModelSelection { entries, selected } => {
            use ModelSelectionEntry as E;
            match key {
                KeyCode::Up | KeyCode::Char('k') => {
                    // 向上跳转，跳过 ProviderHeader
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
                    // 向下跳转，跳过 ProviderHeader
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
                KeyCode::Enter => {
                    if let E::Model {
                        provider_key,
                        model,
                    } = &entries[*selected]
                    {
                        let pkey = provider_key.clone();
                        let model_id = model.id.clone();
                        if model.thinking {
                            // 支持思考 → 进入思考程度选择
                            let saved_entries = entries.clone();
                            let saved_selected = *selected;
                            *step = InteractionStep::ThinkingEffort {
                                provider_key: pkey,
                                model: model.clone(),
                                entries: saved_entries,
                                prev_selected: saved_selected,
                                selected: 2, // 默认 Medium
                            };
                        } else {
                            let _ = request_tx
                                .send(UiRequest::ModelSelected {
                                    provider: pkey,
                                    model: model_id,
                                    thinking_effort: None,
                                })
                                .await;
                        }
                    }
                    true
                }
                KeyCode::Esc => false,
                _ => true,
            }
        }
        InteractionStep::ThinkingEffort {
            provider_key,
            model,
            entries,
            prev_selected,
            selected,
        } => match key {
            KeyCode::Up | KeyCode::Char('k') => {
                *selected = selected.saturating_sub(1);
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                *selected = (*selected + 1).min(3);
                true
            }
            KeyCode::Enter => {
                let te = match *selected {
                    0 => None,
                    1 => Some(ThinkingEffort::Low),
                    2 => Some(ThinkingEffort::Medium),
                    3 => Some(ThinkingEffort::High),
                    _ => None,
                };
                let _ = request_tx
                    .send(UiRequest::ModelSelected {
                        provider: provider_key.clone(),
                        model: model.id.clone(),
                        thinking_effort: te,
                    })
                    .await;
                true
            }
            KeyCode::Esc => {
                // 返回模型选择页
                let saved_entries = std::mem::take(entries);
                let saved_prev = *prev_selected;
                *step = InteractionStep::ModelSelection {
                    entries: saved_entries,
                    selected: saved_prev,
                };
                true
            }
            _ => true,
        },
        InteractionStep::Session {
            sessions,
            all_sessions: _,
            search: _,
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
                        .send(UiRequest::SessionSelected { session_id })
                        .await;
                }
                true
            }
            KeyCode::Char(c) => {
                use crate::tui::state::InteractionStep as IS;
                let fields = match step {
                    IS::Session {
                        sessions,
                        all_sessions,
                        search,
                        selected,
                    } => (sessions, all_sessions, search, selected),
                    _ => unreachable!(),
                };
                let (sessions, all_sessions, search, selected) = fields;
                search.push(c);
                let lower = search.to_lowercase();
                *sessions = all_sessions
                    .iter()
                    .filter(|s| {
                        s.first_message.to_lowercase().contains(&lower)
                            || s.title.to_lowercase().contains(&lower)
                    })
                    .cloned()
                    .collect();
                *selected = 0;
                true
            }
            KeyCode::Backspace => {
                use crate::tui::state::InteractionStep as IS;
                let fields = match step {
                    IS::Session {
                        sessions,
                        all_sessions,
                        search,
                        selected,
                    } => (sessions, all_sessions, search, selected),
                    _ => unreachable!(),
                };
                let (sessions, all_sessions, search, selected) = fields;
                search.pop();
                let lower = search.to_lowercase();
                if lower.is_empty() {
                    *sessions = all_sessions.clone();
                } else {
                    *sessions = all_sessions
                        .iter()
                        .filter(|s| {
                            s.first_message.to_lowercase().contains(&lower)
                                || s.title.to_lowercase().contains(&lower)
                        })
                        .cloned()
                        .collect();
                }
                *selected = 0;
                true
            }
            KeyCode::Esc => false,
            _ => true,
        },
    }
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

    let (agent_tx, mut agent_rx) = mpsc::channel::<RuntimeEvent>(256);
    let (request_tx, request_rx) = mpsc::channel::<UiRequest>(256);

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
                        // ── 交互模式：键盘事件由交互步骤处理 ──
                        if let Some(ref mut step) = state.interaction_step {
                            let consumed = handle_interaction_key(step, key.code, &request_tx).await;
                            if consumed {
                                // 如果 Enter 确认后交互完成，step 被消费
                                // (handle_interaction_key 内部发送了 UiRequest)
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

                        // ── 自动补全模式 ──
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
                                                let _ = request_tx.send(UiRequest::SendMessage(msg)).await;
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
                                                let _ = request_tx.send(UiRequest::SendMessage(msg)).await;
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

                        // ── 普通输入模式 ──
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
                            (KeyCode::Enter, _) => {
                                let msg = std::mem::take(&mut state.input);
                                state.cursor_char = 0;
                                if !msg.is_empty() {
                                    if msg.starts_with('/') {
                                        // 命令：不添加消息，不切换工作模式，仅发送到 runtime
                                        let _ = request_tx.send(UiRequest::SendMessage(msg)).await;
                                    } else {
                                        state.messages.push(Message::from_user_text(msg.clone()));
                                        state.scroll_offset = 0;
                                        state.auto_scroll = true;
                                        state.agent_status = AgentStatus::Working;
                                        let _ = request_tx.send(UiRequest::SendMessage(msg)).await;
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
                        match mouse.kind {
                            MouseEventKind::Down(MouseButton::Left) => {
                                let area = state.messages_area;
                                if mouse.row >= area.top() && mouse.row < area.bottom()
                                    && mouse.column >= area.left() && mouse.column < area.right()
                                {
                                    let visible_line = (mouse.row - area.top()) as usize;
                                    let total = state.total_lines;
                                    let visible_height = area.height as usize;
                                    let max_scroll = total.saturating_sub(visible_height);
                                    let scroll_y = max_scroll.saturating_sub(state.scroll_offset);
                                    let abs_line = scroll_y + visible_line;

                                    let clicked_tool = state.block_ranges.iter()
                                        .find(|(range, _)| range.contains(&abs_line))
                                        .map(|(_, id)| id.clone());
                                    if let Some(tool_id) = clicked_tool
                                        && !state.running_tools.contains(&tool_id) {
                                            state.toggle_tool_expand(&tool_id);
                                        }
                                }
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
                if let RuntimeEvent::InteractionRequest(ref req) = agent_evt {
                    state.interaction_step = match req {
                        ModelSelection { providers, current_provider, current_model } => {
                            let mut entries: Vec<ModelSelectionEntry> = Vec::new();
                            let mut selected = 0;
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
                            Some(InteractionStep::ModelSelection { entries, selected })
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
                if matches!(agent_evt, RuntimeEvent::Shutdown) {
                    break Ok(());
                }

               // SessionChanged → 清空消息区并关闭交互
                if let RuntimeEvent::SessionChanged { session_id, messages, .. } = agent_evt {
                    state.current_session_id = Some(session_id);
                    state.messages = messages;
                    state.pending_assistant = None;
                    state.interaction_step = None;
                    state.interaction_request = None;
                } else {
                    state.apply_event(agent_evt);
                }
            }

            _ = tokio::time::sleep_until(last_tick + tick_rate) => {
                // tick 分支：检查待处理事件并重绘
                let mut shutdown = false;
                while let Ok(evt) = agent_rx.try_recv() {
                    if matches!(evt, RuntimeEvent::Shutdown) {
                        shutdown = true;
                        break;
                    }
                    if let RuntimeEvent::SessionChanged { session_id, messages, .. } = evt {
                        state.current_session_id = Some(session_id);
                        state.messages = messages;
                        state.pending_assistant = None;
                        state.interaction_step = None;
                        state.interaction_request = None;
                    } else {
                        state.apply_event(evt);
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
