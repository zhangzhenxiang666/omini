use crate::app::event::{
    AgentTaskEvent, AgentTaskExecutionMode, AgentTaskSnapshot, CommandKind, CommandSummary,
    CompactTrigger, InteractionRequest, Notification, NotificationKind, RuntimeToUiEvent,
};
use crate::app::state::{
    AgentManagerState, AgentManagerView, AgentStatus, AppState, InteractionStep,
    ModelSelectionEntry, SubagentNode, UiMessage, UiSystemEvent,
    agent_summaries_to_mention_candidates,
};
use crate::client::catalog::ThinkingEffort;
use crate::features::composer::editor::combined_user_draft;
use crate::features::timeline::model::UserDraft;
use jiff::Timestamp;
use omini_domain::conversation::ToolResultRecord;
use omini_domain::subagents::AgentSummary;
use omini_domain::task::TaskStatus;
use omini_model::message::{ContentBlock, ToolResultBlock};
use omini_protocol::HistoryItem;
use std::collections::VecDeque;

const GENERAL_HELP_SELECTABLE_COUNT: usize = 9;

/// 将持久化历史条目转换为 TUI 消息。
fn map_history_item(item: HistoryItem) -> UiMessage {
    UiMessage::from_history_item(item)
}

fn tool_result_record(result: ToolResultBlock) -> ToolResultRecord {
    ToolResultRecord {
        tool_use_id: result.tool_use_id,
        is_error: result.is_error,
        content: result.content,
        metadata: result.metadata,
    }
}

fn push_session_message(view: &mut crate::app::state::SessionState, message: UiMessage) {
    view.messages.push(message);
    if view.auto_scroll {
        view.scroll_offset = 0;
    }
}

impl AppState {
    /// 任务元数据只会改变引用它的通知行，其他历史消息继续命中缓存。
    fn invalidate_task_notice(&mut self, task_id: &str) {
        let view = &self.sessions.views["main"];
        if let Some(index) = view.messages.iter().position(|message| {
            matches!(message, UiMessage::SystemEvent(UiSystemEvent::TaskNotification(notification))
                if notification.tasks.iter().any(|task| task.task_id == task_id))
        }) {
            view.render_cache.borrow_mut().mark_dirty(index);
        }
    }

    pub fn is_run_active(&self) -> bool {
        matches!(
            self.sessions.views["main"].agent_status,
            AgentStatus::Working | AgentStatus::Thinking | AgentStatus::AwaitingInput
        )
    }

    pub fn is_main_query_active(&self) -> bool {
        self.sessions.views["main"].main_query_active
    }

    /// 清除正在流式构建中的 compact 摘要占位。
    fn clear_pending_compact_summary(&mut self) {
        self.sessions.views["main"].pending_compact_summary = None;
    }

    pub fn take_queued_user_draft(&mut self) -> Option<UserDraft> {
        Self::draft_from_inputs(&mut self.composer.queued_user_inputs)
    }

    pub fn push_optimistic_echo(&mut self, ui_message: UiMessage, client_echo_id: String) {
        self.extend_optimistic_echoes(vec![ui_message], client_echo_id);
    }

    pub fn extend_optimistic_echoes(
        &mut self,
        ui_messages: Vec<UiMessage>,
        client_echo_id: String,
    ) {
        if ui_messages.is_empty() {
            return;
        }

        let start = self.sessions.views["main"].messages.len();
        let count = ui_messages.len();
        self.sessions.views["main"].messages.extend(ui_messages);
        self.sessions.views["main"]
            .pending_client_echoes
            .insert(client_echo_id, (start..start + count).collect());
    }

