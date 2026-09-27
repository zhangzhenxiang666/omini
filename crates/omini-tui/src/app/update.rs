use crate::app::effect::Effects;
use crate::app::event::RuntimeToUiEvent;
use crate::app::state::{AgentStatus, AppState};
use crate::client::ClientRequest;
use crate::features::agents::actions as input;
use crate::features::composer::update::*;
use crate::features::help::update::*;
use crate::features::permissions::update::*;
use crate::features::plan::update::*;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use omini_domain::task::TaskStatus;

use crate::app::mouse::handle_mouse_event;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UpdateOutcome {
    pub redraw: bool,
    pub exit: bool,
}

impl UpdateOutcome {
    fn redraw() -> Self {
        Self {
            redraw: true,
            exit: false,
        }
    }

    fn exit() -> Self {
        Self {
            redraw: false,
            exit: true,
        }
    }
}

pub fn handle_input_event(
    state: &mut AppState,
    event: Event,
    request_tx: &mut Effects,
) -> UpdateOutcome {
    let outcome = match event {
        Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
            if handle_key_event(state, key.code, key.modifiers, request_tx) {
                UpdateOutcome::redraw()
            } else {
                UpdateOutcome::exit()
            }
        }
        Event::Paste(text) if state.note_mode() => {
            // note 模式期间放行终端 bracketed paste(Shift+Insert 等),
            // 逐字符塞进 note。修复 Bug 2 的一部分:此前主输入框分支的
            // `active_tool_pause().is_none()` 守卫把 tool pause 期间的
            // `Event::Paste` 整体吞掉,note 模式无处可粘。
            for c in text.chars() {
                state.insert_note_char(c);
            }
            UpdateOutcome::redraw()
        }
        Event::Paste(text)
            if crate::app::focus::current(state) == crate::app::focus::Focus::Composer =>
        {
            state.composer.insert_paste(text);
            state.composer.update_input_autocomplete();
            UpdateOutcome::redraw()
        }
        Event::Resize(_, _) => UpdateOutcome::redraw(),
        Event::Mouse(mouse) => {
            handle_mouse_event(state, mouse.kind, mouse.row, mouse.column, request_tx);
            UpdateOutcome::redraw()
        }
        _ => UpdateOutcome::default(),
    };
    request_tx.local.extend(state.composer.take_requests());
    outcome
}

pub fn handle_runtime_event(
    state: &mut AppState,
    event: RuntimeToUiEvent,
    request_tx: &mut Effects,
) -> UpdateOutcome {
    if let RuntimeToUiEvent::InteractionRequest(ref req) = event {
        state.open_interaction_request(req);
    }

    if matches!(event, RuntimeToUiEvent::Shutdown) {
        return UpdateOutcome::exit();
    }

    if let RuntimeToUiEvent::ThreadSnapshot {
        thread_id,
        messages,
        agent_tasks,
        usage,
    } = event
    {
        state.apply_thread_snapshot(thread_id, messages, agent_tasks, usage);
    } else {
        let should_flush_queue = matches!(event, RuntimeToUiEvent::RunFinished);
        state.apply_event(event);
        if should_flush_queue {
            input::flush_queued_user_inputs(state, request_tx);
        }
    }

    UpdateOutcome::redraw()
}

fn handle_key_event(
    state: &mut AppState,
    code: KeyCode,
    modifiers: KeyModifiers,
    request_tx: &mut Effects,
) -> bool {
    let focus = crate::app::focus::current(state);
    if matches!(
        focus,
        crate::app::focus::Focus::Page | crate::app::focus::Focus::Model
    ) && let Some(ref mut step) = state.dialogs.interaction_step
    {
        let consumed = input::handle_interaction_key(step, code, request_tx);
        if !consumed {
            state.dialogs.interaction_step = None;
            state.dialogs.interaction_request = None;
        }
        return true;
    }

    if focus == crate::app::focus::Focus::Help {
        handle_help_drawer_key(state, code, modifiers);
        return true;
    }

    if focus == crate::app::focus::Focus::Plan {
        handle_plan_approval_key(state, code, request_tx);
        return true;
    }

    if focus == crate::app::focus::Focus::Pause {
        handle_tool_pause_key(state, code, request_tx);
        return true;
    }

    if focus == crate::app::focus::Focus::SessionSelector {
        crate::features::sessions::update::handle_selector_key(state, code);
        return true;
    }

    if code == KeyCode::Esc {
        if let Some(task_id) = state.sessions.active_session_task_id.clone()
            && state.sessions.subagents.values().any(|node| {
                node.task_id == task_id
                    && matches!(node.status, TaskStatus::Running | TaskStatus::Cancelling)
            })
        {
            let _ = request_tx.send(ClientRequest::AgentTaskCancel { task_id });
            return true;
        }
        if state.sessions.active_session_task_id.is_none()
            && (matches!(
                state.sessions.views["main"].agent_status,
                AgentStatus::Working | AgentStatus::Thinking
            ) || state.has_active_agent_tasks())
        {
            let _ = request_tx.send(ClientRequest::RunCancel);
            return true;
        }
    }

    if is_profile_toggle_key(code, modifiers) {
        let _ = request_tx.send(ClientRequest::ProfileToggle);
        return true;
    }

    if state.composer.autocomplete.visible {
        handle_command_autocomplete_key(state, code, modifiers, request_tx);
        return true;
    }

    if state.composer.mention_autocomplete.visible {
        handle_mention_autocomplete_key(state, code, modifiers);
        return true;
    }

    handle_composer_key(state, code, modifiers, request_tx)
}

