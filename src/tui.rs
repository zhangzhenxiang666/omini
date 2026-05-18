use self::clipboard::copy_to_clipboard;
use self::selection::{
    selected_text, selection_point_from_mouse, update_text_selection_from_mouse,
};
use self::state::{AgentStatus, TextSelection, UiMessage, UiState};
use crate::config::project::ProjectDir;
use crate::runtime::AgentRuntime;
use crate::types::config::Settings;
use crate::types::events::{RuntimeToUiEvent, ToolPauseKind, ToolPauseResponse, UiToRuntimeEvent};
use crossterm::cursor::Hide;
use crossterm::event::DisableMouseCapture;
use crossterm::event::EnableMouseCapture;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
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
mod input;
mod render;
mod selection;
mod state;
mod widgets;

fn init_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stderr>>> {
    enable_raw_mode()?;
    execute!(stderr(), EnterAlternateScreen)?;
    execute!(stderr(), EnableBracketedPaste)?;
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

    // 从 settings 加载当前配置到 UI 状态
    state.status_bar.model = settings.model.clone();
    state.status_bar.thinking_effort = settings.thinking_effort;
    state.status_bar.active_provider = settings.active_provider.clone();
    state.status_bar.cwd = settings.cwd.clone();
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

    let tick_rate = std::time::Duration::from_millis(50);
    let mut last_tick = tokio::time::Instant::now();

    let result = loop {
        tokio::select! {
            Some(event) = input_rx.recv() => {
                match event {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        // 交互模式: 键盘事件由交互步骤处理
                        if let Some(ref mut step) = state.interaction_step {
                            let consumed =
                                input::handle_interaction_key(step, key.code, &request_tx).await;
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
                        if let Some(active_pause) = state.active_tool_pause().cloned() {
                            let user_input_option_max = match &active_pause.kind {
                                ToolPauseKind::UserInput(preview) => preview
                                    .questions
                                    .get(state.user_input_question_index)
                                    .map(|question| question.options.len())
                                    .unwrap_or(0),
                                ToolPauseKind::Permission(_) => 1,
                            };

                            if state.user_input_note_mode {
                                match key.code {
                                    KeyCode::Tab | KeyCode::Esc => {
                                        state.user_input_note_mode = false;
                                    }
                                    KeyCode::Enter => {
                                        state.mark_current_user_input_answered();
                                        if state.user_input_unanswered_count() == 0 {
                                            input::resolve_active_tool_pause(&mut state, &request_tx)
                                                .await;
                                        } else {
                                            state.move_to_next_unanswered_user_input();
                                        }
                                    }
                                    KeyCode::Backspace => state.delete_note_before(),
                                    KeyCode::Delete => state.delete_note_after(),
                                    KeyCode::Up | KeyCode::Char('k') => state.permission_select_prev(),
                                    KeyCode::Down | KeyCode::Char('j') => {
                                        state.permission_select_next_with_max(user_input_option_max);
                                    }
                                    KeyCode::Char(c) => state.insert_note_char(c),
                                    KeyCode::Left => state.note_cursor_left(),
                                    KeyCode::Right => state.note_cursor_right(),
                                    KeyCode::Home => state.note_cursor_home(),
                                    KeyCode::End => state.note_cursor_end(),
                                    _ => {}
                                }
                                last_tick = tokio::time::Instant::now();
                                terminal.draw(|frame| render::render(&mut state, frame))?;
                                continue;
                            }

                            match key.code {
                                KeyCode::Up | KeyCode::Char('k') => state.permission_select_prev(),
                                KeyCode::Down | KeyCode::Char('j') => {
                                    state.permission_select_next_with_max(user_input_option_max);
                                }
                                KeyCode::Left | KeyCode::Char('h')
                                    if matches!(active_pause.kind, ToolPauseKind::UserInput(_)) =>
                                {
                                    state.user_input_question_prev();
                                }
                                KeyCode::Right | KeyCode::Char('l')
                                    if matches!(active_pause.kind, ToolPauseKind::UserInput(_)) =>
                                {
                                    state.user_input_question_next();
                                }
                                KeyCode::PageUp => {
                                    let page = 1.max(state.permission_drawer_body_area.height as usize / 2);
                                    state.permission_scroll_up(page);
                                }
                                KeyCode::PageDown => {
                                    let page = 1.max(state.permission_drawer_body_area.height as usize / 2);
                                    state.permission_scroll_down(page);
                                }
                                KeyCode::Tab if matches!(active_pause.kind, ToolPauseKind::UserInput(_)) => {
                                    state.user_input_note_mode = true;
                                    state.note_cursor_end();
                                }
                                KeyCode::Char('y') | KeyCode::Char('Y')
                                    if matches!(active_pause.kind, ToolPauseKind::Permission(_)) =>
                                {
                                    state.permission_selected = 0;
                                    input::resolve_active_tool_pause(&mut state, &request_tx).await;
                                }
                                KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N')
                                    if matches!(active_pause.kind, ToolPauseKind::Permission(_)) =>
                                {
                                    state.permission_selected = 1;
                                    input::resolve_active_tool_pause(&mut state, &request_tx).await;
                                }
                                KeyCode::Esc => {
                                    state.permission_selected = 1;
                                    let _ = request_tx
                                        .send(UiToRuntimeEvent::ResolveToolPause {
                                            tool_use_id: active_pause.tool_use_id.clone(),
                                            response: ToolPauseResponse::Cancelled,
                                        })
                                        .await;
                                    state.pending_tool_previews.remove(&active_pause.tool_use_id);
                                    state.reset_permission_drawer();
                                }
                                KeyCode::Enter => {
                                    if matches!(active_pause.kind, ToolPauseKind::UserInput(_)) {
                                        state.mark_current_user_input_answered();
                                        if state.user_input_unanswered_count() == 0 {
                                            input::resolve_active_tool_pause(
                                                &mut state,
                                                &request_tx,
                                            )
                                            .await;
                                        } else {
                                            state.move_to_next_unanswered_user_input();
                                        }
                                    } else {
                                        input::resolve_active_tool_pause(&mut state, &request_tx)
                                            .await;
                                    }
                                }
                                _ => {}
                            }
                            last_tick = tokio::time::Instant::now();
                            terminal.draw(|frame| render::render(&mut state, frame))?;
                            continue;
                        }

                        // 自动补全模式
                        if state.autocomplete.visible {
                            if input::is_newline_key(key.code, key.modifiers) {
                                state.insert_text("\n");
                                state.update_input_autocomplete();
                                last_tick = tokio::time::Instant::now();
                                terminal.draw(|frame| render::render(&mut state, frame))?;
                                continue;
                            }
                            match key.code {
                                KeyCode::Enter => {
                                    if let Some(cmd) = state.autocomplete.selected_command().cloned() {
                                        if cmd.has_args {
                                            // 只补全命令名 + 空格
                                            state.input = format!("/{} ", cmd.name);
                                            state.input_mentions.clear();
                                            state.input_paste_markers.clear();
                                            state.input_scroll_line = 0;
                                            state.cursor_char = state.input.chars().count();
                                        } else {
                                            // 先用完整命令名替换输入，再发送
                                            state.input = format!("/{}", cmd.name);
                                            state.input_mentions.clear();
                                            state.input_paste_markers.clear();
                                            state.input_scroll_line = 0;
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
                                            state.input_mentions.clear();
                                            state.input_paste_markers.clear();
                                            state.input_scroll_line = 0;
                                            state.cursor_char = state.input.chars().count();
                                            state.autocomplete.visible = false;
                                        } else {
                                            state.autocomplete.visible = false;
                                            state.input = format!("/{}", cmd.name);
                                            state.input_mentions.clear();
                                            state.input_paste_markers.clear();
                                            state.input_scroll_line = 0;
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
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Delete => {
                                    state.delete_after();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Char(c) => {
                                    state.insert_char(c);
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Left => {
                                    state.cursor_left();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Right => {
                                    state.cursor_right();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Home => {
                                    state.cursor_home();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::End => {
                                    state.cursor_end();
                                    state.update_input_autocomplete();
                                }
                                _ => {}
                            }
                            last_tick = tokio::time::Instant::now();
                            terminal.draw(|frame| render::render(&mut state, frame))?;
                            continue;
                        }

                        // @ mention 自动补全模式
                        if state.mention_autocomplete.visible {
                            if input::is_newline_key(key.code, key.modifiers) {
                                state.insert_text("\n");
                                state.update_input_autocomplete();
                                last_tick = tokio::time::Instant::now();
                                terminal.draw(|frame| render::render(&mut state, frame))?;
                                continue;
                            }
                            match key.code {
                                KeyCode::Enter => {
                                    state.insert_selected_mention();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Tab | KeyCode::Right => {
                                    state.expand_selected_mention_directory();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Down => state.mention_autocomplete.select_next(),
                                KeyCode::Up => state.mention_autocomplete.select_prev(),
                                KeyCode::Esc => {
                                    state.cancel_mention_autocomplete();
                                }
                                KeyCode::Backspace => {
                                    state.delete_before();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Delete => {
                                    state.delete_after();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Char(c) => {
                                    state.insert_char(c);
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Left => {
                                    state.cursor_left();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::Home => {
                                    state.cursor_home();
                                    state.update_input_autocomplete();
                                }
                                KeyCode::End => {
                                    state.cursor_end();
                                    state.update_input_autocomplete();
                                }
                                _ => {}
                            }
                            last_tick = tokio::time::Instant::now();
                            terminal.draw(|frame| render::render(&mut state, frame))?;
                            continue;
                        }

                        //  普通输入模式
                        let page_amt = 1.max(state.messages_area.height as usize / 2);
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), KeyModifiers::CONTROL)
                            | (KeyCode::Char('\x03'), _) => {
                                if !state.clear_input() {
                                    break Ok(());
                                }
                            }
                            (KeyCode::Up, _) => {
                                state.cursor_up_in_input();
                            }
                            (KeyCode::Down, _) => {
                                state.cursor_down_in_input();
                            }
                            (KeyCode::PageUp, _) => {
                                state.update_scroll_step(tokio::time::Instant::now());
                                state.scroll_up(state.scroll_step.max(page_amt));
                            }
                            (KeyCode::PageDown, _) => {
                                state.update_scroll_step(tokio::time::Instant::now());
                                state.scroll_down(state.scroll_step.max(page_amt));
                            }
                            (code, modifiers) if input::is_intervention_key(code, modifiers) => {
                                input::submit_queued_intervention(&mut state, &request_tx).await;
                            }
                            (code, modifiers) if input::is_newline_key(code, modifiers) => {
                                state.insert_text("\n");
                                state.update_input_autocomplete();
                            }
                            (KeyCode::Enter, _) => {
                                if !state.pending_intervention_inputs.is_empty() {
                                    last_tick = tokio::time::Instant::now();
                                    terminal.draw(|frame| render::render(&mut state, frame))?;
                                    continue;
                                }

                                if let Some(draft) = state.take_input_draft() {
                                    if draft.text.starts_with('/') {
                                        // 命令：不添加消息，不切换工作模式，仅发送到 runtime
                                        let _ = request_tx
                                            .send(UiToRuntimeEvent::SendCommand(draft.text))
                                            .await;
                                    } else if state.is_run_active() {
                                        state.queued_user_inputs.push_back(draft);
                                    } else {
                                        let ui_message = match draft.clone().history_item() {
                                            crate::types::display::HistoryItem::Message(message) => {
                                                UiMessage::Message(message)
                                            }
                                            crate::types::display::HistoryItem::Display(display) => {
                                                UiMessage::Display(display)
                                            }
                                        };
                                        state.messages.push(ui_message);
                                        let _ = request_tx
                                            .send(UiToRuntimeEvent::SendMessage(draft))
                                            .await;
                                        state.scroll_offset = 0;
                                        state.auto_scroll = true;
                                        state.agent_status = AgentStatus::Working;
                                    }
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                state.delete_before();
                                state.update_input_autocomplete();
                            }
                            (KeyCode::Delete, _) => {
                                state.delete_after();
                                state.update_input_autocomplete();
                            }
                            (KeyCode::Char(c), _) => {
                                state.insert_char(c);
                                state.update_input_autocomplete();
                            }
                            (KeyCode::Left, _) => {
                                state.cursor_left();
                                state.update_input_autocomplete();
                            }
                            (KeyCode::Right, _) => {
                                state.cursor_right();
                                state.update_input_autocomplete();
                            }
                            (KeyCode::Home, KeyModifiers::CONTROL) => state.scroll_to_top(),
                            (KeyCode::End, KeyModifiers::CONTROL) => state.scroll_to_bottom(),
                            (KeyCode::Home, _) => {
                                state.cursor_home();
                                state.update_input_autocomplete();
                            }
                            (KeyCode::End, _) => {
                                state.cursor_end();
                                state.update_input_autocomplete();
                            }
                            _ => {}
                        }
                    }
                    Event::Paste(text)
                        if state.interaction_step.is_none() && state.active_tool_pause().is_none() =>
                    {
                        state.insert_paste(text);
                        state.update_input_autocomplete();
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
                    state.open_interaction_request(req);
                }

                // 检查是否需要退出
                if matches!(agent_evt, RuntimeToUiEvent::Shutdown) {
                    break Ok(());
                }

                // SessionChanged → 清空消息区并关闭交互
                if let RuntimeToUiEvent::SessionChanged { session_id, messages, subagents } = agent_evt {
                    state.apply_session_changed(session_id, messages, subagents);
                } else {
                    let should_flush_queue = matches!(agent_evt, RuntimeToUiEvent::RunFinished);
                    state.apply_event(agent_evt);
                    if should_flush_queue {
                        input::flush_queued_user_inputs(&mut state, &request_tx).await;
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
                    if let RuntimeToUiEvent::SessionChanged { session_id, messages, subagents } = evt {
                        state.apply_session_changed(session_id, messages, subagents);
                    } else {
                        let should_flush_queue = matches!(evt, RuntimeToUiEvent::RunFinished);
                        state.apply_event(evt);
                        if should_flush_queue {
                            input::flush_queued_user_inputs(&mut state, &request_tx).await;
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