    fn take_client_echo_positions(&mut self, client_echo_id: Option<&str>) -> Option<Vec<usize>> {
        self.sessions.views["main"]
            .pending_client_echoes
            .remove(client_echo_id?)
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
        self.dialogs.help_drawer = None;
        self.dialogs.interaction_step = match req {
            InteractionRequest::ModelSelection {
                providers,
                current_provider,
                current_model,
            } => {
                let mut entries: Vec<ModelSelectionEntry> = Vec::new();
                let mut selected = 0;
                let default_thinking = match self.project.status_bar.thinking_effort {
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
                    .project
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
        self.composer.autocomplete.visible = false;
        self.composer.mention_autocomplete.visible = false;
        self.dialogs.help_drawer =
            Some(crate::features::help::state::HelpDrawerState::new(commands));
    }

    pub fn close_help_drawer(&mut self) {
        self.dialogs.help_drawer = None;
    }

    pub fn help_next_tab(&mut self) {
        let Some(drawer) = &mut self.dialogs.help_drawer else {
            return;
        };
        drawer.tab = match drawer.tab {
            crate::features::help::state::HelpTab::General => {
                crate::features::help::state::HelpTab::Commands
            }
            crate::features::help::state::HelpTab::Commands => {
                crate::features::help::state::HelpTab::Skills
            }
            crate::features::help::state::HelpTab::Skills => {
                crate::features::help::state::HelpTab::General
            }
        };
    }

    pub fn help_prev_tab(&mut self) {
        let Some(drawer) = &mut self.dialogs.help_drawer else {
            return;
        };
        drawer.tab = match drawer.tab {
            crate::features::help::state::HelpTab::General => {
                crate::features::help::state::HelpTab::Skills
            }
            crate::features::help::state::HelpTab::Commands => {
                crate::features::help::state::HelpTab::General
            }
            crate::features::help::state::HelpTab::Skills => {
                crate::features::help::state::HelpTab::Commands
            }
        };
    }

    pub fn help_select_next(&mut self) {
        let Some(drawer) = &mut self.dialogs.help_drawer else {
            return;
        };
        match drawer.tab {
            crate::features::help::state::HelpTab::Commands => {
                let len = command_count(&drawer.commands, CommandKind::Builtin);
                if len > 0 {
                    drawer.command_selected = (drawer.command_selected + 1).min(len - 1);
                }
            }
            crate::features::help::state::HelpTab::Skills => {
                let len = command_count(&drawer.commands, CommandKind::Skill);
                if len > 0 {
                    drawer.skill_selected = (drawer.skill_selected + 1).min(len - 1);
                }
            }
            crate::features::help::state::HelpTab::General => {
                drawer.general_selected =
                    (drawer.general_selected + 1).min(GENERAL_HELP_SELECTABLE_COUNT - 1);
            }
        }
    }

    pub fn help_select_prev(&mut self) {
        let Some(drawer) = &mut self.dialogs.help_drawer else {
            return;
        };
        match drawer.tab {
            crate::features::help::state::HelpTab::Commands => {
                drawer.command_selected = drawer.command_selected.saturating_sub(1);
            }
            crate::features::help::state::HelpTab::Skills => {
                drawer.skill_selected = drawer.skill_selected.saturating_sub(1);
            }
            crate::features::help::state::HelpTab::General => {
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
            RuntimeToUiEvent::AgentRunChanged(run) => {
                self.sessions.agent_runs.insert(run.id.clone(), run);
            }
            RuntimeToUiEvent::RunStarted => {
                self.sessions.views["main"].main_query_active = true;
                self.start.show_start_screen = false;
                self.sessions.views["main"].manual_compact_running = false;
                self.sessions.views["main"].pending_assistant = None;
                self.sessions.views["main"].pending_proposed_plan = None;
                self.sessions.views["main"].pending_compact_summary = None;
                self.sessions.views["main"].thinking_started_at = None;
                self.clear_run_dividers();
                // 重连状态同步可能已校准活动计时器，避免被 replay 的 RunStarted 重置。
                if self.sessions.views["main"].run_timer.is_none() {
                    self.start_run_timer();
                }
                self.sessions.views["main"].agent_status = AgentStatus::Thinking;
            }
            RuntimeToUiEvent::UserMessageInjected {
                item,
                client_echo_id,
            } => {
                self.start.show_start_screen = false;
                let ui_message = map_history_item(item);
                let replaces_matching_local_echo = client_echo_id.is_none()
                    && matches!(
                        (self.sessions.views["main"].messages.last(), &ui_message),
                        (
                            Some(UiMessage::SystemEvent(UiSystemEvent::UserInputEcho(draft))),
                            UiMessage::UserInput(input)
                        ) if draft == &crate::features::timeline::model::user_input_draft(input)
                    );
                if self
                    .take_client_echo_positions(client_echo_id.as_deref())
                    .is_none()
                {
                    if replaces_matching_local_echo {
                        let index = self.sessions.views["main"].messages.len() - 1;
                        *self.sessions.views["main"]
                            .messages
                            .last_mut()
                            .expect("matching echo is present") = ui_message;
                        self.sessions.views["main"]
                            .render_cache
                            .get_mut()
                            .mark_dirty(index);
                    } else if client_echo_id.is_some()
                        || self.sessions.views["main"].messages.last() != Some(&ui_message)
                    {
                        // 不同提交可以有相同内容；仅无提交 ID 的重放沿用相邻消息去重。
                        self.sessions.views["main"].messages.push(ui_message);
                    }
                }
                if self.sessions.views["main"].auto_scroll {
                    self.sessions.views["main"].scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::AgentTaskUserMessageQueued { task_id, item, .. } => {
                if let Some(view) = self.sessions.views.get_mut(&task_id) {
                    let message = map_history_item(item);
                    push_session_message(view, message);
                }
            }
            RuntimeToUiEvent::AgentTaskMessageQueued { task_id, item } => {
                if let Some(view) = self.sessions.views.get_mut(&task_id) {
                    push_session_message(view, map_history_item(item));
                }
            }
            RuntimeToUiEvent::TurnStarted => {
                // 上轮残留的思考计时先结算，再提交 pending_assistant
                self.settle_active_thinking_segment();
                // 如果上轮还有未提交的 pending_assistant，先推入 messages
                if let Some(msg) = self.sessions.views["main"].pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.sessions.views["main"].messages.extend(msg.finish());
                }
                self.sessions.views["main"].agent_status = AgentStatus::Thinking;
            }
            RuntimeToUiEvent::ThinkingDelta(t) => {
                self.sessions.views["main"].agent_status = AgentStatus::Thinking;
                // 首个 delta 开启本地计时；结束由 text/tool/turn 结算
                if self.sessions.views["main"].thinking_started_at.is_none() {
                    self.sessions.views["main"].thinking_started_at =
                        Some(std::time::Instant::now());
                }
                let pending = self.sessions.views["main"]
                    .pending_assistant
                    .get_or_insert_with(|| {
                        crate::features::timeline::model::StreamingMessage::default()
                    });
                if let Some(ContentBlock::Thinking(tb)) = pending.content.last_mut() {
                    tb.thinking.push_str(&t);
                } else {
                    pending.content.push(ContentBlock::from_thinking(t));
                }
            }
            RuntimeToUiEvent::TextDelta(t) => {
                self.settle_active_thinking_segment();
                self.sessions.views["main"].agent_status = AgentStatus::Working;
                let pending = self.sessions.views["main"]
                    .pending_assistant
                    .get_or_insert_with(|| {
                        crate::features::timeline::model::StreamingMessage::default()
                    });
                if let Some(ContentBlock::Text(tb)) = pending.content.last_mut() {
                    tb.text.push_str(&t);
                } else {
                    pending.content.push(ContentBlock::from_text(t));
                }
            }
            RuntimeToUiEvent::ProposedPlanDelta(t) => {
                self.settle_active_thinking_segment();
                self.sessions.views["main"].agent_status = AgentStatus::Working;
                self.sessions.views["main"]
                    .pending_proposed_plan
                    .get_or_insert_with(String::new)
                    .push_str(&t);
            }
            RuntimeToUiEvent::ToolUse(tu) => {
                self.settle_active_thinking_segment();
                self.sessions.views["main"]
                    .running_tools
                    .insert(tu.id.clone());
                let pending = self.sessions.views["main"]
                    .pending_assistant
                    .get_or_insert_with(|| {
                        crate::features::timeline::model::StreamingMessage::default()
                    });
                pending.content.push(ContentBlock::ToolUse(tu));
                self.sessions.views["main"].agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::ToolResult(tr) => {
                self.settle_active_thinking_segment();
                self.finish_subagent_for_tool_result(&tr);
                self.sessions.views["main"]
                    .running_tools
                    .remove(&tr.tool_use_id);
                let removed_active = self.remove_tool_pause(&tr.tool_use_id);
                self.finish_tool_pause_removal(removed_active);
                // 工具结果异步返回，追加到 pending_assistant 或最后一条消息中
                if let Some(pending) = &mut self.sessions.views["main"].pending_assistant {
                    pending.content.push(ContentBlock::ToolResult(tr));
                } else {
                    self.sessions.views["main"]
                        .messages
                        .push(UiMessage::SystemEvent(UiSystemEvent::ToolResults {
                            results: vec![tool_result_record(tr)],
                        }));
                }
            }
            RuntimeToUiEvent::TaskChanged(event) => {
                if self
                    .project
                    .current_thread_id
                    .as_ref()
                    .is_none_or(|thread_id| thread_id == &event.task.owner_thread_id)
                {
                    self.track_background_task(event.task.task_id.clone(), event.task.status);
                }
                if event.task.kind == omini_domain::task::TaskKind::SubAgent
                    && let Some(node) = self
                        .sessions
                        .subagents
                        .values_mut()
                        .find(|node| node.task_id == event.task.task_id)
                {
                    node.status = event.task.status;
                    node.started_at = event.task.created_at;
                    node.duration = event
                        .task
                        .status
                        .is_terminal()
                        .then_some(event.task.completed_at.unwrap_or(event.task.updated_at))
                        .and_then(|finished_at| {
                            std::time::Duration::try_from(
                                finished_at.duration_since(event.task.created_at),
                            )
                            .ok()
                        });
                    if let Some(view) = self.sessions.views.get_mut(&node.task_id)
                        && !matches!(
                            event.task.status,
                            TaskStatus::Running | TaskStatus::Cancelling
                        )
                    {
                        view.agent_status = AgentStatus::Idle;
                        view.run_timer = None;
                    }
                    self.prune_terminal_tasks();
                }
                self.invalidate_task_notice(&event.task.task_id);
            }
            RuntimeToUiEvent::TaskOutputDelta(_) => {}
            RuntimeToUiEvent::TurnEnded => {
                self.settle_active_thinking_segment();
                if let Some(msg) = self.sessions.views["main"].pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.sessions.views["main"].messages.extend(msg.finish());
                }
                if self.sessions.views["main"].auto_scroll {
                    self.sessions.views["main"].scroll_offset = 0;
                }
                self.sessions.views["main"].agent_status = AgentStatus::Working;
            }
            RuntimeToUiEvent::GitBranchChanged { branch } => {
                self.project.status_bar.git_branch = branch;
            }
            RuntimeToUiEvent::RunFinished => {
                self.sessions.views["main"].main_query_active = false;
                self.settle_active_thinking_segment();
                if let Some(msg) = self.sessions.views["main"].pending_assistant.take()
                    && !msg.content.is_empty()
                {
                    self.sessions.views["main"].messages.extend(msg.finish());
                }
                if let Some(plan) = self.sessions.views["main"].pending_proposed_plan.take()
                    && !plan.trim().is_empty()
                {
                    self.sessions.views["main"]
                        .messages
                        .push(UiMessage::SystemEvent(UiSystemEvent::Plan { text: plan }));
                }
                if let Some(elapsed) = self.finish_run_timer() {
                    self.sessions.views["main"]
                        .messages
                        .push(UiMessage::SystemEvent(UiSystemEvent::RunDivider {
                            elapsed,
                        }));
                }
                self.sessions.views["main"].pending_client_echoes.clear();
                if self.sessions.views["main"].auto_scroll {
                    self.sessions.views["main"].scroll_offset = 0;
                }
                self.composer.refresh_input_placeholder();
                self.sessions.views["main"].agent_status = AgentStatus::Idle;
            }
            RuntimeToUiEvent::ToolPauseRequested(req) => {
                let should_prepare = self.push_tool_pause(req);
                if should_prepare {
                    self.prepare_active_tool_pause();
                }
                self.pause_run_timer();
                self.sessions.views["main"].agent_status = AgentStatus::AwaitingInput;
            }
            RuntimeToUiEvent::PlanSubmitted(plan) => {
                self.open_plan_approval(plan);
            }
            RuntimeToUiEvent::PlanApprovalResolved { plan_id, .. } => {
                self.clear_resolved_plan_approval(&plan_id);
            }
            RuntimeToUiEvent::AgentTaskEvent(event) => {
                let task_id = event.task_id.clone();
                let thread_id = event.thread_id.clone();
                let direct_child = event.parent_task_id.is_none();
                match event.payload {
                    AgentTaskEvent::Started {
                        parent_thread_id,
                        spawn_tool_use_id,
                        agent,
                        title,
                        initial_prompt,
                        depth,
                        execution_mode,
                        ..
                    } if depth == 1 && direct_child => {
                        if execution_mode == AgentTaskExecutionMode::Background
                            && self
                                .project
                                .current_thread_id
                                .as_ref()
                                .is_none_or(|thread_id| thread_id == &event.owner_thread_id)
                        {
                            self.track_background_task(task_id.clone(), TaskStatus::Running);
                        }
                        let new_task = !self.sessions.subagents.contains_key(&thread_id);
                        self.sessions
                            .subagents_by_tool_use
                            .insert(spawn_tool_use_id.clone(), thread_id.clone());
                        self.sessions.subagents.insert(
                            thread_id.clone(),
                            SubagentNode {
                                task_id: task_id.clone(),
                                thread_id,
                                parent_thread_id,
                                spawn_tool_use_id,
                                agent_label: agent,
                                title,
                                execution_mode,
                                status: TaskStatus::Running,
                                duration: None,
                                started_at: Timestamp::now(),
                                messages: Vec::new(),
                            },
                        );
                        if new_task && execution_mode == AgentTaskExecutionMode::Background {
                            self.sessions.subagent_order.push(task_id.clone());
                            let prompt = map_history_item(HistoryItem::UserInput(initial_prompt));
                            let mut view = crate::app::state::SessionState {
                                agent_status: AgentStatus::Thinking,
                                run_timer: Some(crate::app::state::RunTimer::started_at(
                                    tokio::time::Instant::now(),
                                )),
                                auto_scroll: true,
                                ..crate::app::state::SessionState::default()
                            };
                            view.messages.push(prompt);
                            self.sessions.views.insert(task_id.clone(), view);
                        }
                        self.invalidate_task_notice(&task_id);
                    }
                    AgentTaskEvent::Started { .. } => {}
                    AgentTaskEvent::MessageCommitted { message, .. } => {
                        let ui_messages = UiMessage::from_model_message(message.clone());
                        if !ui_messages.is_empty() {
                            if let Some(node) = self.sessions.subagents.get_mut(&thread_id) {
                                node.messages.push(message);
                            }
                            if let Some(view) = self.sessions.views.get_mut(&task_id) {
                                view.pending_assistant = None;
                                view.thinking_started_at = None;
                                for ui_message in ui_messages {
                                    push_session_message(view, ui_message);
                                }
                            }
                        }
                    }
                    AgentTaskEvent::ToolUse { tool_use } => {
                        if let Some(view) = self.sessions.views.get_mut(&task_id) {
                            view.running_tools.insert(tool_use.id.clone());
                            let pending = view.pending_assistant.get_or_insert_with(|| {
                                crate::features::timeline::model::StreamingMessage::default()
                            });
                            if !pending.content.iter().any(|block| {
                                matches!(block, ContentBlock::ToolUse(current) if current.id == tool_use.id)
                            }) {
                                pending.content.push(ContentBlock::ToolUse(tool_use));
                            }
                            view.agent_status = AgentStatus::Working;
                        }
                    }
                    AgentTaskEvent::ToolResult { tool_result } => {
                        let scoped_tool_use_id =
                            format!("{}:{}", thread_id, tool_result.tool_use_id);
                        if let Some(view) = self.sessions.views.get_mut(&task_id) {
                            view.running_tools.remove(&tool_result.tool_use_id);
                        }
                        let removed_active = self.remove_tool_pause(&scoped_tool_use_id);
                        self.finish_tool_pause_removal(removed_active);
                    }
                    AgentTaskEvent::Finished { status, .. } => {
                        if let Some(status) = status
                            && let Some(node) = self.sessions.subagents.get_mut(&thread_id)
                        {
                            node.status = status;
                            if status.is_terminal() && node.duration.is_none() {
                                node.duration = std::time::Duration::try_from(
                                    Timestamp::now().duration_since(node.started_at),
                                )
                                .ok();
                            }
                        }
                        if let Some(view) = self.sessions.views.get_mut(&task_id) {
                            if let Some(pending) = view.pending_assistant.take()
                                && !pending.content.is_empty()
                            {
                                for message in pending.finish() {
                                    push_session_message(view, message);
                                }
                            }
                            view.agent_status = AgentStatus::Idle;
                            view.run_timer = None;
                        }
                        let removed_active = self.remove_tool_pauses_for_source_thread(&thread_id);
                        self.finish_tool_pause_removal(removed_active);
                        self.prune_terminal_tasks();
                        self.invalidate_task_notice(&task_id);
                    }
                    AgentTaskEvent::TurnStarted => {
                        if let Some(view) = self.sessions.views.get_mut(&task_id) {
                            if let Some(pending) = view.pending_assistant.take()
                                && !pending.content.is_empty()
                            {
                                for message in pending.finish() {
                                    push_session_message(view, message);
                                }
                            }
                            view.agent_status = AgentStatus::Thinking;
                            view.run_timer.get_or_insert_with(|| {
                                crate::app::state::RunTimer::started_at(tokio::time::Instant::now())
                            });
                        }
                    }
                    AgentTaskEvent::ThinkingDelta { delta } => {
                        if let Some(view) = self.sessions.views.get_mut(&task_id) {
                            let now = std::time::Instant::now();
                            view.thinking_started_at.get_or_insert(now);
                            let pending = view.pending_assistant.get_or_insert_with(|| {
                                crate::features::timeline::model::StreamingMessage::default()
                            });
                            match pending.content.last_mut() {
                                Some(ContentBlock::Thinking(thinking)) => {
                                    thinking.thinking.push_str(&delta);
                                }
                                _ => pending.content.push(ContentBlock::Thinking(
                                    omini_model::message::ThinkingBlock {
                                        thinking: delta,
                                        duration_ms: None,
                                    },
                                )),
                            }
                            view.agent_status = AgentStatus::Thinking;
                        }
                    }
                    AgentTaskEvent::TextDelta { delta } => {
                        if let Some(view) = self.sessions.views.get_mut(&task_id) {
                            let pending = view.pending_assistant.get_or_insert_with(|| {
                                crate::features::timeline::model::StreamingMessage::default()
                            });
                            match pending.content.last_mut() {
                                Some(ContentBlock::Text(text)) => text.text.push_str(&delta),
                                _ => pending.content.push(ContentBlock::from_text(delta)),
                            }
                            view.thinking_started_at = None;
                            view.agent_status = AgentStatus::Thinking;
                        }
                    }
                    AgentTaskEvent::TurnEnded => {
                        if let Some(view) = self.sessions.views.get_mut(&task_id) {
                            view.agent_status = AgentStatus::Working;
                        }
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
                        if !self.dialogs.pending_tool_pauses.is_empty() {
                            self.sessions.views["main"].agent_status = AgentStatus::AwaitingInput;
                        } else if !self.is_run_active() {
                            self.sessions.views["main"].agent_status = AgentStatus::Idle;
                        }
                    }
                }
                self.sessions.views["main"]
                    .messages
                    .push(UiMessage::SystemEvent(UiSystemEvent::Notification(
                        notification,
                    )));
                if self.sessions.views["main"].auto_scroll {
                    self.sessions.views["main"].scroll_offset = 0;
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
                self.project.status_bar.active_provider = provider;
                self.project.status_bar.model = model;
                self.project.status_bar.thinking_effort = thinking_effort;
                self.project.status_bar.context_window = context_window;
                // 模型切换成功后自动关闭选择弹窗
                self.dialogs.interaction_step = None;
                self.dialogs.interaction_request = None;
            }
            RuntimeToUiEvent::UsageChanged(usage) => {
                self.project.status_bar.current_context_tokens = usage.current_context_tokens;
                self.project.status_bar.total_tokens = usage.total_tokens;
                self.project.status_bar.total_cached_tokens = usage.total_cached_tokens;
                self.project.status_bar.context_window = usage.context_window;
            }
            RuntimeToUiEvent::UsageTotalsChanged {
                total_tokens,
                total_cached_tokens,
            } => {
                self.project.status_bar.total_tokens = total_tokens;
                self.project.status_bar.total_cached_tokens = total_cached_tokens;
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
                self.sessions.views["main"].pending_compact_summary = Some(String::new());
                if self.sessions.views["main"].auto_scroll {
                    self.sessions.views["main"].scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryDelta(event) => {
                if event.trigger == CompactTrigger::Manual {
                    self.begin_manual_compact();
                }
                self.sessions.views["main"]
                    .pending_compact_summary
                    .get_or_insert_with(String::new)
                    .push_str(&event.delta);
                if self.sessions.views["main"].auto_scroll {
                    self.sessions.views["main"].scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryFinished(event) => {
                let trigger = event.trigger;
                let summary = event.summary;
                // 优先使用 pending 中累积的流式文本；Finished 的 summary 为最终权威版本
                let final_text = if !summary.trim().is_empty() {
                    summary
                } else {
                    self.sessions.views["main"]
                        .pending_compact_summary
                        .take()
                        .unwrap_or_default()
                };
                self.sessions.views["main"].pending_compact_summary = None;
                if !final_text.trim().is_empty() {
                    self.sessions.views["main"]
                        .messages
                        .push(UiMessage::SystemEvent(UiSystemEvent::Summary {
                            text: final_text,
                        }));
                }
                self.project.status_bar.current_context_tokens = event.after_tokens as i64;
                if trigger == CompactTrigger::Manual {
                    self.finish_manual_compact();
                }
                if self.sessions.views["main"].auto_scroll {
                    self.sessions.views["main"].scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::CompactSummaryFailed(event) => {
                self.clear_pending_compact_summary();
                self.sessions.views["main"]
                    .messages
                    .push(UiMessage::SystemEvent(UiSystemEvent::Notification(
                        Notification::warning(compact_summary_failed_text(
                            event.trigger,
                            event.agent_label.as_deref(),
                            &event.message,
                        )),
                    )));
                if event.trigger == CompactTrigger::Manual {
                    self.finish_manual_compact();
                }
                if self.sessions.views["main"].auto_scroll {
                    self.sessions.views["main"].scroll_offset = 0;
                }
            }
            RuntimeToUiEvent::ActiveProfileChanged(profile) => {
                self.project.status_bar.active_profile = profile;
            }
            RuntimeToUiEvent::ThreadTitleChanged { title } => {
                self.project.current_thread_title = title.clone();
                // WebSocket 是 thread-scoped 的，事件来源 thread 就是
                // TUI 当前的 current_thread_id；用它去同步两个 thread
                // 列表缓存里对应条目的 title。
                let Some(current_thread_id) = self.project.current_thread_id.clone() else {
                    return;
                };
                if let Some(title) = title {
                    for thread in self.start.startup_recent_threads.iter_mut() {
                        if thread.id == current_thread_id {
                            thread.title = title.clone();
                        }
                    }
                    if let Some(InteractionStep::Thread {
                        threads,
                        all_threads,
                        ..
                    }) = self.dialogs.interaction_step.as_mut()
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
                self.dialogs.interaction_request = Some(req);
            }
            RuntimeToUiEvent::ShowHelpDrawer(commands) => {
                self.open_help_drawer(commands);
            }
            RuntimeToUiEvent::CommandList(cmds) => {
                self.composer.autocomplete.all_commands =
                    crate::features::commands::commands_with_runtime_skills(cmds);
            }
            RuntimeToUiEvent::AgentManagementUpdated { records } => {
                self.composer.mention_autocomplete.set_candidates(
                    agent_summaries_to_mention_candidates(
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
                    ),
                );
                self.composer.update_input_autocomplete();
                if let Some(InteractionStep::Agents(manager)) = &mut self.dialogs.interaction_step {
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
                if let Some(InteractionStep::Agents(manager)) = &mut self.dialogs.interaction_step {
                    manager.apply_generated(source_kind, draft);
                }
            }
            RuntimeToUiEvent::AgentGenerateFailed { message } => {
                if let Some(InteractionStep::Agents(manager)) = &mut self.dialogs.interaction_step {
                    manager.fail_generation(message);
                } else {
                    self.sessions.views["main"]
                        .messages
                        .push(UiMessage::SystemEvent(UiSystemEvent::Notification(
                            Notification::info(message),
                        )));
                }
            }
            // ThreadSnapshot 由 TUI 主循环直接处理，此处无需匹配
            RuntimeToUiEvent::ThreadSnapshot { .. } => {}
        }
    }

    fn finish_subagent_for_tool_result(&mut self, result: &ToolResultBlock) {
        let Some(thread_id) = self.sessions.subagents_by_tool_use.get(&result.tool_use_id) else {
            return;
        };
        let Some(node) = self.sessions.subagents.get_mut(thread_id) else {
            return;
        };
        if node.status != TaskStatus::Running {
            return;
        }

        if !result.is_error && node.execution_mode == AgentTaskExecutionMode::Background {
            return;
        }

        node.status = if result.is_error {
            if result.content.trim() == "Execution cancelled" {
                TaskStatus::Cancelled
            } else {
                TaskStatus::Failed
            }
        } else {
            TaskStatus::Completed
        };
        if node.duration.is_none() {
            node.duration =
                std::time::Duration::try_from(Timestamp::now().duration_since(node.started_at))
                    .ok();
        }
        let task_id = node.task_id.clone();
        self.invalidate_task_notice(&task_id);
    }

    pub fn apply_thread_snapshot(
        &mut self,
        thread_id: Option<String>,
        messages: Vec<HistoryItem>,
        subagents: Vec<AgentTaskSnapshot>,
        usage: crate::app::event::ThreadUsageSnapshot,
    ) {
        let active_task_id = if self.project.current_thread_id == thread_id {
            self.sessions.active_session_task_id.clone()
        } else {
            None
        };
        self.start.show_start_screen = false;
        self.project.current_thread_id = thread_id;
        if self.project.current_thread_id.is_none() {
            // 「新建线程」前的 blank 状态：把 title 缓存也清空,避免残留上一个 thread 的 title。
            self.project.current_thread_title = None;
        }
        self.sessions.views["main"].messages = UiMessage::from_history_items(messages);
        self.sessions.views["main"].render_cache.get_mut().reset();
        self.sessions.views["main"].pending_client_echoes.clear();
        self.project.status_bar.current_context_tokens = usage.current_context_tokens;
        self.project.status_bar.total_tokens = usage.total_tokens;
        self.project.status_bar.total_cached_tokens = usage.total_cached_tokens;
        self.project.status_bar.context_window = usage.context_window;
        self.sessions.subagents.clear();
        self.sessions.background_tasks.clear();
        self.sessions.subagents_by_tool_use.clear();
        self.sessions.subagent_order.clear();
        self.sessions.views.clear_children();
        self.sessions.active_session_task_id = None;
        self.sessions.session_selector_focused = false;
        self.sessions.session_selection_index = 0;
        for subagent in subagents {
            let task_id = subagent.task.task_id.clone();
            if subagent.task.depth != 1
                || subagent.task.parent_task_id.is_some()
                || subagent.task.execution_mode != AgentTaskExecutionMode::Background
            {
                continue;
            }
            if self
                .project
                .current_thread_id
                .as_ref()
                .is_none_or(|thread_id| thread_id == &subagent.task.owner_thread_id)
            {
                self.track_background_task(task_id.clone(), subagent.task.status);
            }
            if subagent.task.status.is_terminal() && active_task_id.as_ref() != Some(&task_id) {
                continue;
            }
            let history = UiMessage::from_history_items(
                subagent.history.into_iter().map(Into::into).collect(),
            );
            let node = SubagentNode::from(subagent.task);
            self.sessions
                .subagents_by_tool_use
                .insert(node.spawn_tool_use_id.clone(), node.thread_id.clone());
            self.sessions.subagent_order.push(task_id.clone());
            self.sessions.views.insert(
                task_id.clone(),
                crate::app::state::SessionState {
                    messages: history,
                    auto_scroll: true,
                    ..crate::app::state::SessionState::default()
                },
            );
            self.sessions.subagents.insert(node.thread_id.clone(), node);
            if active_task_id.as_ref() == Some(&task_id) {
                self.sessions.active_session_task_id = Some(task_id);
            }
        }
        self.sessions.views["main"].pending_assistant = None;
        self.sessions.views["main"].pending_proposed_plan = None;
        self.sessions.views["main"].pending_compact_summary = None;
        self.sessions.views["main"].thinking_started_at = None;
        self.sessions.views["main"].main_query_active = false;
        self.sessions.views["main"].run_timer = None;
        self.sessions.views["main"].manual_compact_running = false;
        self.composer.queued_user_inputs.clear();
        self.composer.input.clear();
        self.composer.input_mentions.clear();
        self.composer.input_images.clear();
        self.composer.input_paste_markers.clear();
        self.composer.cursor_char = 0;
        self.composer.input_scroll_line = 0;
        self.sessions.views["main"].agent_status = AgentStatus::Idle;
        self.dialogs.interaction_step = None;
        self.dialogs.interaction_request = None;
        self.dialogs.help_drawer = None;
        self.dialogs.plan.queued.clear();
        self.clear_plan_approval();
        self.scroll_to_bottom();
    }
}

fn compact_summary_failed_text(
    trigger: crate::app::event::CompactTrigger,
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
