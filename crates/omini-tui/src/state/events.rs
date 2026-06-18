use super::input::combined_user_draft;
use super::{
    AgentManagerState, AgentManagerView, AgentStatus, InteractionStep, ModelSelectionEntry,
    SubagentNode, UiMessage, UiState, agent_summaries_to_mention_candidates,
};
use crate::types::config::ThinkingEffort;
use crate::types::events::{
    CommandKind, CommandSummary, CompactTrigger, InteractionRequest, Notification,
    NotificationKind, RuntimeToUiEvent, SubagentSnapshot, SubagentStatus,
};
use omini_domain::display::{HistoryItem, UserDraft};
use omini_domain::message::{ContentBlock, Message, Role, ToolResultBlock};
use omini_domain::subagents::AgentSummary;
use std::collections::VecDeque;

const GENERAL_HELP_SELECTABLE_COUNT: usize = 9;

fn ui_message_from_history_item(item: HistoryItem) -> UiMessage {
    match item {
        HistoryItem::Message(message) => UiMessage::Message(message),
        HistoryItem::Display(display) => UiMessage::Display(display),
        HistoryItem::Plan(plan) => UiMessage::ProposedPlan {
            text: plan.markdown,
        },
        HistoryItem::Summary(summary) => UiMessage::CompactSummary {
            text: summary.markdown,
        },
    }
}

impl UiState {
    pub fn is_run_active(&self) -> bool {
        matches!(
            self.agent_status,
            AgentStatus::Working | AgentStatus::Thinking | AgentStatus::AwaitingInput
        )
    }

    /// 清除正在流式构建中的 compact 摘要占位。
    fn clear_pending_compact_summary(&mut self) {
        self.pending_compact_summary = None;
    }

    pub fn take_queued_user_draft(&mut self) -> Option<UserDraft> {
        Self::draft_from_inputs(&mut self.queued_user_inputs)
    }

    pub fn take_queued_user_draft_for_intervention(
        &mut self,
        client_echo_id: String,
    ) -> Option<UserDraft> {
        if !self.pending_intervention_inputs.is_empty() {
            return None;
        }

        let pending = self.queued_user_inputs.drain(..).collect::<VecDeque<_>>();
        let draft = Self::draft_from_input_iter(pending.iter())?;
        self.pending_intervention_inputs = pending;
        self.pending_intervention_client_echo_id = Some(client_echo_id);
        Some(draft)
    }

    fn take_pending_intervention_ui_messages(&mut self) -> (Vec<UiMessage>, Option<String>) {
        let messages = self
            .pending_intervention_inputs
            .drain(..)
            .map(|draft| ui_message_from_history_item(draft.history_item()))
            .collect();
        (messages, self.pending_intervention_client_echo_id.take())
    }

    pub(crate) fn push_optimistic_echo(&mut self, ui_message: UiMessage, client_echo_id: String) {
        self.extend_optimistic_echoes(vec![ui_message], client_echo_id);
    }

    pub(crate) fn extend_optimistic_echoes(
        &mut self,
        ui_messages: Vec<UiMessage>,
        client_echo_id: String,
    ) {
        if ui_messages.is_empty() {
            return;
        }

        let start = self.messages.len();
        let count = ui_messages.len();
        self.messages.extend(ui_messages);
        self.pending_client_echoes
            .insert(client_echo_id, (start..start + count).collect());
    }

    fn take_client_echo_positions(&mut self, client_echo_id: Option<&str>) -> Option<Vec<usize>> {
        self.pending_client_echoes.remove(client_echo_id?)
    }

    fn draft_from_inputs(inputs: &mut VecDeque<UserDraft>) -> Option<UserDraft> {
        if inputs.is_empty() {
            return None;
        }

        let drafts = inputs.drain(..).collect::<Vec<_>>();
        Self::draft_from_input_iter(drafts.iter())
    }

