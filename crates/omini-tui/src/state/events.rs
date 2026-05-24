use super::input::combined_user_draft;
use super::{
    AgentManagerState, AgentManagerView, AgentStatus, InteractionStep, ModelSelectionEntry,
    SubagentNode, UiMessage, UiState, mention,
};
use crate::types::config::ThinkingEffort;
use crate::types::display::{HistoryItem, UserDraft};
use crate::types::events::{
    CommandKind, CommandSummary, CompactTrigger, InteractionRequest, RuntimeToUiEvent,
    SubagentSnapshot, SubagentStatus,
};
use crate::types::message::{ContentBlock, Message, Role, ToolResultBlock};
use std::collections::VecDeque;

const GENERAL_HELP_SELECTABLE_COUNT: usize = 9;

impl UiState {
    pub fn is_run_active(&self) -> bool {
        matches!(
            self.agent_status,
            AgentStatus::Working | AgentStatus::Thinking | AgentStatus::AwaitingInput
        )
    }

    fn remove_empty_compact_summary_placeholder(&mut self) {
        if matches!(
            self.messages.last(),
            Some(UiMessage::CompactSummary { text }) if text.trim().is_empty()
        ) {
            self.messages.pop();
        }
    }

    pub fn take_queued_user_draft(&mut self) -> Option<UserDraft> {
        Self::draft_from_inputs(&mut self.queued_user_inputs)
    }

    pub fn take_queued_user_draft_for_intervention(&mut self) -> Option<UserDraft> {
        if !self.pending_intervention_inputs.is_empty() {
            return None;
        }

        let pending = self.queued_user_inputs.drain(..).collect::<VecDeque<_>>();
        let draft = Self::draft_from_input_iter(pending.iter())?;
        self.pending_intervention_inputs = pending;
        Some(draft)
    }