pub fn is_profile_toggle_key(code: KeyCode, modifiers: KeyModifiers) -> bool {
    code == KeyCode::BackTab || (code == KeyCode::Tab && modifiers.contains(KeyModifiers::SHIFT))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::event::*;
    use crate::app::event::{
        ActiveProfile, AgentTaskEvent, AgentTaskEventEnvelope, AgentTaskExecutionMode, CommandKind,
        CommandSummary, EditPermissionPreview, PermissionPreview, SubmittedPlan, ToolPauseRequest,
        UserInputOption, UserInputPreview, UserInputQuestion,
    };
    use crate::app::state::InputMention;
    use crate::features::timeline::model::MentionKind;
    use chrono::Utc;
    use crossterm::event::{KeyEvent, MouseButton, MouseEventKind};
    use std::path::PathBuf;

    fn add_background_task(state: &mut AppState, task_id: &str, thread_id: &str) {
        state.apply_event(RuntimeToUiEvent::AgentTaskEvent(AgentTaskEventEnvelope {
            task_id: task_id.to_string(),
            thread_id: thread_id.to_string(),
            parent_task_id: None,
            owner_thread_id: "owner".to_string(),
            truncated: false,
            payload: AgentTaskEvent::Started {
                parent_thread_id: "owner".to_string(),
                spawn_tool_use_id: format!("tool-{task_id}"),
                agent: "Explore".to_string(),
                title: format!("Task {task_id}"),
                initial_prompt: omini_domain::conversation::UserInput {
                    intent: omini_domain::input::UserInputIntent::Message,
                    parts: vec![omini_domain::input::InputPart::Text {
                        text: "inspect".to_string(),
                    }],
                    attachments: Vec::new(),
                },
                depth: 1,
                execution_mode: AgentTaskExecutionMode::Background,
            },
        }));
    }

    fn permission_pause(tool_use_id: &str) -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "bash".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Custom {
                tool_name: "bash".to_string(),
                payload: serde_json::Map::new(),
            }),
        }
    }

    fn edit_permission_pause(tool_use_id: &str) -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "edit".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::Permission(PermissionPreview::Edit(EditPermissionPreview {
                summary: "Edit /tmp/demo.rs".to_string(),
                path: "/tmp/demo.rs".to_string(),
                replacement_count: 1,
                diff: String::new(),
            })),
        }
    }

    fn state_with_permission_pause() -> AppState {
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
            "tool_1",
        )));
        state
    }

    fn user_input_pause(tool_use_id: &str) -> ToolPauseRequest {
        ToolPauseRequest {
            tool_use_id: tool_use_id.to_string(),
            preview_tool_use_id: None,
            tool_name: "ask_user".to_string(),
            permission_source: None,
            source_thread_id: None,
            source_agent_label: None,
            kind: ToolPauseKind::UserInput(UserInputPreview {
                questions: vec![UserInputQuestion {
                    id: "q1".to_string(),
                    header: "header".to_string(),
                    question: "Pick one".to_string(),
                    options: (0..3)
                        .map(|i| UserInputOption {
                            label: format!("opt{i}"),
                            description: String::new(),
                        })
                        .collect(),
                }],
            }),
        }
    }

    fn state_with_user_input_pause() -> AppState {
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(user_input_pause(
            "tool_1",
        )));
        state
    }

    fn submitted_plan() -> SubmittedPlan {
        SubmittedPlan {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            markdown: "# Plan\n\n- Step".to_string(),
            path: PathBuf::from("/tmp/plan.md"),
            created_at: Utc::now(),
        }
    }

    fn recv_pause_response(tx: &mut Effects) -> omini_protocol::ToolPauseResponse {
        let Some(ClientRequest::ToolPauseResolve { response, .. }) = tx.requests.pop_front() else {
            panic!("expected tool pause response");
        };
        response
    }

    fn history_input(text: &str) -> omini_protocol::HistoryItem {
        omini_protocol::HistoryItem::UserInput(omini_domain::conversation::UserInput {
            intent: omini_domain::input::UserInputIntent::Message,
            parts: vec![omini_domain::input::InputPart::Text {
                text: text.to_string(),
            }],
            attachments: Vec::new(),
        })
    }

    /// 服务端回显是插入消息的唯一展示来源，轮次结束不再追加本地副本。
    #[test]
    fn intervention_echo_once() {
        // 给定：主会话运行中暂存一条输入。
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::RunStarted);
        state.composer.input = "first".to_string();
        let mut tx = Effects::default();
        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.composer.queued_user_inputs.len(), 1);

        // 当：插入队列并收到服务端回显。
        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::ALT, &mut tx);
        let Some(ClientRequest::RunInterveneInput {
            input,
            client_echo_id: Some(client_echo_id),
        }) = tx.requests.pop_front()
        else {
            panic!("expected intervention request");
        };
        assert_eq!(
            input.input.parts,
            vec![omini_protocol::InputPart::Text {
                text: "first".to_string(),
            }]
        );
        assert!(state.composer.queued_user_inputs.is_empty());
        assert!(state.sessions.views["main"].messages.is_empty());
        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::ALT, &mut tx);
        assert!(tx.requests.is_empty());
        handle_runtime_event(
            &mut state,
            RuntimeToUiEvent::UserMessageInjected {
                item: history_input("first"),
                client_echo_id: Some(client_echo_id),
            },
            &mut tx,
        );

        // 则：轮次与运行结束后，消息仅展示一次，也不会再次发送。
        handle_runtime_event(&mut state, RuntimeToUiEvent::TurnEnded, &mut tx);
        handle_runtime_event(&mut state, RuntimeToUiEvent::RunFinished, &mut tx);
        assert!(tx.requests.is_empty());
        assert_eq!(
            state.sessions.views["main"]
                .messages
                .iter()
                .filter(|message| matches!(message, crate::app::state::UiMessage::UserInput(_)))
                .count(),
            1
        );
    }

    /// 多次插入后仍可暂存，运行结束只发送最后尚未插入的队列。
    #[test]
    fn consecutive_injections() {
        // 给定：主会话运行中可连续输入。
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::RunStarted);
        let mut tx = Effects::default();

        // 当：首条尚未回显时继续输入，并再次插入相同文本。
        let mut echo_ids = Vec::new();
        for _ in 0..2 {
            state.composer.input = "same".to_string();
            handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);
            handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::ALT, &mut tx);
            let Some(ClientRequest::RunInterveneInput {
                client_echo_id: Some(client_echo_id),
                ..
            }) = tx.requests.pop_front()
            else {
                panic!("expected intervention request");
            };
            assert!(state.composer.queued_user_inputs.is_empty());
            echo_ids.push(client_echo_id);
        }
        for client_echo_id in &echo_ids {
            handle_runtime_event(
                &mut state,
                RuntimeToUiEvent::UserMessageInjected {
                    item: history_input("same"),
                    client_echo_id: Some(client_echo_id.clone()),
                },
                &mut tx,
            );
        }
        state.composer.input = "tail".to_string();
        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);
        handle_runtime_event(&mut state, RuntimeToUiEvent::TurnEnded, &mut tx);
        handle_runtime_event(&mut state, RuntimeToUiEvent::RunFinished, &mut tx);

        // 则：不同提交 ID 的同文消息都可见，结束时只提交剩余输入。
        assert_ne!(echo_ids[0], echo_ids[1]);
        assert_eq!(
            state.sessions.views["main"]
                .messages
                .iter()
                .filter(|message| matches!(message, crate::app::state::UiMessage::UserInput(_)))
                .count(),
            2
        );
        let Some(ClientRequest::RunSubmitUserInput { input, .. }) = tx.requests.pop_front() else {
            panic!("expected remaining queued input");
        };
        assert_eq!(
            input.input.parts,
            vec![omini_protocol::InputPart::Text {
                text: "tail".to_string(),
            }]
        );
        assert!(tx.requests.is_empty());
        assert!(state.composer.queued_user_inputs.is_empty());
    }

    /// 提交错误只由现有通知路径呈现，不自动重发可能已落库的输入。
    #[test]
    fn intervention_error_notice() {
        // 给定：运行中存在待插入输入。
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::RunStarted);
        state.composer.input = "first".to_string();
        let mut tx = Effects::default();
        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);

        // 当：插入请求失败，客户端报告错误。
        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::ALT, &mut tx);
        assert!(matches!(
            tx.requests.pop_front(),
            Some(ClientRequest::RunInterveneInput { .. })
        ));
        handle_runtime_event(&mut state, RuntimeToUiEvent::error("failed"), &mut tx);

        // 则：错误可见，队列保持清空，且不会自动重试。
        assert!(state.composer.queued_user_inputs.is_empty());
        assert!(tx.requests.is_empty());
        assert!(state.sessions.views["main"].messages.iter().any(|message| {
            matches!(message, crate::app::state::UiMessage::SystemEvent(crate::app::state::UiSystemEvent::Notification(notification)) if notification.kind == crate::app::event::NotificationKind::Error)
        }));
    }

    #[test]
    fn idle_main_accepts_input_and_escape_cancels_active_agent_tasks() {
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::AgentTaskEvent(AgentTaskEventEnvelope {
            task_id: "task_1".to_string(),
            thread_id: "agent_1".to_string(),
            parent_task_id: None,
            owner_thread_id: "owner".to_string(),
            truncated: false,
            payload: AgentTaskEvent::Started {
                parent_thread_id: "owner".to_string(),
                spawn_tool_use_id: "tool_1".to_string(),
                agent: "general".to_string(),
                title: "Background work".to_string(),
                initial_prompt: omini_domain::conversation::UserInput {
                    intent: omini_domain::input::UserInputIntent::Message,
                    parts: vec![omini_domain::input::InputPart::Text {
                        text: "Background work".to_string(),
                    }],
                    attachments: Vec::new(),
                },
                depth: 1,
                execution_mode: AgentTaskExecutionMode::Background,
            },
        }));
        assert_eq!(state.sessions.views["main"].agent_status, AgentStatus::Idle);
        let mut tx = Effects::default();

        handle_key_event(&mut state, KeyCode::Char('x'), KeyModifiers::NONE, &mut tx);
        assert_eq!(state.composer.input, "x");

        handle_key_event(&mut state, KeyCode::Enter, KeyModifiers::ALT, &mut tx);
        assert!(tx.requests.is_empty());

        handle_key_event(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);
        assert!(state.composer.queued_user_inputs.is_empty());
        assert!(matches!(
            tx.requests.pop_front(),
            Some(ClientRequest::RunSubmitUserInput { .. })
        ));

        handle_key_event(&mut state, KeyCode::Esc, KeyModifiers::NONE, &mut tx);
        assert!(matches!(
            tx.requests.pop_front(),
            Some(ClientRequest::RunCancel)
        ));
    }

    #[test]
    fn session_list_navigation() {
        let mut state = AppState::new();
        add_background_task(&mut state, "task_1", "thread_1");
        add_background_task(&mut state, "task_2", "thread_2");
        state.composer.input = "first\nsecond".to_string();
        state.composer.cursor_char = 6;
        let mut tx = Effects::default();

        handle_key_event(&mut state, KeyCode::Up, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.composer.cursor_char, 0);
        assert!(!state.sessions.session_selector_focused);
        handle_key_event(&mut state, KeyCode::Down, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.composer.cursor_char, 6);
        assert!(!state.sessions.session_selector_focused);
        handle_key_event(&mut state, KeyCode::Down, KeyModifiers::NONE, &mut tx);
        assert!(state.sessions.session_selector_focused);
        assert!(state.sessions.active_session_task_id.is_none());
        handle_key_event(&mut state, KeyCode::Down, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.sessions.session_selection_index, 1);
        assert!(state.sessions.active_session_task_id.is_none());
        assert!(state.sessions.session_selector_focused);
        handle_key_event(&mut state, KeyCode::Down, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.sessions.session_selection_index, 2);
        assert!(state.sessions.active_session_task_id.is_none());
        handle_key_event(&mut state, KeyCode::Up, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.sessions.session_selection_index, 1);
        assert!(state.sessions.active_session_task_id.is_none());
        assert_eq!(state.composer.cursor_char, 6);
        handle_key_event(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);
        assert_eq!(
            state.sessions.active_session_task_id.as_deref(),
            Some("task_1")
        );
        assert!(!state.sessions.session_selector_focused);
        handle_key_event(&mut state, KeyCode::Up, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.composer.cursor_char, 0);
        handle_key_event(&mut state, KeyCode::Down, KeyModifiers::NONE, &mut tx);
        handle_key_event(&mut state, KeyCode::Down, KeyModifiers::NONE, &mut tx);
        handle_key_event(&mut state, KeyCode::Up, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.sessions.session_selection_index, 0);
        assert_eq!(
            state.sessions.active_session_task_id.as_deref(),
            Some("task_1")
        );
        assert!(state.sessions.session_selector_focused);
        handle_key_event(&mut state, KeyCode::Up, KeyModifiers::NONE, &mut tx);
        assert!(!state.sessions.session_selector_focused);
        assert_eq!(
            state.sessions.active_session_task_id.as_deref(),
            Some("task_1")
        );
        assert_eq!(state.composer.input, "first\nsecond");
        assert!(tx.requests.is_empty());
    }

    #[test]
    fn switch_prunes_terminal() {
        let mut state = AppState::new();
        add_background_task(&mut state, "task_1", "thread_1");
        add_background_task(&mut state, "task_2", "thread_2");
        state.sessions.active_session_task_id = Some("task_1".to_string());
        state.sessions.subagents.get_mut("thread_1").unwrap().status = TaskStatus::Completed;
        state.sessions.session_selector_focused = true;
        state.sessions.session_selection_index = 1;
        let mut tx = Effects::default();

        handle_key_event(&mut state, KeyCode::Down, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.sessions.session_selection_index, 2);
        assert_eq!(
            state.sessions.active_session_task_id.as_deref(),
            Some("task_1")
        );
        assert!(state.sessions.views.contains_key("task_1"));
        handle_key_event(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);

        assert_eq!(
            state.sessions.active_session_task_id.as_deref(),
            Some("task_2")
        );
        assert_eq!(state.sessions.subagent_order, vec!["task_2"]);
        assert_eq!(state.sessions.session_selection_index, 1);
        assert!(!state.sessions.session_selector_focused);
        assert!(!state.sessions.views.contains_key("task_1"));
    }

    /// 验证回收列表前部的终态任务后，高亮项仍指向原候选会话。
    #[test]
    fn verify_task_pruning() {
        let mut state = AppState::new();
        for index in 1..=4 {
            add_background_task(
                &mut state,
                &format!("task_{index}"),
                &format!("thread_{index}"),
            );
        }
        state.sessions.active_session_task_id = Some("task_3".to_string());
        state.sessions.session_selector_focused = true;
        state.sessions.session_selection_index = 4;
        state.sessions.subagents.get_mut("thread_1").unwrap().status = TaskStatus::Completed;

        state.prune_terminal_tasks();

        assert_eq!(
            state.sessions.subagent_order,
            vec!["task_2", "task_3", "task_4"]
        );
        assert_eq!(state.sessions.session_selection_index, 3);
        assert_eq!(
            state.sessions.active_session_task_id.as_deref(),
            Some("task_3")
        );
    }

    /// 验证列表焦点隔离输入与粘贴，Esc 只退出列表而不取消任务。
    #[test]
    fn verify_selector_focus() {
        let mut state = AppState::new();
        add_background_task(&mut state, "task_1", "thread_1");
        add_background_task(&mut state, "task_2", "thread_2");
        state.sessions.active_session_task_id = Some("task_1".to_string());
        state.sessions.session_selector_focused = true;
        state.sessions.session_selection_index = 1;
        state.composer.input = "draft".to_string();
        state.composer.cursor_char = 5;
        let mut tx = Effects::default();

        assert_eq!(
            crate::app::focus::current(&state),
            crate::app::focus::Focus::SessionSelector
        );

        handle_key_event(&mut state, KeyCode::Down, KeyModifiers::NONE, &mut tx);
        assert_eq!(state.sessions.session_selection_index, 2);
        assert_eq!(
            state.sessions.active_session_task_id.as_deref(),
            Some("task_1")
        );
        handle_input_event(&mut state, Event::Paste(" pasted".into()), &mut tx);
        handle_key_event(&mut state, KeyCode::Char('x'), KeyModifiers::NONE, &mut tx);
        assert_eq!(state.composer.input, "draft");
        handle_key_event(&mut state, KeyCode::Esc, KeyModifiers::NONE, &mut tx);
        assert!(!state.sessions.session_selector_focused);
        assert_eq!(
            state.sessions.active_session_task_id.as_deref(),
            Some("task_1")
        );
        assert!(tx.requests.is_empty());
        handle_input_event(&mut state, Event::Paste(" pasted".into()), &mut tx);
        assert_eq!(state.composer.input, "draft pasted");
        handle_key_event(&mut state, KeyCode::Esc, KeyModifiers::NONE, &mut tx);
        assert!(matches!(
            tx.requests.pop_front(),
            Some(ClientRequest::AgentTaskCancel { task_id }) if task_id == "task_1"
        ));
    }

    #[test]
    fn empty_session_list_down() {
        let mut state = AppState::new();
        state.composer.input = "hello".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Down, KeyModifiers::NONE, &mut tx);

        assert_eq!(state.session_count(), 1);
        assert!(!state.sessions.session_selector_focused);
        assert_eq!(state.composer.cursor_char, 5);
    }

    #[test]
    fn task_input_escape_scope() {
        let mut state = AppState::new();
        add_background_task(&mut state, "task_1", "thread_1");
        add_background_task(&mut state, "task_2", "thread_2");
        state.sessions.active_session_task_id = Some("task_1".to_string());
        state.sessions.views["main"].main_query_active = true;
        state.sessions.views["main"].agent_status = AgentStatus::Working;
        state.composer.input = "review this".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);
        let Some(ClientRequest::AgentTaskSubmitInput { task_id, input, .. }) =
            tx.requests.pop_front()
        else {
            panic!("expected selected task input request");
        };
        assert_eq!(task_id, "task_1");
        assert!(
            matches!(input.input.parts.as_slice(), [omini_protocol::InputPart::Text { text }] if text == "review this")
        );
        assert!(state.composer.queued_user_inputs.is_empty());
        assert_eq!(
            state.sessions.views["main"].agent_status,
            AgentStatus::Working
        );

        handle_key_event(&mut state, KeyCode::Esc, KeyModifiers::NONE, &mut tx);
        assert!(matches!(
            tx.requests.pop_front(),
            Some(ClientRequest::AgentTaskCancel { task_id }) if task_id == "task_1"
        ));
        assert_eq!(
            state.sessions.subagents["thread_2"].status,
            TaskStatus::Running
        );
    }

    #[test]
    fn skill_input_routing() {
        let mut state = AppState::new();
        add_background_task(&mut state, "task_1", "thread_1");
        state.sessions.active_session_task_id = Some("task_1".to_string());
        state.composer.autocomplete.all_commands = vec![CommandSummary {
            name: "review".to_string(),
            aliases: Vec::new(),
            description: String::new(),
            sort_weight: 0,
            has_args: true,
            args_description: None,
            kind: CommandKind::Skill,
        }];
        state.composer.input = "/review inspect src/main.rs".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);

        let Some(ClientRequest::AgentTaskSubmitInput { task_id, input, .. }) =
            tx.requests.pop_front()
        else {
            panic!("expected selected task input request");
        };
        assert_eq!(task_id, "task_1");
        assert!(matches!(input.input.parts.as_slice(), [
            omini_protocol::InputPart::Skill { name },
            omini_protocol::InputPart::Text { text },
        ] if name == "review" && text == " inspect src/main.rs"));
    }

    #[test]
    fn terminal_task_read_only() {
        let mut state = AppState::new();
        add_background_task(&mut state, "task_1", "thread_1");
        state.sessions.active_session_task_id = Some("task_1".to_string());
        state.sessions.subagents.get_mut("thread_1").unwrap().status = TaskStatus::Completed;
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Char('x'), KeyModifiers::NONE, &mut tx);
        assert!(state.composer.input.is_empty());
        state.composer.input = "cannot send".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);
        assert!(tx.requests.is_empty());
    }

    #[test]
    fn tab_starts_permission_deny_note_mode() {
        let mut state = state_with_permission_pause();
        let mut tx = Effects::default();

        handle_tool_pause_key(&mut state, KeyCode::Tab, &mut tx);

        assert!(state.note_mode());
        assert_eq!(state.dialogs.permission.permission_selected, 1);
        assert_eq!(state.current_user_input_note(), "");
    }

    // 修复 Bug 1 的回归测试:ask_user 暂停进入 note 模式后,`j` / `k`
    // 必须是普通字符插入,不能被 vim 风格方向键别名吞掉。
    #[test]
    fn user_input_note_mode_inserts_jk_as_text() {
        let mut state = state_with_user_input_pause();
        let mut tx = Effects::default();

        // Tab 切换到 note 模式
        handle_tool_pause_key(&mut state, KeyCode::Tab, &mut tx);
        assert!(state.note_mode());
        let initial_selected = state.current_user_input_selected();

        for c in "skip、kick this off".chars() {
            handle_tool_pause_key(&mut state, KeyCode::Char(c), &mut tx);
        }

        assert_eq!(state.current_user_input_note(), "skip、kick this off");
        assert_eq!(state.current_user_input_selected(), initial_selected);
    }

    // 修复 Bug 1 的回归测试:note 模式下方向键 `↑` / `↓` 仍要能切选项。
    // 仅验证"切换选项"语义,saturate 边界(max_selected 取 options.len()
    // 而非 len-1 导致的 off-by-one)与本 issue 无关,不在此测试覆盖。
    #[test]
    fn user_input_note_mode_arrows_still_navigate_options() {
        let mut state = state_with_user_input_pause();
        let mut tx = Effects::default();

        handle_tool_pause_key(&mut state, KeyCode::Tab, &mut tx);
        assert!(state.note_mode());
        assert_eq!(state.current_user_input_selected(), 0);

        handle_tool_pause_key(&mut state, KeyCode::Down, &mut tx);
        assert_eq!(state.current_user_input_selected(), 1);
        handle_tool_pause_key(&mut state, KeyCode::Up, &mut tx);
        assert_eq!(state.current_user_input_selected(), 0);
    }

    // Permission 暂停的 note 模式回归基线:`j` / `k` 原本就能插入,
    // 此处固定当前行为,防止后续改动破坏非 ask_user 路径。
    #[test]
    fn permission_note_mode_inserts_jk_as_text() {
        let mut state = state_with_permission_pause();
        let mut tx = Effects::default();

        handle_tool_pause_key(&mut state, KeyCode::Tab, &mut tx);
        assert!(state.note_mode());

        for c in "jk".chars() {
            handle_tool_pause_key(&mut state, KeyCode::Char(c), &mut tx);
        }

        assert_eq!(state.current_user_input_note(), "jk");
    }

    // 修复 Bug 2 的回归测试:note 模式期间 `Event::Paste` 走
    // `handle_input_event` 中独立的 note 分支,把每个字符塞进 note。
    // 之前主输入框分支的 `active_tool_pause().is_none()` 守卫把整条
    // bracketed paste 路径吞掉,note 模式无处可粘。
    #[test]
    fn user_input_note_mode_paste_inserts_chars() {
        let mut state = state_with_user_input_pause();
        let mut tx = Effects::default();

        handle_tool_pause_key(&mut state, KeyCode::Tab, &mut tx);
        assert!(state.note_mode());

        let outcome =
            handle_input_event(&mut state, Event::Paste("pasted note".to_string()), &mut tx);
        assert!(outcome.redraw);

        assert_eq!(state.current_user_input_note(), "pasted note");
    }

    #[test]
    fn permission_note_mode_paste_inserts_chars() {
        let mut state = state_with_permission_pause();
        let mut tx = Effects::default();

        handle_tool_pause_key(&mut state, KeyCode::Tab, &mut tx);
        assert!(state.note_mode());

        let outcome = handle_input_event(&mut state, Event::Paste("hi".to_string()), &mut tx);
        assert!(outcome.redraw);

        assert_eq!(state.current_user_input_note(), "hi");
    }

    #[test]
    fn permission_deny_note_is_sent_on_enter() {
        let mut state = state_with_permission_pause();
        let mut tx = Effects::default();

        handle_tool_pause_key(&mut state, KeyCode::Tab, &mut tx);
        for c in "Need context".chars() {
            handle_tool_pause_key(&mut state, KeyCode::Char(c), &mut tx);
        }
        handle_tool_pause_key(&mut state, KeyCode::Enter, &mut tx);

        assert_eq!(
            recv_pause_response(&mut tx),
            omini_protocol::ToolPauseResponse::Permission {
                approved: false,
                note: Some("Need context".to_string()),
            }
        );
    }

    #[test]
    fn permission_esc_denies_without_note() {
        let mut state = state_with_permission_pause();
        let mut tx = Effects::default();

        handle_tool_pause_key(&mut state, KeyCode::Esc, &mut tx);

        assert_eq!(
            recv_pause_response(&mut tx),
            omini_protocol::ToolPauseResponse::Permission {
                approved: false,
                note: None,
            }
        );
    }

    #[test]
    fn permission_approval_ignores_stale_note() {
        let mut state = state_with_permission_pause();
        let mut tx = Effects::default();

        handle_tool_pause_key(&mut state, KeyCode::Tab, &mut tx);
        for c in "Do not run".chars() {
            handle_tool_pause_key(&mut state, KeyCode::Char(c), &mut tx);
        }
        handle_tool_pause_key(&mut state, KeyCode::Tab, &mut tx);
        handle_tool_pause_key(&mut state, KeyCode::Char('y'), &mut tx);

        assert_eq!(
            recv_pause_response(&mut tx),
            omini_protocol::ToolPauseResponse::Permission {
                approved: true,
                note: None,
            }
        );
        assert_eq!(
            state.sessions.views["main"].agent_status,
            AgentStatus::Working
        );
    }

    #[test]
    fn active_permission_pause_allows_message_text_selection_outside_drawer() {
        let mut state = state_with_permission_pause();
        state.register_selectable_screen_line(2, 0, 80, "assistant line".to_string());
        state.geometry.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.geometry.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);

        handle_mouse_event(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            2,
            0,
            &mut Effects::default(),
        );

        assert!(state.selection.is_selecting_text);
        assert!(state.selection.text_selection.is_some());
    }

    #[test]
    fn active_permission_pause_allows_drawer_text_selection() {
        let mut state = state_with_permission_pause();
        state.register_selectable_screen_line(12, 3, 74, "drawer line".to_string());
        state.geometry.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.geometry.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);

        handle_mouse_event(
            &mut state,
            MouseEventKind::Down(MouseButton::Left),
            12,
            3,
            &mut Effects::default(),
        );

        assert!(state.selection.is_selecting_text);
        assert!(state.selection.text_selection.is_some());
    }

    #[test]
    fn scrollable_edit_permission_drawer_captures_mouse_wheel() {
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(edit_permission_pause(
            "tool_1",
        )));
        state.geometry.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.geometry.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);
        state.geometry.permission_drawer_content_len = 8;
        state.dialogs.permission.permission_scroll_offset = 0;

        handle_mouse_event(
            &mut state,
            MouseEventKind::ScrollUp,
            2,
            0,
            &mut Effects::default(),
        );

        assert!(state.dialogs.permission.permission_scroll_offset > 0);
        assert_eq!(state.sessions.views["main"].scroll_offset, 0);
        assert!(state.sessions.views["main"].auto_scroll);
    }

    #[test]
    fn non_scrollable_or_non_edit_permission_drawer_uses_message_wheel() {
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::ToolPauseRequested(edit_permission_pause(
            "tool_1",
        )));
        state.geometry.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.geometry.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);
        state.geometry.permission_drawer_content_len = 2;

        handle_mouse_event(
            &mut state,
            MouseEventKind::ScrollUp,
            12,
            3,
            &mut Effects::default(),
        );

        assert_eq!(
            state.dialogs.permission.permission_scroll_offset,
            usize::MAX
        );
        assert!(state.sessions.views["main"].scroll_offset > 0);
        assert!(!state.sessions.views["main"].auto_scroll);

        let mut state = state_with_permission_pause();
        state.geometry.permission_drawer_area = ratatui::layout::Rect::new(0, 10, 80, 6);
        state.geometry.permission_drawer_body_area = ratatui::layout::Rect::new(3, 12, 74, 2);
        state.geometry.permission_drawer_content_len = 8;
        state.dialogs.permission.permission_scroll_offset = 0;

        handle_mouse_event(
            &mut state,
            MouseEventKind::ScrollUp,
            12,
            3,
            &mut Effects::default(),
        );

        assert_eq!(state.dialogs.permission.permission_scroll_offset, 0);
        assert!(state.sessions.views["main"].scroll_offset > 0);
        assert!(!state.sessions.views["main"].auto_scroll);
    }

    #[test]
    fn plan_approval_enter_sends_selected_action() {
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::PlanSubmitted(submitted_plan()));
        state.dialogs.plan.plan_approval_selected = 1;
        let mut tx = Effects::default();

        handle_plan_approval_key(&mut state, KeyCode::Enter, &mut tx);

        let Some(ClientRequest::PlanResolve { action }) = tx.requests.pop_front() else {
            panic!("expected plan approval response");
        };
        assert_eq!(
            action,
            omini_protocol::PlanApprovalAction::ApproveInNewThread {
                profile: omini_protocol::PlanExecutionProfile::Main,
            }
        );
        assert!(state.dialogs.plan.plan_approval.is_none());
        assert!(!state.dialogs.plan.plan_approval_auto);
    }

    #[test]
    fn plan_approval_auto_toggle_sends_auto_profile() {
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::PlanSubmitted(submitted_plan()));
        let mut tx = Effects::default();

        handle_plan_approval_key(&mut state, KeyCode::Char('a'), &mut tx);
        assert!(state.dialogs.plan.plan_approval_auto);
        handle_plan_approval_key(&mut state, KeyCode::Char('1'), &mut tx);

        let Some(ClientRequest::PlanResolve { action }) = tx.requests.pop_front() else {
            panic!("expected plan approval response");
        };
        assert_eq!(
            action,
            omini_protocol::PlanApprovalAction::Approve {
                profile: omini_protocol::PlanExecutionProfile::Auto,
            }
        );
        assert!(state.dialogs.plan.plan_approval.is_none());
        assert!(!state.dialogs.plan.plan_approval_auto);
    }

    #[test]
    fn plan_approval_esc_continues_discussion() {
        let mut state = AppState::new();
        state.apply_event(RuntimeToUiEvent::PlanSubmitted(submitted_plan()));
        state.dialogs.plan.plan_approval_auto = true;
        let mut tx = Effects::default();

        handle_plan_approval_key(&mut state, KeyCode::Esc, &mut tx);

        let Some(ClientRequest::PlanResolve { action }) = tx.requests.pop_front() else {
            panic!("expected plan approval response");
        };
        assert_eq!(
            action,
            omini_protocol::PlanApprovalAction::ContinueDiscussing
        );
        assert!(state.dialogs.plan.plan_approval.is_none());
        assert!(!state.dialogs.plan.plan_approval_auto);
    }

    #[test]
    fn shift_tab_sends_profile_toggle() {
        let mut state = AppState::new();
        let mut tx = Effects::default();

        let handled = handle_key_event(&mut state, KeyCode::BackTab, KeyModifiers::SHIFT, &mut tx);

        assert!(handled);
        let Some(ClientRequest::ProfileToggle) = tx.requests.pop_front() else {
            panic!("expected profile toggle event");
        };
    }

    #[test]
    fn auto_profile_permission_pause_uses_drawer_when_forwarded() {
        let mut state = AppState::new();
        state.project.status_bar.active_profile = ActiveProfile::Auto;
        let mut tx = Effects::default();

        let outcome = handle_runtime_event(
            &mut state,
            RuntimeToUiEvent::ToolPauseRequested(permission_pause("tool_1")),
            &mut tx,
        );

        assert!(outcome.redraw);
        assert!(state.active_tool_pause().is_some());
        assert_eq!(
            state.sessions.views["main"].agent_status,
            AgentStatus::AwaitingInput
        );
        assert!(tx.requests.is_empty());
    }

    #[test]
    fn repeat_key_events_edit_composer() {
        let mut state = AppState::new();
        let mut tx = Effects::default();

        handle_input_event(
            &mut state,
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Press,
            )),
            &mut tx,
        );
        handle_input_event(
            &mut state,
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('a'),
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )),
            &mut tx,
        );
        handle_input_event(
            &mut state,
            Event::Key(KeyEvent::new_with_kind(
                KeyCode::Backspace,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )),
            &mut tx,
        );

        assert_eq!(state.composer.input, "a");
        assert_eq!(state.composer.cursor_char, 1);
    }

    #[test]
    fn compact_command_sets_manual_compact_working_state() {
        let mut state = AppState::new();
        state.composer.input = "/compact".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);

        assert_eq!(
            state.sessions.views["main"].agent_status,
            AgentStatus::Working
        );
        assert!(state.sessions.views["main"].manual_compact_running);
        assert!(state.sessions.views["main"].run_timer.is_some());
        let Some(ClientRequest::ContextCompact { instructions }) = tx.requests.pop_front() else {
            panic!("expected compact command");
        };
        assert_eq!(instructions, None);
    }

    #[test]
    fn command_submit_preserves_argument_mentions() {
        let mut state = AppState::new();
        state.composer.autocomplete.all_commands = vec![CommandSummary {
            name: "commit-message".to_string(),
            aliases: vec![],
            description: String::new(),
            sort_weight: 0,
            has_args: true,
            args_description: None,
            kind: CommandKind::Skill,
        }];
        state.composer.input = "/commit-message summarize @src/main.rs".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        state.composer.input_mentions.push(InputMention {
            start_char: 26,
            end_char: 38,
            kind: MentionKind::File,
            label: "src/main.rs".to_string(),
            target: "src/main.rs".to_string(),
            description: "file".to_string(),
        });
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);

        let Some(ClientRequest::RunSubmitUserInput { input, .. }) = tx.requests.pop_front() else {
            panic!("expected command draft");
        };
        assert_eq!(
            input.input.parts,
            vec![
                omini_protocol::InputPart::Skill {
                    name: "commit-message".to_string(),
                },
                omini_protocol::InputPart::Text {
                    text: " summarize ".to_string(),
                },
                omini_protocol::InputPart::File {
                    path: "src/main.rs".to_string(),
                    label: Some("src/main.rs".to_string()),
                },
            ]
        );
    }

    #[test]
    fn unknown_slash_command_sends_as_normal_message() {
        let mut state = AppState::new();
        state.composer.input = "/nonexistent some text".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);

        let Some(ClientRequest::RunSubmitUserInput { input, .. }) = tx.requests.pop_front() else {
            panic!("expected normal user input submission");
        };
        assert_eq!(
            input.input.parts,
            vec![omini_protocol::InputPart::Text {
                text: "/nonexistent some text".to_string(),
            }]
        );
    }

    #[test]
    fn init_is_sent_as_typed_command_without_expanding_prompt() {
        let mut state = AppState::new();
        state.composer.input = "/init notes".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);

        let Some(ClientRequest::RunExecuteCommand { command, input, .. }) = tx.requests.pop_front()
        else {
            panic!("expected typed init command");
        };
        assert_eq!(command, omini_protocol::RunCommand::Init);
        assert_eq!(
            input.input.parts,
            vec![omini_protocol::InputPart::Text {
                text: " notes".to_string(),
            }]
        );
    }

    #[test]
    fn effort_command_accepts_max() {
        let mut state = AppState::new();
        state.composer.input = "/effort max".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);

        let Some(ClientRequest::ModelThinkingEffortSet { effort }) = tx.requests.pop_front() else {
            panic!("expected effort request");
        };
        assert_eq!(effort, omini_protocol::ThinkingEffort::Max);
    }

    #[test]
    fn manual_compact_does_not_queue_normal_user_input() {
        let mut state = AppState::new();
        state.begin_manual_compact();
        state.composer.input = "hello".to_string();
        state.composer.cursor_char = state.composer.input.chars().count();
        let mut tx = Effects::default();

        handle_composer_key(&mut state, KeyCode::Enter, KeyModifiers::NONE, &mut tx);

        assert!(state.composer.queued_user_inputs.is_empty());
        let Some(ClientRequest::RunSubmitUserInput { .. }) = tx.requests.pop_front() else {
            panic!("expected user message");
        };
    }
}
