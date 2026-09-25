use super::input::combined_user_draft;
use super::{
    AgentManagerState, AgentManagerView, AgentStatus, InteractionStep, ModelSelectionEntry,
    SubagentNode, UiMessage, UiState, agent_summaries_to_mention_candidates,
};
use crate::types::config::ThinkingEffort;
use crate::types::events::{
    AgentTaskEvent, AgentTaskExecutionMode, AgentTaskSnapshot, AgentTaskStatus, CommandKind,
    CommandSummary, CompactTrigger, InteractionRequest, Notification, NotificationKind,
    RuntimeToUiEvent,
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
        HistoryItem::UserInput(input) => UiMessage::Display(input.display_message()),
        HistoryItem::Plan(plan) => UiMessage::ProposedPlan {
            text: plan.markdown,
        },
        HistoryItem::Summary(summary) => UiMessage::CompactSummary {
            text: summary.markdown,
        },
        HistoryItem::AgentTaskNotification(notification) => {
            UiMessage::AgentTaskNotification(notification)
        }
    }
}

impl UiState {
    pub fn is_run_active(&self) -> bool {
        matches!(
            self.agent_status,
            AgentStatus::Working | AgentStatus::Thinking | AgentStatus::AwaitingInput
        )
    }

    pub fn is_main_query_active(&self) -> bool {
        self.main_query_active
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

        let pending = std::mem::take(&mut self.queued_user_inputs);
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
            InteractionRequest::ThreadSelection { threads } => {
                let mut sorted = threads.clone();
                sorted.sort_by_key(|item| std::cmp::Reverse(item.updated_at));
                let all_threads = sorted.clone();
                let selected = self
                    .current_thread_id
                    .as_ref()
                    .and_then(|id| sorted.iter().position(|s| s.id == *id))
                    .unwrap_or(0);
                Some(InteractionStep::Thread {
                    threads: sorted,
                    all_threads,
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
                self.main_query_active = true;
                self.show_start_screen = false;
                self.manual_compact_running = false;
                self.pending_assistant = None;
                self.pending_proposed_plan = None;
                self.pending_compact_summary = None;
                self.thinking_started_at = None;
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
                // 上轮残留的思考计时先结算，再提交 pending_assistant
                self.settle_active_thinking_segment();
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
                // 首个 delta 开启本地计时；结束由 text/tool/turn 结算
                if self.thinking_started_at.is_none() {
                    self.thinking_started_at = Some(std::time::Instant::now());
                }
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
                self.settle_active_thinking_segment();
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
                self.settle_active_thinking_segment();
                self.agent_status = AgentStatus::Working;
                self.pending_proposed_plan
                    .get_or_insert_with(String::new)
                    .push_str(&t);
            }
            RuntimeToUiEvent::ToolUse(tu) => {
                self.settle_active_thinking_segment();
                self.running_tools.insert(tu.id.clone());
                let pending = self
                    .pending_assistant
                    .get_or_insert_with(|| Message::new(Role::Assistant, Vec::new()));
                pending.content.push(ContentBlock::ToolUse(tu));
                self.agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::ToolResult(tr) => {
                let tool_use_id = tr.tool_use_id.clone();
                self.settle_active_thinking_segment();
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
                self.settle_active_thinking_segment();
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
                self.agent_status = AgentStatus::Working;
                self.update_live_boundary();
            }
            RuntimeToUiEvent::GitBranchChanged { branch } => {
                self.status_bar.git_branch = branch;
            }
            RuntimeToUiEvent::RunFinished => {
                self.main_query_active = false;
                self.settle_active_thinking_segment();
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
                self.refresh_input_placeholder();
                self.agent_status = AgentStatus::Idle;
                self.update_live_boundary();
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
                self.open_plan_approval(plan);
            }
            RuntimeToUiEvent::PlanApprovalResolved { plan_id, .. } => {
                self.clear_resolved_plan_approval(&plan_id);
            }
            RuntimeToUiEvent::AgentTaskEvent(event) => {
                match event.payload {
                    AgentTaskEvent::Started {
                        parent_thread_id,
                        spawn_tool_use_id,
                        agent,
                        title,
                        execution_mode,
                        ..
                    } => {
                        self.subagents_by_tool_use
                            .insert(spawn_tool_use_id.clone(), event.thread_id.clone());
                        self.subagents.insert(
                            event.thread_id.clone(),
                            SubagentNode {
                                task_id: event.task_id,
                                thread_id: event.thread_id,
                                parent_thread_id,
                                spawn_tool_use_id,
                                agent_label: agent,
                                title,
                                execution_mode,
                                status: AgentTaskStatus::Running,
                                messages: Vec::new(),
                            },
                        );
                        self.update_live_boundary();
                    }
                    AgentTaskEvent::MessageCommitted { .. } | AgentTaskEvent::ToolUse { .. } => {}
                    AgentTaskEvent::ToolResult { tool_result } => {
                        let scoped_tool_use_id =
                            format!("{}:{}", event.thread_id, tool_result.tool_use_id);
                        self.running_tools.remove(&scoped_tool_use_id);
                        let removed_active = self.remove_tool_pause(&scoped_tool_use_id);
                        self.finish_tool_pause_removal(removed_active);
                    }
                    AgentTaskEvent::Finished { status, .. } => {
                        if let Some(node) = self.subagents.get_mut(&event.thread_id) {
                            node.status = status;
                        }
                        let removed_active =
                            self.remove_tool_pauses_for_source_thread(&event.thread_id);
                        self.finish_tool_pause_removal(removed_active);
                        self.update_live_boundary();
                    }
                    AgentTaskEvent::TurnStarted
                    | AgentTaskEvent::ThinkingDelta { .. }
                    | AgentTaskEvent::TextDelta { .. }
                    | AgentTaskEvent::TurnEnded => {
                        // 当前不渲染子线程流式内容，也不允许它写入主线程的
                        // pending_assistant 缓冲区。
                    }
                }
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
            RuntimeToUiEvent::RuntimeStatusSynced {
                status,
                restore_pending_pauses,
            } => {
                self.apply_runtime_status_sync(status, restore_pending_pauses);
            }
            RuntimeToUiEvent::CompactSummaryStarted(event) => {
                if event.trigger == CompactTrigger::Manual {
                    self.begin_manual_compact();
                }
                self.pending_compact_summary = Some(String::new());
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryDelta(event) => {
                if event.trigger == CompactTrigger::Manual {
                    self.begin_manual_compact();
                }
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
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::ActiveProfileChanged(profile) => {
                self.status_bar.active_profile = profile;
            }
            RuntimeToUiEvent::ThreadTitleChanged { title } => {
                self.current_thread_title = title.clone();
                // WebSocket 是 thread-scoped 的，事件来源 thread 就是
                // TUI 当前的 current_thread_id；用它去同步两个 thread
                // 列表缓存里对应条目的 title。
                let Some(current_thread_id) = self.current_thread_id.clone() else {
                    return;
                };
                if let Some(title) = title {
                    for thread in self.startup_recent_threads.iter_mut() {
                        if thread.id == current_thread_id {
                            thread.title = title.clone();
                        }
                    }
                    if let Some(InteractionStep::Thread {
                        threads,
                        all_threads,
                        ..
                    }) = self.interaction_step.as_mut()
                    {
                        for thread in threads.iter_mut() {
                            if thread.id == current_thread_id {
                                thread.title = title.clone();
                            }
                        }
                        for thread in all_threads.iter_mut() {
                            if thread.id == current_thread_id {
                                thread.title = title.clone();
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
                                short_description: record.short_description.clone(),
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
            // ThreadSnapshot 由 TUI 主循环直接处理，此处无需匹配
            RuntimeToUiEvent::ThreadSnapshot { .. } => {}
        }
    }

    fn finish_subagent_for_tool_result(&mut self, result: &ToolResultBlock) {
        let Some(thread_id) = self.subagents_by_tool_use.get(&result.tool_use_id) else {
            return;
        };
        let Some(node) = self.subagents.get_mut(thread_id) else {
            return;
        };
        if node.status != AgentTaskStatus::Running {
            return;
        }

        if !result.is_error && node.execution_mode == AgentTaskExecutionMode::Background {
            return;
        }

        node.status = if result.is_error {
            if result.content.trim() == "Execution cancelled" {
                AgentTaskStatus::Cancelled
            } else {
                AgentTaskStatus::Failed
            }
        } else {
            AgentTaskStatus::Completed
        };
    }

    pub fn apply_thread_snapshot(
        &mut self,
        thread_id: Option<String>,
        messages: Vec<HistoryItem>,
        subagents: Vec<AgentTaskSnapshot>,
        usage: crate::types::events::ThreadUsageSnapshot,
    ) {
        self.show_start_screen = false;
        self.current_thread_id = thread_id;
        if self.current_thread_id.is_none() {
            // 「新建线程」前的 blank 状态：把 title 缓存也清空,避免残留上一个 thread 的 title。
            self.current_thread_title = None;
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
                .insert(node.spawn_tool_use_id.clone(), node.thread_id.clone());
            self.subagents.insert(node.thread_id.clone(), node);
        }
        self.pending_assistant = None;
        self.pending_proposed_plan = None;
        self.pending_compact_summary = None;
        self.thinking_started_at = None;
        self.pending_intervention_client_echo_id = None;
        self.main_query_active = false;
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
#[path = "events/tests.rs"]
mod tests;