    fn take_pending_intervention_ui_messages(&mut self) -> Vec<UiMessage> {
        self.pending_intervention_inputs
            .drain(..)
            .map(|draft| match draft.history_item() {
                HistoryItem::Message(message) => UiMessage::Message(message),
                HistoryItem::Display(display) => UiMessage::Display(display),
                HistoryItem::Plan(plan) => UiMessage::ProposedPlan {
                    text: plan.markdown,
                },
                HistoryItem::Summary(summary) => UiMessage::CompactSummary {
                    text: summary.markdown,
                },
            })
            .collect()
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
                    Some(ThinkingEffort::None) | None => 0,
                };
                let mut sorted: Vec<_> = providers.clone().into_iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                for (provider_key, profile) in &sorted {
                    entries.push(ModelSelectionEntry::ProviderHeader {
                        name: profile.name.clone(),
                    });
                    for model in &profile.models {
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
                sorted.sort_by(|a, b| b.created_at.cmp(&a.created_at));
                let all_sessions = sorted.clone();
                Some(InteractionStep::Session {
                    sessions: sorted,
                    all_sessions,
                    search: String::new(),
                    selected: 0,
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
                self.manual_compact_running = false;
                self.pending_assistant = None;
                self.pending_proposed_plan = None;
                self.clear_run_dividers();
                self.start_run_timer();
                self.agent_status = AgentStatus::Thinking;
            }
            RuntimeToUiEvent::UserMessageInjected(item) => {
                let ui_message = match item {
                    HistoryItem::Message(message) => UiMessage::Message(message),
                    HistoryItem::Display(display) => UiMessage::Display(display),
                    HistoryItem::Plan(plan) => UiMessage::ProposedPlan {
                        text: plan.markdown,
                    },
                    HistoryItem::Summary(summary) => UiMessage::CompactSummary {
                        text: summary.markdown,
                    },
                };
                self.messages.push(ui_message);
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
            }
            RuntimeToUiEvent::TextDelta(t) => {
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
                self.agent_status = AgentStatus::Working;
                self.pending_proposed_plan
                    .get_or_insert_with(String::new)
                    .push_str(&t);
            }
            RuntimeToUiEvent::ToolUse(tu) => {
                self.running_tools.insert(tu.id.clone());
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                pending.content.push(ContentBlock::ToolUse(tu));
                self.agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::ToolResult(tr) => {
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
                } else {
                    let mut msg = Message::new(Role::Assistant, Vec::new());
                    msg.content.push(ContentBlock::ToolResult(tr));
                    self.messages.push(UiMessage::Message(msg));
                }
            }
            RuntimeToUiEvent::TurnEnded => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(UiMessage::Message(msg));
                }
                let pending_inputs = self.take_pending_intervention_ui_messages();
                self.messages.extend(pending_inputs);
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                self.agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::RunFinished => {
                if let Some(msg) = self.pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.messages.push(UiMessage::Message(msg));
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
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                self.agent_status = AgentStatus::Idle;
            }
            RuntimeToUiEvent::ToolPauseRequested(req) => {
                let should_prepare = self.push_tool_pause(req);
                if should_prepare {
                    self.prepare_active_tool_pause();
                }
                self.pause_run_timer();
                self.agent_status = AgentStatus::AwaitingInput;
            }
            RuntimeToUiEvent::PlanSubmitted(plan) => {
                self.plan_approval = Some(plan);
                self.plan_approval_selected = 0;
                self.plan_approval_auto = false;
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
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
                self.agent_status = AgentStatus::Working;
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
            }
            RuntimeToUiEvent::Error(e) => {
                self.messages.push(UiMessage::Error { text: e });
                self.fail_running_subagents();
                if !self.pending_tool_pauses.is_empty() {
                    self.agent_status = AgentStatus::AwaitingInput;
                } else if !self.is_run_active() {
                    self.agent_status = AgentStatus::Idle;
                }
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::Warning(text) => {
                self.messages.push(UiMessage::Warning { text });
                self.finish_manual_compact();
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            // ===== 命令系统事件 =====
            RuntimeToUiEvent::Shutdown => {
                // TUI 主循环检测到此状态后会 break
            }
            RuntimeToUiEvent::CommandNotice(text) => {
                self.messages.push(UiMessage::Notice { text });
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
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
            RuntimeToUiEvent::CompactSummaryStarted(event) => {
                if event.trigger == CompactTrigger::Manual {
                    self.begin_manual_compact();
                }
                self.messages.push(UiMessage::CompactSummary {
                    text: String::new(),
                });
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryDelta(event) => {
                if event.trigger == CompactTrigger::Manual {
                    self.begin_manual_compact();
                }
                if let Some(UiMessage::CompactSummary { text }) = self.messages.last_mut() {
                    text.push_str(&event.delta);
                } else {
                    self.messages
                        .push(UiMessage::CompactSummary { text: event.delta });
                }
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryFinished(event) => {
                let trigger = event.trigger;
                let summary = event.summary;
                if summary.trim().is_empty() {
                    self.remove_empty_compact_summary_placeholder();
                } else if let Some(UiMessage::CompactSummary { text }) = self.messages.last_mut() {
                    *text = summary;
                } else {
                    self.messages
                        .push(UiMessage::CompactSummary { text: summary });
                }
                self.status_bar.current_context_tokens = event.after_tokens as i64;
                if trigger == CompactTrigger::Manual {
                    self.finish_manual_compact();
                }
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryFailed(event) => {
                self.remove_empty_compact_summary_placeholder();
                self.messages.push(UiMessage::Warning {
                    text: compact_summary_failed_text(
                        event.trigger,
                        event.agent_label.as_deref(),
                        &event.message,
                    ),
                });
                if event.trigger == CompactTrigger::Manual {
                    self.finish_manual_compact();
                }
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::ActiveProfileChanged(profile) => {
                if self.status_bar.active_profile != profile {
                    self.status_bar.plan_mode_message_sent = false;
                }
                self.status_bar.active_profile = profile;
            }
            RuntimeToUiEvent::SessionTitleChanged { title } => {
                self.current_session_title = title;
            }
            RuntimeToUiEvent::InteractionRequest(req) => {
                self.interaction_request = Some(req);
            }
            RuntimeToUiEvent::ShowHelpDrawer(commands) => {
                self.open_help_drawer(commands);
            }
            RuntimeToUiEvent::CommandList(cmds) => {
                self.autocomplete.all_commands = cmds;
            }
            RuntimeToUiEvent::AgentList(agents) => {
                self.mention_autocomplete
                    .set_candidates(mention::agent_summaries_to_mention_candidates(agents));
                self.update_input_autocomplete();
            }
            RuntimeToUiEvent::AgentManagementUpdated { records } => {
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
                    self.messages.push(UiMessage::Notice { text: message });
                }
            }
            // SessionChanged 由 TUI 主循环直接处理，此处无需匹配
            RuntimeToUiEvent::SessionChanged { .. } => {}
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

    fn fail_running_subagents(&mut self) {
        for node in self.subagents.values_mut() {
            if node.status == SubagentStatus::Running {
                node.status = SubagentStatus::Failed;
            }
        }
    }

    pub fn apply_session_changed(
        &mut self,
        session_id: Option<String>,
        messages: Vec<HistoryItem>,
        subagents: Vec<SubagentSnapshot>,
        usage: crate::types::events::SessionUsageSnapshot,
    ) {
        self.current_session_id = session_id;
        self.messages = UiMessage::from_history_items(messages);
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
        self.plan_approval = None;
        self.plan_approval_selected = 0;
        self.plan_approval_auto = false;
        self.scroll_to_bottom();
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::events::{
        ActiveProfile, CompactEvent, CompactSummaryDeltaEvent, CompactSummaryFailedEvent,
        CompactSummaryFinishedEvent, CompactTrigger,
    };

    #[test]
    fn entering_plan_mode_resets_plan_message_hint_state() {
        let mut state = UiState::new();
        state.status_bar.active_profile = ActiveProfile::Main;
        state.status_bar.plan_mode_message_sent = true;

        state.apply_event(RuntimeToUiEvent::ActiveProfileChanged(ActiveProfile::Plan));

        assert_eq!(state.status_bar.active_profile, ActiveProfile::Plan);
        assert!(!state.status_bar.plan_mode_message_sent);
    }

    #[test]
    fn plan_message_sent_is_recorded_only_in_plan_mode() {
        let mut state = UiState::new();
        state.mark_plan_mode_message_sent();
        assert!(!state.status_bar.plan_mode_message_sent);

        state.apply_event(RuntimeToUiEvent::ActiveProfileChanged(ActiveProfile::Plan));
        state.mark_plan_mode_message_sent();
        assert!(state.status_bar.plan_mode_message_sent);
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

        assert_eq!(state.messages.len(), 1);
        let Some(UiMessage::CompactSummary { text }) = state.messages.first() else {
            panic!("expected compact summary message");
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

        assert_eq!(state.messages.len(), 2);
        let Some(UiMessage::CompactSummary { text }) = state.messages.last() else {
            panic!("expected compact summary message");
        };
        assert_eq!(text, "new");
    }

    #[test]
    fn compact_summary_finished_replaces_streamed_text_and_updates_context_tokens() {
        let mut state = UiState::new();
        state.messages.push(UiMessage::CompactSummary {
            text: "partial".to_string(),
        });

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
        state.messages.push(UiMessage::CompactSummary {
            text: String::new(),
        });

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
    }

    #[test]
    fn auto_compact_summary_finished_does_not_force_idle() {
        let mut state = UiState::new();
        state.agent_status = AgentStatus::Working;
        state.messages.push(UiMessage::CompactSummary {
            text: "partial".to_string(),
        });

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
            [UiMessage::Warning { .. }]
        ));
    }

    #[test]
    fn manual_compact_warning_returns_status_to_idle() {
        let mut state = UiState::new();
        state.begin_manual_compact();

        state.apply_event(RuntimeToUiEvent::Warning(
            "没有可压缩的会话历史".to_string(),
        ));

        assert_eq!(state.agent_status, AgentStatus::Idle);
        assert!(!state.manual_compact_running);
        assert!(state.run_timer.is_none());
        assert!(matches!(
            state.messages.as_slice(),
            [UiMessage::Warning { .. }]
        ));
    }
}

fn command_count(commands: &[CommandSummary], kind: CommandKind) -> usize {
    commands
        .iter()
        .filter(|command| command.kind == kind)
        .count()
}