    fn draft_from_input_iter<'a>(inputs: impl Iterator<Item = &'a UserDraft>) -> Option<UserDraft> {
        let drafts = inputs.collect::<Vec<_>>();
        if drafts.is_empty() {
            return None;
        }

        Some(combined_user_draft(&drafts))
    }

    pub fn open_interaction_request(&mut self, req: &InteractionRequest) {
        self.help_drawer = None;
        self.interaction_step = match req {
            InteractionRequest::ModelSelection {
                providers,
                current_provider,
                current_model,
            } => {
                let mut entries: Vec<ModelSelectionEntry> = Vec::new();
                let mut selected = 0;
                let default_thinking = match self.status_bar.thinking_effort {
                    Some(ThinkingEffort::Low) => 1,
                    Some(ThinkingEffort::Medium) => 2,
                    Some(ThinkingEffort::High) => 3,
                    Some(ThinkingEffort::XHigh) => 4,
                    Some(ThinkingEffort::Max) => 5,
                    Some(ThinkingEffort::None) => 0,
                    None => 2,
                };
                let mut sorted: Vec<_> = providers.clone().into_iter().collect();
                sorted.sort_by(|a, b| a.1.name.cmp(&b.1.name));
                for (provider_key, profile) in &sorted {
                    entries.push(ModelSelectionEntry::ProviderHeader {
                        name: profile.name.clone(),
                    });
                    let mut sorted_models: Vec<_> = profile.models.iter().collect();
                    sorted_models.sort_by(|a, b| a.id.cmp(&b.id));
                    for model in sorted_models {
                        if *provider_key == *current_provider && model.id == *current_model {
                            selected = entries.len();
                        }
                        entries.push(ModelSelectionEntry::Model {
                            provider_key: provider_key.clone(),
                            model: model.clone(),
                        });
                    }
                }
                Some(InteractionStep::ModelSelection {
                    entries,
                    selected,
                    thinking_idx: default_thinking,
                    active_provider: current_provider.clone(),
                    active_model: current_model.clone(),
                })
            }
            InteractionRequest::SessionSelection { sessions } => {
                let mut sorted = sessions.clone();
                sorted.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                let all_sessions = sorted.clone();
                let selected = self
                    .current_session_id
                    .as_ref()
                    .and_then(|id| sorted.iter().position(|s| s.id == *id))
                    .unwrap_or(0);
                Some(InteractionStep::Session {
                    sessions: sorted,
                    all_sessions,
                    search: String::new(),
                    selected,
                })
            }
            InteractionRequest::AgentManagement {
                records,
                providers,
                current_provider,
                current_model,
            } => Some(InteractionStep::Agents(Box::new(AgentManagerState::new(
                records.clone(),
                providers.clone(),
                current_provider.clone(),
                current_model.clone(),
            )))),
        };
    }

    pub fn open_help_drawer(&mut self, commands: Vec<CommandSummary>) {
        self.autocomplete.visible = false;
        self.mention_autocomplete.visible = false;
        self.help_drawer = Some(super::HelpDrawerState::new(commands));
    }

    pub fn close_help_drawer(&mut self) {
        self.help_drawer = None;
    }

    pub fn help_next_tab(&mut self) {
        let Some(drawer) = &mut self.help_drawer else {
            return;
        };
        drawer.tab = match drawer.tab {
            super::HelpTab::General => super::HelpTab::Commands,
            super::HelpTab::Commands => super::HelpTab::Skills,
            super::HelpTab::Skills => super::HelpTab::General,
        };
    }

    pub fn help_prev_tab(&mut self) {
        let Some(drawer) = &mut self.help_drawer else {
            return;
        };
        drawer.tab = match drawer.tab {
            super::HelpTab::General => super::HelpTab::Skills,
            super::HelpTab::Commands => super::HelpTab::General,
            super::HelpTab::Skills => super::HelpTab::Commands,
        };
    }

    pub fn help_select_next(&mut self) {
        let Some(drawer) = &mut self.help_drawer else {
            return;
        };
        match drawer.tab {
            super::HelpTab::Commands => {
                let len = command_count(&drawer.commands, CommandKind::Builtin);
                if len > 0 {
                    drawer.command_selected = (drawer.command_selected + 1).min(len - 1);
                }
            }
            super::HelpTab::Skills => {
                let len = command_count(&drawer.commands, CommandKind::Skill);
                if len > 0 {
                    drawer.skill_selected = (drawer.skill_selected + 1).min(len - 1);
                }
            }
            super::HelpTab::General => {
                drawer.general_selected =
                    (drawer.general_selected + 1).min(GENERAL_HELP_SELECTABLE_COUNT - 1);
            }
        }
    }

    pub fn help_select_prev(&mut self) {
        let Some(drawer) = &mut self.help_drawer else {
            return;
        };
        match drawer.tab {
            super::HelpTab::Commands => {
                drawer.command_selected = drawer.command_selected.saturating_sub(1);
            }
            super::HelpTab::Skills => {
                drawer.skill_selected = drawer.skill_selected.saturating_sub(1);
            }
            super::HelpTab::General => {
                drawer.general_selected = drawer.general_selected.saturating_sub(1);
            }
        }
    }

    pub fn help_page_down(&mut self, amount: usize) {
        for _ in 0..amount.max(1) {
            self.help_select_next();
        }
    }

    pub fn help_page_up(&mut self, amount: usize) {
        for _ in 0..amount.max(1) {
            self.help_select_prev();
        }
    }

    pub fn apply_event(&mut self, event: RuntimeToUiEvent) {
        match event {
            RuntimeToUiEvent::RunStarted => {
                self.show_start_screen = false;
                self.manual_compact_running = false;
                self.activity_status_title = None;
                self.pending_assistant = None;
                self.pending_proposed_plan = None;
                self.pending_compact_summary = None;
                self.clear_run_dividers();
                // 重连状态同步可能已校准活动计时器，避免被 replay 的 RunStarted 重置。
                if self.run_timer.is_none() {
                    self.start_run_timer();
                }
                self.agent_status = AgentStatus::Thinking;
            }
            RuntimeToUiEvent::UserMessageInjected {
                item,
                client_echo_id,
            } => {
                self.show_start_screen = false;
                let ui_message = ui_message_from_history_item(item);
                if self
                    .take_client_echo_positions(client_echo_id.as_deref())
                    .is_none()
                    && self.messages.last() != Some(&ui_message)
                {
                    self.messages.push(ui_message);
                }
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::TurnStarted => {
                // 如果上轮还有未提交的 pending_assistant，先推入 messages
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(UiMessage::Message(msg));
                }
                self.activity_status_title = None;
                self.agent_status = AgentStatus::Thinking;
            }
            RuntimeToUiEvent::ThinkingDelta(t) => {
                self.agent_status = AgentStatus::Thinking;
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                if let Some(ContentBlock::Thinking(tb)) = pending.content.last_mut() {
                    tb.thinking.push_str(&t);
                } else {
                    pending.content.push(ContentBlock::from_thinking(t));
                }
                if self.activity_status_title.is_none()
                    && let Some(title) = pending_activity_title(pending)
                {
                    self.activity_status_title = Some(title);
                }
            }
            RuntimeToUiEvent::TextDelta(t) => {
                self.activity_status_title = None;
                self.agent_status = AgentStatus::Working;
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                if let Some(ContentBlock::Text(tb)) = pending.content.last_mut() {
                    tb.text.push_str(&t);
                } else {
                    pending.content.push(ContentBlock::from_text(t));
                }
            }
            RuntimeToUiEvent::ProposedPlanDelta(t) => {
                self.activity_status_title = None;
                self.agent_status = AgentStatus::Working;
                self.pending_proposed_plan
                    .get_or_insert_with(String::new)
                    .push_str(&t);
            }
            RuntimeToUiEvent::ToolUse(tu) => {
                self.activity_status_title = None;
                self.running_tools.insert(tu.id.clone());
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                pending.content.push(ContentBlock::ToolUse(tu));
                self.agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::ToolResult(tr) => {
                let tool_use_id = tr.tool_use_id.clone();
                self.activity_status_title = None;
                self.finish_subagent_for_tool_result(&tr);
                self.running_tools.remove(&tr.tool_use_id);
                let removed_active = self.remove_tool_pause(&tr.tool_use_id);
                self.finish_tool_pause_removal(removed_active);
                // 工具结果异步返回，追加到 pending_assistant 或最后一条消息中
                if let Some(pending) = &mut self.pending_assistant {
                    pending.content.push(ContentBlock::ToolResult(tr));
                } else if let Some(last) = self
                    .messages
                    .iter_mut()
                    .rev()
                    .find_map(UiMessage::as_message_mut)
                {
                    last.content.push(ContentBlock::ToolResult(tr));
                    self.invalidate_completed_cache();
                } else {
                    let mut msg = Message::new(Role::Assistant, Vec::new());
                    msg.content.push(ContentBlock::ToolResult(tr));
                    self.messages.push(UiMessage::Message(msg));
                }
                self.on_tool_result(&tool_use_id);
            }
            RuntimeToUiEvent::TurnEnded => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    let msg_idx = self.messages.len();
                    self.messages.push(UiMessage::Message(msg));
                    self.populate_pending_tool_map_from_message(msg_idx);
                }
                let (pending_inputs, client_echo_id) = self.take_pending_intervention_ui_messages();
                if let Some(client_echo_id) = client_echo_id {
                    self.extend_optimistic_echoes(pending_inputs, client_echo_id);
                } else {
                    self.messages.extend(pending_inputs);
                }
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                self.activity_status_title = None;
                self.agent_status = AgentStatus::Working;
                self.update_live_boundary();
            }
            RuntimeToUiEvent::GitBranchChanged { branch } => {
                self.status_bar.git_branch = branch;
            }
            RuntimeToUiEvent::RunFinished => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    let msg_idx = self.messages.len();
                    self.messages.push(UiMessage::Message(msg));
                    self.populate_pending_tool_map_from_message(msg_idx);
                }
                if let Some(plan) = self.pending_proposed_plan.take()
                    && !plan.trim().is_empty()
                {
                    self.messages.push(UiMessage::ProposedPlan { text: plan });
                }
                if let Some(elapsed) = self.finish_run_timer() {
                    self.messages.push(UiMessage::RunDivider { elapsed });
                }
                self.pending_intervention_inputs.clear();
                self.pending_intervention_client_echo_id = None;
                self.pending_client_echoes.clear();
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                self.activity_status_title = None;
                self.refresh_input_placeholder();
                self.agent_status = AgentStatus::Idle;
                // Run 结束：将仍然 Running 的 subagent 标记为 Failed（安全网）。
                // 正常情况下 SubagentFinished 先于 RunFinished 到达，此处仅处理
                // 事件丢失的边界情况，避免 live_message_start 被永久压低。
                for node in self.subagents.values_mut() {
                    if matches!(node.status, crate::types::events::SubagentStatus::Running) {
                        node.status = crate::types::events::SubagentStatus::Failed;
                    }
                }
                self.update_live_boundary();
            }
            RuntimeToUiEvent::ToolPauseRequested(req) => {
                let should_prepare = self.push_tool_pause(req);
                if should_prepare {
                    self.prepare_active_tool_pause();
                }
                self.pause_run_timer();
                self.activity_status_title = None;
                self.agent_status = AgentStatus::AwaitingInput;
            }
            RuntimeToUiEvent::PlanSubmitted(plan) => {
                self.open_plan_approval(plan);
            }
            RuntimeToUiEvent::PlanApprovalResolved { plan_id, .. } => {
                self.clear_resolved_plan_approval(&plan_id);
            }
            RuntimeToUiEvent::SubagentStarted(event) => {
                self.subagents_by_tool_use
                    .insert(event.spawn_tool_use_id.clone(), event.session_id.clone());
                self.subagents.insert(
                    event.session_id.clone(),
                    SubagentNode {
                        session_id: event.session_id,
                        parent_session_id: event.parent_session_id,
                        spawn_tool_use_id: event.spawn_tool_use_id,
                        agent_label: event.agent_label,
                        status: SubagentStatus::Running,
                        messages: Vec::new(),
                    },
                );
                self.activity_status_title = None;
                self.agent_status = AgentStatus::Working;
                self.update_live_boundary();
            }
            RuntimeToUiEvent::SubagentMessageProduced(event) => {
                if let Some(node) = self.subagents.get_mut(&event.session_id) {
                    node.messages.push(event.message);
                }
            }
            RuntimeToUiEvent::SubagentToolUse(event) => {
                if let Some(node) = self.subagents.get_mut(&event.session_id) {
                    let msg =
                        Message::new(Role::Assistant, vec![ContentBlock::ToolUse(event.tool_use)]);
                    node.messages.push(msg);
                }
            }
            RuntimeToUiEvent::SubagentToolResult(event) => {
                self.activity_status_title = None;
                self.running_tools.remove(&event.tool_result.tool_use_id);
                let removed_active = self.remove_tool_pause(&event.tool_result.tool_use_id);
                let removed_active = self.remove_tool_pause(&format!(
                    "{}:{}",
                    event.session_id, event.tool_result.tool_use_id
                )) || removed_active;
                self.finish_tool_pause_removal(removed_active);
                if let Some(node) = self.subagents.get_mut(&event.session_id) {
                    let msg = Message::new(
                        Role::User,
                        vec![ContentBlock::ToolResult(event.tool_result)],
                    );
                    node.messages.push(msg);
                }
            }
            RuntimeToUiEvent::SubagentFinished(event) => {
                if let Some(node) = self.subagents.get_mut(&event.session_id) {
                    node.status = event.status;
                }
                self.update_live_boundary();
            }
            RuntimeToUiEvent::Notification(notification) => {
                match notification.kind {
                    NotificationKind::Info => {}
                    NotificationKind::Warn => {
                        self.finish_manual_compact();
                    }
                    NotificationKind::Error => {
                        self.finish_manual_compact();
                        if !self.pending_tool_pauses.is_empty() {
                            self.agent_status = AgentStatus::AwaitingInput;
                        } else if !self.is_run_active() {
                            self.agent_status = AgentStatus::Idle;
                        }
                    }
                }
                self.messages.push(UiMessage::Notification(notification));
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            // ===== 命令系统事件 =====
            RuntimeToUiEvent::Shutdown => {
                // TUI 主循环检测到此状态后会 break
            }
            RuntimeToUiEvent::ModelChanged {
                provider,
                model,
                thinking_effort,
                context_window,
            } => {
                self.status_bar.active_provider = provider;
                self.status_bar.model = model;
                self.status_bar.thinking_effort = thinking_effort;
                self.status_bar.context_window = context_window;
                // 模型切换成功后自动关闭选择弹窗
                self.interaction_step = None;
                self.interaction_request = None;
            }
            RuntimeToUiEvent::ThinkingDisplayChanged { show } => {
                self.show_thinking_blocks = show;
                self.invalidate_completed_cache();
            }
            RuntimeToUiEvent::UsageChanged(usage) => {
                self.status_bar.current_context_tokens = usage.current_context_tokens;
                self.status_bar.total_tokens = usage.total_tokens;
                self.status_bar.total_cached_tokens = usage.total_cached_tokens;
                self.status_bar.context_window = usage.context_window;
            }
            RuntimeToUiEvent::UsageTotalsChanged {
                total_tokens,
                total_cached_tokens,
            } => {
                self.status_bar.total_tokens = total_tokens;
                self.status_bar.total_cached_tokens = total_cached_tokens;
            }
            RuntimeToUiEvent::RuntimeStatusSynced { status } => {
                self.apply_runtime_status_sync(status);
            }
            RuntimeToUiEvent::CompactSummaryStarted(event) => {
                if event.trigger == CompactTrigger::Manual {
                    self.begin_manual_compact();
                }
                self.activity_status_title = None;
                self.pending_compact_summary = Some(String::new());
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryDelta(event) => {
                if event.trigger == CompactTrigger::Manual {
                    self.begin_manual_compact();
                }
                self.activity_status_title = None;
                self.pending_compact_summary
                    .get_or_insert_with(String::new)
                    .push_str(&event.delta);
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryFinished(event) => {
                let trigger = event.trigger;
                let summary = event.summary;
                // 优先使用 pending 中累积的流式文本；Finished 的 summary 为最终权威版本
                let final_text = if !summary.trim().is_empty() {
                    summary
                } else {
                    self.pending_compact_summary.take().unwrap_or_default()
                };
                self.pending_compact_summary = None;
                if !final_text.trim().is_empty() {
                    self.messages
                        .push(UiMessage::CompactSummary { text: final_text });
                    self.invalidate_completed_cache();
                }
                self.status_bar.current_context_tokens = event.after_tokens as i64;
                if trigger == CompactTrigger::Manual {
                    self.finish_manual_compact();
                }
                self.activity_status_title = None;
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryFailed(event) => {
                self.clear_pending_compact_summary();
                self.messages
                    .push(UiMessage::Notification(Notification::warning(
                        compact_summary_failed_text(
                            event.trigger,
                            event.agent_label.as_deref(),
                            &event.message,
                        ),
                    )));
                if event.trigger == CompactTrigger::Manual {
                    self.finish_manual_compact();
                }
                self.activity_status_title = None;
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::ActiveProfileChanged(profile) => {
                self.status_bar.active_profile = profile;
            }
            RuntimeToUiEvent::SessionTitleChanged { title } => {
                self.current_session_title = title.clone();
                // WebSocket 是 session-scoped 的，事件来源 session 就是
                // TUI 当前的 current_session_id；用它去同步两个 session
                // 列表缓存里对应条目的 title。
                let Some(current_session_id) = self.current_session_id.clone() else {
                    return;
                };
                if let Some(title) = title {
                    for session in self.startup_recent_sessions.iter_mut() {
                        if session.id == current_session_id {
                            session.title = title.clone();
                        }
                    }
                    if let Some(InteractionStep::Session {
                        sessions,
                        all_sessions,
                        ..
                    }) = self.interaction_step.as_mut()
                    {
                        for session in sessions.iter_mut() {
                            if session.id == current_session_id {
                                session.title = title.clone();
                            }
                        }
                        for session in all_sessions.iter_mut() {
                            if session.id == current_session_id {
                                session.title = title.clone();
                            }
                        }
                    }
                }
            }
            RuntimeToUiEvent::InteractionRequest(req) => {
                self.interaction_request = Some(req);
            }
            RuntimeToUiEvent::ShowHelpDrawer(commands) => {
                self.open_help_drawer(commands);
            }
            RuntimeToUiEvent::CommandList(cmds) => {
                self.autocomplete.all_commands = crate::command::commands_with_runtime_skills(cmds);
            }
            RuntimeToUiEvent::AgentManagementUpdated { records } => {
                self.mention_autocomplete
                    .set_candidates(agent_summaries_to_mention_candidates(
                        records
                            .iter()
                            .map(|record| AgentSummary {
                                name: record.name.clone(),
                                description: record.description.clone(),
                                location: record
                                    .path
                                    .as_ref()
                                    .map(|path| path.display().to_string())
                                    .unwrap_or_else(|| "<built-in>".to_string()),
                            })
                            .collect(),
                    ));
                self.update_input_autocomplete();
                if let Some(InteractionStep::Agents(manager)) = &mut self.interaction_step {
                    let keep_view = matches!(
                        manager.view,
                        AgentManagerView::EditMenu
                            | AgentManagerView::EditMetadata
                            | AgentManagerView::EditTools
                            | AgentManagerView::EditModel
                    );
                    manager.refresh_records(records);
                    if !keep_view {
                        manager.view = AgentManagerView::List;
                    }
                }
            }
            RuntimeToUiEvent::AgentGenerated { source_kind, draft } => {
                if let Some(InteractionStep::Agents(manager)) = &mut self.interaction_step {
                    manager.apply_generated(source_kind, draft);
                }
            }
            RuntimeToUiEvent::AgentGenerateFailed { message } => {
                if let Some(InteractionStep::Agents(manager)) = &mut self.interaction_step {
                    manager.fail_generation(message);
                } else {
                    self.messages
                        .push(UiMessage::Notification(Notification::info(message)));
                }
            }
            // SessionSnapshot 由 TUI 主循环直接处理，此处无需匹配
            RuntimeToUiEvent::SessionSnapshot { .. } => {}
        }
    }

    fn finish_subagent_for_tool_result(&mut self, result: &ToolResultBlock) {
        let Some(session_id) = self.subagents_by_tool_use.get(&result.tool_use_id) else {
            return;
        };
        let Some(node) = self.subagents.get_mut(session_id) else {
            return;
        };
        if node.status != SubagentStatus::Running {
            return;
        }

        node.status = if result.is_error {
            if result.content.trim() == "Execution cancelled" {
                SubagentStatus::Cancelled
            } else {
                SubagentStatus::Failed
            }
        } else {
            SubagentStatus::Completed
        };
    }

    pub fn apply_session_snapshot(
        &mut self,
        session_id: Option<String>,
        messages: Vec<HistoryItem>,
        subagents: Vec<SubagentSnapshot>,
        usage: crate::types::events::SessionUsageSnapshot,
    ) {
        self.show_start_screen = false;
        self.current_session_id = session_id;
        if self.current_session_id.is_none() {
            // 「新建会话」前的 blank 状态：把 title 缓存也清空,避免残留上一个 session 的 title。
            self.current_session_title = None;
        }
        self.messages = UiMessage::from_history_items(messages);
        self.pending_client_echoes.clear();
        self.invalidate_completed_cache();
        self.status_bar.current_context_tokens = usage.current_context_tokens;
        self.status_bar.total_tokens = usage.total_tokens;
        self.status_bar.total_cached_tokens = usage.total_cached_tokens;
        self.status_bar.context_window = usage.context_window;
        self.subagents.clear();
        self.subagents_by_tool_use.clear();
        for subagent in subagents {
            let node = SubagentNode::from(subagent);
            self.subagents_by_tool_use
                .insert(node.spawn_tool_use_id.clone(), node.session_id.clone());
            self.subagents.insert(node.session_id.clone(), node);
        }
        self.pending_assistant = None;
        self.pending_proposed_plan = None;
        self.pending_compact_summary = None;
        self.pending_intervention_client_echo_id = None;
        self.activity_status_title = None;
        self.run_timer = None;
        self.manual_compact_running = false;
        self.queued_user_inputs.clear();
        self.input.clear();
        self.input_mentions.clear();
        self.input_images.clear();
        self.input_paste_markers.clear();
        self.cursor_char = 0;
        self.input_scroll_line = 0;
        self.agent_status = AgentStatus::Idle;
        self.interaction_step = None;
        self.interaction_request = None;
        self.help_drawer = None;
        self.clear_plan_approval();
        self.scroll_to_bottom();
        self.rebuild_pending_tool_map();
    }
}

const ACTIVITY_STATUS_TITLE_MAX_CHARS: usize = 48;

fn pending_activity_title(message: &Message) -> Option<String> {
    message.content.iter().find_map(|block| {
        if let ContentBlock::Thinking(thinking) = block {
            extract_first_bold_title(&thinking.thinking)
        } else {
            None
        }
    })
}

fn extract_first_bold_title(text: &str) -> Option<String> {
    // 只在第一句中搜索加粗标题
    let first_sentence = text
        .find(['。', '.', '？', '?', '！', '!', '\n'])
        .map(|end| &text[..end])
        .unwrap_or(text);

    let bytes = first_sentence.as_bytes();
    let mut start = None;
    let mut idx = 0;

    while idx + 1 < bytes.len() {
        if bytes[idx] == b'*' && bytes[idx + 1] == b'*' {
            if let Some(start_idx) = start {
                return normalize_activity_title(&first_sentence[start_idx..idx]);
            }
            start = Some(idx + 2);
            idx += 2;
            continue;
        }
        idx += 1;
    }

    None
}

fn normalize_activity_title(title: &str) -> Option<String> {
    let normalized = title.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }

    let char_count = normalized.chars().count();
    if char_count <= ACTIVITY_STATUS_TITLE_MAX_CHARS {
        return Some(normalized);
    }

    let mut truncated = normalized
        .chars()
        .take(ACTIVITY_STATUS_TITLE_MAX_CHARS.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    Some(truncated)
}

fn compact_summary_failed_text(
    trigger: crate::types::events::CompactTrigger,
    agent_label: Option<&str>,
    message: &str,
) -> String {
    let subject = agent_label
        .map(|label| format!("subagent {label}"))
        .unwrap_or_else(|| "session".to_string());
    format!("Failed to summarize compacted {subject} context ({trigger}): {message}")
}

fn command_count(commands: &[CommandSummary], kind: CommandKind) -> usize {
    commands
        .iter()
        .filter(|command| command.kind == kind)
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::config::{ModelConfig, ProviderProfile, ProviderType};
    use crate::types::events::{
        CompactEvent, CompactSummaryDeltaEvent, CompactSummaryFailedEvent,
        CompactSummaryFinishedEvent, CompactTrigger,
    };
    use omini_domain::display::{DisplayMention, DisplayMessage, MentionKind};
    use std::collections::HashMap;

    fn model_selection_request() -> InteractionRequest {
        InteractionRequest::ModelSelection {
            providers: HashMap::from([(
                "openai".to_string(),
                ProviderProfile {
                    name: "OpenAI".to_string(),
                    endpoint: ProviderType::OpenAI,
                    base_url: "https://openai.example".to_string(),
                    models: vec![ModelConfig {
                        id: "reasoner".to_string(),
                        name: None,
                        limit: 1000,
                        thinking: true,
                        input_modalities: None,
                        extra_body: None,
                        extra_headers: None,
                    }],
                },
            )]),
            current_provider: "openai".to_string(),
            current_model: "reasoner".to_string(),
        }
    }

    fn subagent_display_message(description: &str) -> DisplayMessage {
        DisplayMessage {
            role: Role::User,
            text: "@code-reviewer review this".to_string(),
            mentions: vec![DisplayMention {
                start_char: 0,
                end_char: 14,
                kind: MentionKind::Subagent,
                label: "code-reviewer".to_string(),
                target: "code-reviewer".to_string(),
                description: description.to_string(),
            }],
        }
    }

    #[test]
    fn model_selection_defaults_missing_thinking_effort_to_medium() {
        let mut state = UiState::new();
        state.status_bar.thinking_effort = None;

        state.open_interaction_request(&model_selection_request());

        let Some(InteractionStep::ModelSelection { thinking_idx, .. }) = state.interaction_step
        else {
            panic!("expected model selection interaction");
        };
        assert_eq!(thinking_idx, 2);
    }

    #[test]
    fn model_selection_preserves_explicit_no_thinking_effort() {
        let mut state = UiState::new();
        state.status_bar.thinking_effort = Some(ThinkingEffort::None);

        state.open_interaction_request(&model_selection_request());

        let Some(InteractionStep::ModelSelection { thinking_idx, .. }) = state.interaction_step
        else {
            panic!("expected model selection interaction");
        };
        assert_eq!(thinking_idx, 0);
    }

    #[test]
    fn model_selection_preserves_max_thinking_effort() {
        let mut state = UiState::new();
        state.status_bar.thinking_effort = Some(ThinkingEffort::Max);

        state.open_interaction_request(&model_selection_request());

        let Some(InteractionStep::ModelSelection { thinking_idx, .. }) = state.interaction_step
        else {
            panic!("expected model selection interaction");
        };
        assert_eq!(thinking_idx, 5);
    }

    #[test]
    fn usage_totals_changed_preserves_current_context_usage() {
        let mut state = UiState::new();
        state.status_bar.current_context_tokens = 123;
        state.status_bar.context_window = Some(456);

        state.apply_event(RuntimeToUiEvent::UsageTotalsChanged {
            total_tokens: 789,
            total_cached_tokens: 12,
        });

        assert_eq!(state.status_bar.current_context_tokens, 123);
        assert_eq!(state.status_bar.context_window, Some(456));
        assert_eq!(state.status_bar.total_tokens, 789);
        assert_eq!(state.status_bar.total_cached_tokens, 12);
    }

    #[test]
    fn thinking_display_changed_updates_ui_state() {
        let mut state = UiState::new();

        state.apply_event(RuntimeToUiEvent::ThinkingDisplayChanged { show: false });

        assert!(!state.show_thinking_blocks);

        state.apply_event(RuntimeToUiEvent::ThinkingDisplayChanged { show: true });

        assert!(state.show_thinking_blocks);
    }

    #[test]
    fn user_message_injected_does_not_duplicate_optimistic_echo() {
        let mut state = UiState::new();
        let message = Message::from_user_text("hello".to_string());
        state.messages.push(UiMessage::Message(message.clone()));

        state.apply_event(RuntimeToUiEvent::UserMessageInjected {
            item: HistoryItem::Message(message),
            client_echo_id: None,
        });

        assert_eq!(state.messages.len(), 1);
    }

    #[test]
    fn user_message_injected_uses_client_echo_id_for_display_metadata_differences() {
        let mut state = UiState::new();
        let local = subagent_display_message("Review code changes");
        let runtime = subagent_display_message("subagent");

        state.push_optimistic_echo(UiMessage::Display(local.clone()), "echo-1".to_string());
        state.apply_event(RuntimeToUiEvent::UserMessageInjected {
            item: HistoryItem::Display(runtime),
            client_echo_id: Some("echo-1".to_string()),
        });

        assert_eq!(state.messages, vec![UiMessage::Display(local)]);
        assert!(state.pending_client_echoes.is_empty());
    }

    #[test]
    fn user_message_injected_without_client_echo_id_keeps_compatible_dedup() {
        let mut state = UiState::new();
        let local = subagent_display_message("Review code changes");
        let runtime = subagent_display_message("subagent");

        state.messages.push(UiMessage::Display(local));
        state.apply_event(RuntimeToUiEvent::UserMessageInjected {
            item: HistoryItem::Display(runtime),
            client_echo_id: None,
        });

        assert_eq!(state.messages.len(), 2);
    }

    #[test]
    fn user_message_injected_with_unmatched_client_echo_id_appends_for_observers() {
        let mut state = UiState::new();
        let runtime = subagent_display_message("subagent");

        state.apply_event(RuntimeToUiEvent::UserMessageInjected {
            item: HistoryItem::Display(runtime.clone()),
            client_echo_id: Some("echo-1".to_string()),
        });

        assert_eq!(state.messages, vec![UiMessage::Display(runtime)]);
    }

    #[test]
    fn extracts_activity_title_from_first_sentence_bold_text() {
        assert_eq!(
            extract_first_bold_title("先看看 **分析代码结构** 再行动").as_deref(),
            Some("分析代码结构")
        );
        // 换行符截断第一句，跨行加粗不再匹配
        assert_eq!(extract_first_bold_title("** 分析   当前\n改动 **"), None);
        assert_eq!(extract_first_bold_title("还没闭合 **分析代码"), None);
        assert_eq!(extract_first_bold_title("空标题 **** 后面"), None);
        assert_eq!(
            extract_first_bold_title("**第一步** 然后 **第二步**").as_deref(),
            Some("第一步")
        );
        // 第一句有句号时，只在第一句内搜索
        assert_eq!(
            extract_first_bold_title("让我先 **分析代码结构**。然后 **执行修改**。").as_deref(),
            Some("分析代码结构")
        );
        // 加粗在第二句时不匹配
        assert_eq!(
            extract_first_bold_title("让我先分析代码结构。然后 **执行修改**。"),
            None
        );
        // 句号分隔：第一句内无加粗
        assert_eq!(
            extract_first_bold_title("先想一下。接下来 **分析代码结构**。"),
            None
        );
    }

    #[test]
    fn thinking_delta_sets_activity_title_after_bold_closes() {
        let mut state = UiState::new();

        state.apply_event(RuntimeToUiEvent::RunStarted);
        state.apply_event(RuntimeToUiEvent::ThinkingDelta("**分析".to_string()));

        assert_eq!(state.activity_status_title, None);

        state.apply_event(RuntimeToUiEvent::ThinkingDelta("代码结构**".to_string()));

        assert_eq!(state.activity_status_title.as_deref(), Some("分析代码结构"));
    }

    #[test]
    fn working_events_clear_activity_title() {
        let mut state = UiState::new();

        state.apply_event(RuntimeToUiEvent::RunStarted);
        state.apply_event(RuntimeToUiEvent::ThinkingDelta(
            "**分析代码结构**".to_string(),
        ));
        assert_eq!(state.activity_status_title.as_deref(), Some("分析代码结构"));

        state.apply_event(RuntimeToUiEvent::TextDelta("开始处理".to_string()));

        assert_eq!(state.activity_status_title, None);
        assert_eq!(state.agent_status, AgentStatus::Working);
    }

    #[test]
    fn run_started_clears_previous_activity_title() {
        let mut state = UiState::new();
        state.activity_status_title = Some("旧标题".to_string());

        state.apply_event(RuntimeToUiEvent::RunStarted);

        assert_eq!(state.activity_status_title, None);
    }

    #[test]
    fn compact_summary_delta_streams_into_single_ui_message() {
        let mut state = UiState::new();

        state.apply_event(RuntimeToUiEvent::CompactSummaryDelta(
            CompactSummaryDeltaEvent {
                trigger: CompactTrigger::Manual,
                delta: "first ".to_string(),
                session_id: Some("session".to_string()),
                agent_label: None,
            },
        ));
        state.apply_event(RuntimeToUiEvent::CompactSummaryDelta(
            CompactSummaryDeltaEvent {
                trigger: CompactTrigger::Manual,
                delta: "second".to_string(),
                session_id: Some("session".to_string()),
                agent_label: None,
            },
        ));

        let Some(text) = &state.pending_compact_summary else {
            panic!("expected pending compact summary");
        };
        assert!(text.contains("first second"));
    }

    #[test]
    fn compact_summary_started_creates_new_summary_message() {
        let mut state = UiState::new();
        state.messages.push(UiMessage::CompactSummary {
            text: "previous".to_string(),
        });

        state.apply_event(RuntimeToUiEvent::CompactSummaryStarted(CompactEvent {
            trigger: CompactTrigger::Manual,
            session_id: Some("session".to_string()),
            agent_label: None,
        }));
        state.apply_event(RuntimeToUiEvent::CompactSummaryDelta(
            CompactSummaryDeltaEvent {
                trigger: CompactTrigger::Manual,
                delta: "new".to_string(),
                session_id: Some("session".to_string()),
                agent_label: None,
            },
        ));

        // 历史摘要保留在 messages 中，新的流式内容在 pending 中
        assert_eq!(state.messages.len(), 1);
        assert_eq!(state.pending_compact_summary.as_deref(), Some("new"));
    }

    #[test]
    fn compact_summary_finished_replaces_streamed_text_and_updates_context_tokens() {
        let mut state = UiState::new();
        state.pending_compact_summary = Some("partial".to_string());

        state.apply_event(RuntimeToUiEvent::CompactSummaryFinished(
            CompactSummaryFinishedEvent {
                trigger: CompactTrigger::Manual,
                summary: "final summary".to_string(),
                after_tokens: 250,
                session_id: Some("session".to_string()),
                agent_label: None,
            },
        ));

        assert_eq!(state.status_bar.current_context_tokens, 250);
        let Some(UiMessage::CompactSummary { text }) = state.messages.last() else {
            panic!("expected compact summary message");
        };
        assert_eq!(text, "final summary");
    }

    #[test]
    fn manual_compact_summary_lifecycle_returns_status_to_idle() {
        let mut state = UiState::new();

        state.apply_event(RuntimeToUiEvent::CompactSummaryStarted(CompactEvent {
            trigger: CompactTrigger::Manual,
            session_id: Some("session".to_string()),
            agent_label: None,
        }));

        assert_eq!(state.agent_status, AgentStatus::Working);
        assert!(state.manual_compact_running);
        assert!(state.run_timer.is_some());

        state.apply_event(RuntimeToUiEvent::CompactSummaryFinished(
            CompactSummaryFinishedEvent {
                trigger: CompactTrigger::Manual,
                summary: "final summary".to_string(),
                after_tokens: 250,
                session_id: Some("session".to_string()),
                agent_label: None,
            },
        ));

        assert_eq!(state.agent_status, AgentStatus::Idle);
        assert!(!state.manual_compact_running);
        assert!(state.run_timer.is_none());
    }

    #[test]
    fn empty_compact_summary_finished_removes_loading_placeholder() {
        let mut state = UiState::new();
        // 模拟 Started 事件创建的流式占位
        state.pending_compact_summary = Some(String::new());

        state.apply_event(RuntimeToUiEvent::CompactSummaryFinished(
            CompactSummaryFinishedEvent {
                trigger: CompactTrigger::Manual,
                summary: String::new(),
                after_tokens: 250,
                session_id: Some("session".to_string()),
                agent_label: None,
            },
        ));

        assert!(state.messages.is_empty());
        assert!(state.pending_compact_summary.is_none());
    }

    #[test]
    fn auto_compact_summary_finished_does_not_force_idle() {
        let mut state = UiState::new();
        state.agent_status = AgentStatus::Working;
        state.pending_compact_summary = Some("partial".to_string());

        state.apply_event(RuntimeToUiEvent::CompactSummaryFinished(
            CompactSummaryFinishedEvent {
                trigger: CompactTrigger::Auto,
                summary: "final summary".to_string(),
                after_tokens: 250,
                session_id: Some("session".to_string()),
                agent_label: None,
            },
        ));

        assert_eq!(state.agent_status, AgentStatus::Working);
    }

    #[test]
    fn manual_compact_summary_failed_clears_empty_placeholder_and_status() {
        let mut state = UiState::new();
        state.apply_event(RuntimeToUiEvent::CompactSummaryStarted(CompactEvent {
            trigger: CompactTrigger::Manual,
            session_id: Some("session".to_string()),
            agent_label: None,
        }));

        state.apply_event(RuntimeToUiEvent::CompactSummaryFailed(
            CompactSummaryFailedEvent {
                trigger: CompactTrigger::Manual,
                message: "nope".to_string(),
                session_id: Some("session".to_string()),
                agent_label: None,
            },
        ));

        assert_eq!(state.agent_status, AgentStatus::Idle);
        assert!(!state.manual_compact_running);
        assert!(state.run_timer.is_none());
        assert!(matches!(
            state.messages.as_slice(),
            [UiMessage::Notification(notification)]
                if notification.kind == NotificationKind::Warn
        ));
    }

    #[test]
    fn manual_compact_warning_returns_status_to_idle() {
        let mut state = UiState::new();
        state.begin_manual_compact();

        state.apply_event(RuntimeToUiEvent::warning(
            "没有可压缩的会话历史".to_string(),
        ));

        assert_eq!(state.agent_status, AgentStatus::Idle);
        assert!(!state.manual_compact_running);
        assert!(state.run_timer.is_none());
        assert!(matches!(
            state.messages.as_slice(),
            [UiMessage::Notification(notification)]
                if notification.kind == NotificationKind::Warn
        ));
    }

    #[test]
    fn manual_compact_error_returns_status_to_idle() {
        let mut state = UiState::new();
        state.begin_manual_compact();

        state.apply_event(RuntimeToUiEvent::error(
            "This client is not connected to the session event stream".to_string(),
        ));

        assert_eq!(state.agent_status, AgentStatus::Idle);
        assert!(!state.manual_compact_running);
        assert!(state.run_timer.is_none());
        assert!(matches!(
            state.messages.as_slice(),
            [UiMessage::Notification(notification)]
                if notification.kind == NotificationKind::Error
        ));
    }

    #[test]
    fn error_notification_does_not_fail_running_subagents() {
        use crate::state::SubagentNode;

        let mut state = UiState::new();
        state.subagents.insert(
            "sub-1".to_string(),
            SubagentNode {
                session_id: "sub-1".to_string(),
                parent_session_id: "main".to_string(),
                spawn_tool_use_id: "tool-1".to_string(),
                agent_label: "worker".to_string(),
                status: SubagentStatus::Running,
                messages: Vec::new(),
            },
        );

        state.apply_event(RuntimeToUiEvent::error(
            "Cannot handle this request while a run is active".to_string(),
        ));

        let sub = state.subagents.get("sub-1").unwrap();
        assert_eq!(sub.status, SubagentStatus::Running);
    }
}
