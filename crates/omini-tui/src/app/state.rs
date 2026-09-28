use crate::app::event::{ActiveProfile, SubmittedPlan, ToolPauseRequest};
use jiff::Timestamp;
use omini_domain::task::TaskStatus;
use rand::Rng;
use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;
use tokio::time::Instant;

pub use crate::features::agents::state::{
    AgentCreateStep, AgentEditorField, AgentGenerateReturn, AgentManagerState, AgentManagerView,
    AgentModelEntry, InteractionStep, ModelSelectionEntry,
};
pub use crate::features::composer::autocomplete::CommandAutocomplete;
pub use crate::features::composer::mention::{
    InputMention, MentionAutocomplete, MentionCandidate, agent_summaries_to_mention_candidates,
};
pub use crate::features::sessions::timing::{RunTimer, format_run_duration};

const START_SCREEN_TIPS: &[&str] = &[
    "先用 /plan 把方案聊清楚，再进入实现会更稳。",
    "用 @文件 或 @目录 限定上下文，答案会更贴近当前代码。",
    "陌生项目可以先让 omini 梳理模块职责和调用链。",
    "复杂问题先交给 @subagent 调研，再让主会话做决策。",
    "改动前可以说明验收标准，omini 会更容易选择合适测试。",
    "上下文变长后用 /compact 保留关键决策和线索。",
    "用 /agents 管理适合当前项目的专用 subagent。",
    "让 omini 总结当前 diff，可以快速检查风险和漏测点。",
];

#[derive(Debug)]
pub struct AppState {
    pub sessions: crate::features::sessions::state::SessionsState,
    pub composer: crate::features::composer::state::ComposerState,
    pub dialogs: crate::features::dialogs::state::DialogsState,
    pub project: crate::features::status::state::ProjectState,
    pub start: crate::features::start::state::StartState,
    pub selection: crate::ui::selection_state::SelectionState,
    pub geometry: crate::ui::geometry::FrameGeometry,
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: Default::default(),
            composer: Default::default(),
            dialogs: Default::default(),
            project: Default::default(),
            start: Default::default(),
            selection: Default::default(),
            geometry: Default::default(),
        }
    }

    pub fn active_tool_pause(&self) -> Option<&ToolPauseRequest> {
        self.dialogs.pending_tool_pauses.front()
    }

    pub fn push_tool_pause(&mut self, req: ToolPauseRequest) -> bool {
        if let Some(existing) = self
            .dialogs
            .pending_tool_pauses
            .iter_mut()
            .find(|pause| pause.tool_use_id == req.tool_use_id)
        {
            *existing = req;
            return false;
        }

        let was_empty = self.dialogs.pending_tool_pauses.is_empty();
        self.dialogs.pending_tool_pauses.push_back(req);
        was_empty
    }

    pub fn remove_tool_pause(&mut self, tool_use_id: &str) -> bool {
        let removed_active = self
            .active_tool_pause()
            .is_some_and(|pause| pause.tool_use_id == tool_use_id);
        self.dialogs
            .pending_tool_pauses
            .retain(|pause| pause.tool_use_id != tool_use_id);
        removed_active
    }

    pub fn remove_tool_pauses_for_source_thread(&mut self, source_thread_id: &str) -> bool {
        let removed_active = self
            .active_tool_pause()
            .is_some_and(|pause| pause.source_thread_id.as_deref() == Some(source_thread_id));
        self.dialogs
            .pending_tool_pauses
            .retain(|pause| pause.source_thread_id.as_deref() != Some(source_thread_id));
        removed_active
    }

    pub fn finish_tool_pause_removal(&mut self, removed_active: bool) {
        if self.dialogs.pending_tool_pauses.is_empty() {
            self.resume_run_timer();
            self.reset_permission_drawer();
            if self.sessions.views["main"].agent_status == AgentStatus::AwaitingInput {
                self.sessions.views["main"].agent_status = AgentStatus::Working;
            }
        } else if removed_active {
            self.prepare_active_tool_pause();
            self.sessions.views["main"].agent_status = AgentStatus::AwaitingInput;
        }
    }

    pub fn open_plan_approval(&mut self, plan: SubmittedPlan) {
        if self
            .dialogs
            .plan
            .plan_approval
            .as_ref()
            .is_some_and(|current| current.id == plan.id)
        {
            return;
        }
        if self.dialogs.plan.plan_approval.is_some() {
            if !self
                .dialogs
                .plan
                .queued
                .iter()
                .any(|queued| queued.id == plan.id)
            {
                self.dialogs.plan.queued.push_back(plan);
            }
            return;
        }
        self.dialogs.plan.plan_approval = Some(plan);
        self.dialogs.plan.plan_approval_selected = 0;
        self.dialogs.plan.plan_approval_auto = false;
        if self.sessions.views["main"].auto_scroll {
            self.sessions.views["main"].scroll_offset = 0;
        }
    }

    pub fn clear_resolved_plan_approval(&mut self, plan_id: &str) {
        self.dialogs.plan.queued.retain(|plan| plan.id != plan_id);
        if self
            .dialogs
            .plan
            .plan_approval
            .as_ref()
            .is_some_and(|plan| plan.id == plan_id)
        {
            self.clear_plan_approval();
        }
    }

    pub fn clear_plan_approval(&mut self) {
        self.dialogs.plan.plan_approval = self.dialogs.plan.queued.pop_front();
        self.dialogs.plan.plan_approval_selected = 0;
        self.dialogs.plan.plan_approval_auto = false;
    }

    pub fn start_run_timer(&mut self) {
        self.sessions.views["main"].run_timer = Some(RunTimer::started_at(Instant::now()));
    }

    pub fn sync_run_timer(&mut self, elapsed: Duration, paused: bool) {
        self.sessions.views["main"].run_timer = Some(RunTimer::started_with_elapsed_at(
            Instant::now(),
            elapsed,
            paused,
        ));
    }

    pub fn begin_manual_compact(&mut self) {
        self.sessions.views["main"].manual_compact_running = true;
        self.sessions.views["main"].agent_status = AgentStatus::Working;
        if self.sessions.views["main"].run_timer.is_none() {
            self.start_run_timer();
        }
    }

    pub fn finish_manual_compact(&mut self) {
        if !self.sessions.views["main"].manual_compact_running {
            return;
        }
        self.sessions.views["main"].manual_compact_running = false;
        self.sessions.views["main"].run_timer = None;
        self.sessions.views["main"].agent_status = AgentStatus::Idle;
    }

    pub fn begin_main_query_submission(&mut self) {
        self.sessions.views["main"].main_query_active = true;
        self.sessions.views["main"].agent_status = AgentStatus::Working;
    }

    pub fn clear_run_dividers(&mut self) {
        let view = &mut self.sessions.views["main"];
        let before = view.messages.len();
        view.messages.retain(|message| {
            !matches!(
                message,
                UiMessage::SystemEvent(UiSystemEvent::RunDivider { .. })
            )
        });
        if view.messages.len() != before {
            view.render_cache.get_mut().reset();
        }
    }

    pub fn pause_run_timer(&mut self) {
        if let Some(timer) = &mut self.sessions.views["main"].run_timer {
            timer.pause_at(Instant::now());
        }
    }

    pub fn resume_run_timer(&mut self) {
        if let Some(timer) = &mut self.sessions.views["main"].run_timer {
            timer.resume_at(Instant::now());
        }
    }

    pub fn finish_run_timer(&mut self) -> Option<Duration> {
        self.sessions.views["main"]
            .run_timer
            .take()
            .map(|timer| timer.finish_at(Instant::now()))
    }

    pub fn current_run_elapsed(&self) -> Option<Duration> {
        self.sessions.views["main"]
            .run_timer
            .as_ref()
            .map(|timer| timer.elapsed_at(Instant::now()))
    }

    pub fn apply_runtime_status_sync(
        &mut self,
        status: omini_protocol::ThreadRuntimeStatus,
        restore_pending_pauses: bool,
    ) {
        let omini_protocol::ThreadRuntimeStatus {
            active_profile,
            pending_plan_approval,
            subagent_threads,
            activity,
            state,
            pending_pauses,
            git_branch,
            ..
        } = status;

        self.project.status_bar.active_profile = match active_profile {
            omini_protocol::ActiveProfile::Main => ActiveProfile::Main,
            omini_protocol::ActiveProfile::Auto => ActiveProfile::Auto,
            omini_protocol::ActiveProfile::Plan => ActiveProfile::Plan,
        };
        self.project.status_bar.git_branch = git_branch;
        self.composer
            .mention_autocomplete
            .set_candidates(agent_summaries_to_mention_candidates(
                subagent_threads.into_iter().map(Into::into).collect(),
            ));
        self.composer.update_input_autocomplete();
        self.sync_pending_plan_approval(pending_plan_approval);
        if restore_pending_pauses {
            self.sync_pending_tool_pauses(pending_pauses.clone());
        }

        self.sessions.views["main"].main_query_active = activity.as_ref().is_some_and(|activity| {
            activity.kind == omini_protocol::ThreadRuntimeActivityKind::Query
        });

        let Some(activity) = activity else {
            if state == omini_protocol::ThreadRuntimeState::Idle {
                self.sessions.views["main"].manual_compact_running = false;
                self.sessions.views["main"].run_timer = None;
                self.sessions.views["main"].agent_status = AgentStatus::Idle;
            }
            return;
        };

        let agent_status = match state {
            omini_protocol::ThreadRuntimeState::Idle => return,
            omini_protocol::ThreadRuntimeState::Thinking => AgentStatus::Thinking,
            omini_protocol::ThreadRuntimeState::Waiting => AgentStatus::AwaitingInput,
            omini_protocol::ThreadRuntimeState::Working
            | omini_protocol::ThreadRuntimeState::Compacting => AgentStatus::Working,
        };
        // RuntimeStatus 是连接同步事实；compact 可能由其它客户端发起，不能标记为本地 manual compact。
        let paused = activity.kind == omini_protocol::ThreadRuntimeActivityKind::Query
            && (state == omini_protocol::ThreadRuntimeState::Waiting || !pending_pauses.is_empty());

        self.sessions.views["main"].manual_compact_running = false;
        self.sync_run_timer(Duration::from_millis(activity.elapsed_ms), paused);
        self.sessions.views["main"].agent_status = agent_status;
    }

    fn sync_pending_tool_pauses(&mut self, pending_pauses: Vec<omini_protocol::ToolPauseRequest>) {
        self.dialogs.pending_tool_pauses = pending_pauses.into_iter().map(Into::into).collect();
        if self.dialogs.pending_tool_pauses.is_empty() {
            self.reset_permission_drawer();
            return;
        }
        self.prepare_active_tool_pause();
        self.sessions.views["main"].agent_status = AgentStatus::AwaitingInput;
    }

    fn sync_pending_plan_approval(&mut self, pending: Option<omini_protocol::PlanSubmittedEvent>) {
        match pending {
            Some(plan) => self.open_plan_approval(SubmittedPlan {
                id: plan.plan_id,
                title: plan.title,
                markdown: plan.markdown,
                path: PathBuf::new(),
                created_at: Timestamp::now(),
            }),
            None => self.clear_plan_approval(),
        }
    }

    pub fn is_run_timer_paused(&self) -> bool {
        self.sessions.views["main"]
            .run_timer
            .as_ref()
            .is_some_and(RunTimer::is_paused)
    }

    pub fn clear_selectable_screen_lines(&mut self) {
        self.geometry.selectable_screen_lines.clear();
    }

    pub fn register_selectable_screen_line(
        &mut self,
        row: u16,
        col: u16,
        width: u16,
        text: String,
    ) {
        if width == 0 {
            return;
        }
        self.geometry
            .selectable_screen_lines
            .push(SelectableScreenLine {
                row,
                col,
                width,
                text,
            });
    }

    /// 结算当前思考计时段：把起点至今的耗时写入 `pending_assistant` 中
    /// 最后一个未计时的 Thinking 块，并关闭计时。
    /// 在首个非思考内容（text/tool/plan）到达或回合结束时调用；
    /// 本地结算值用于流式显示，历史重载以 engine 测量的持久化值为准。
    pub fn settle_active_thinking_segment(&mut self) {
        let Some(started_at) = self.sessions.views["main"].thinking_started_at.take() else {
            return;
        };
        let duration_ms = u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX);
        if let Some(pending) = &mut self.sessions.views["main"].pending_assistant {
            // 从后往前找当前正在累积（未计时）的 Thinking 块，
            // 已计时的块属于更早的思考段，不能覆盖。
            for block in pending.content.iter_mut().rev() {
                if let omini_model::message::ContentBlock::Thinking(tb) = block
                    && tb.duration_ms.is_none()
                {
                    tb.duration_ms = Some(duration_ms);
                    break;
                }
            }
        }
    }

    pub fn has_active_agent_tasks(&self) -> bool {
        self.sessions
            .subagents
            .values()
            .any(|node| matches!(node.status, TaskStatus::Running | TaskStatus::Cancelling))
    }

    /// 更新主线程后台任务计数；终态立即移除，不受子会话节点回收影响。
    pub fn track_background_task(&mut self, task_id: String, status: TaskStatus) {
        if status.is_terminal() {
            self.sessions.background_tasks.remove(&task_id);
        } else {
            self.sessions.background_tasks.insert(task_id, status);
        }
    }

    /// 主 thread 与当前视图均空闲时统计其他后台任务；查看子会话时不计入自身。
    pub fn background_wait_count(&self) -> usize {
        let active = self.sessions.active();
        if self.is_run_active()
            || self.sessions.views["main"].main_query_active
            || !matches!(active.agent_status, AgentStatus::Idle)
        {
            return 0;
        }

        let selected = self.sessions.active_session_task_id.as_deref();
        self.sessions
            .background_tasks
            .keys()
            .filter(|task_id| Some(task_id.as_str()) != selected)
            .count()
    }

    pub fn session_count(&self) -> usize {
        1 + self.sessions.subagent_order.len()
    }

    /// 判断当前子任务会话是否已结束并应保持只读。
    pub fn session_is_terminal(&self) -> bool {
        self.sessions
            .active_session_task_id
            .as_ref()
            .is_some_and(|task_id| {
                self.sessions
                    .subagents
                    .values()
                    .any(|node| node.task_id == *task_id && node.status.is_terminal())
            })
    }

    /// 移除已结束且当前未查看的直接异步子任务。
    pub fn prune_terminal_tasks(&mut self) {
        // 异步结束可能删掉高亮项之前的行，按任务 ID 恢复高亮而不切换当前视图。
        let highlighted_main = self.sessions.session_selection_index == 0;
        let highlighted_task_id = self
            .sessions
            .session_selection_index
            .checked_sub(1)
            .and_then(|index| self.sessions.subagent_order.get(index))
            .cloned();
        let removed = self
            .sessions
            .subagent_order
            .iter()
            .filter_map(|task_id| {
                let active = self.sessions.active_session_task_id.as_ref() == Some(task_id);
                let terminal = self
                    .sessions
                    .subagents
                    .values()
                    .any(|node| node.task_id == *task_id && node.status.is_terminal());
                (!active && terminal).then(|| task_id.clone())
            })
            .collect::<Vec<_>>();
        if removed.is_empty() {
            return;
        }

        let removed_threads = self
            .sessions
            .subagents
            .values()
            .filter(|node| removed.contains(&node.task_id))
            .map(|node| node.thread_id.clone())
            .collect::<HashSet<_>>();
        for node in self
            .sessions
            .subagents
            .values()
            .filter(|node| removed.contains(&node.task_id))
        {
            if let Some(duration) = node.duration {
                self.sessions
                    .subagent_completion_durations
                    .insert(node.task_id.clone(), duration);
            }
            // 节点回收后 send_message 历史条目仍需可读的目标标题。
            self.sessions
                .subagent_title_memory
                .insert(node.task_id.clone(), node.display_title().to_string());
        }
        self.sessions
            .subagent_order
            .retain(|task_id| !removed.contains(task_id));
        self.sessions
            .views
            .retain(|task_id, _| !removed.contains(task_id));
        self.sessions
            .subagents
            .retain(|_, node| !removed.contains(&node.task_id));
        self.sessions
            .subagents_by_tool_use
            .retain(|_, thread_id| !removed_threads.contains(thread_id));
        if self.sessions.subagent_order.is_empty() {
            self.sessions.session_selector_focused = false;
        }
        let selected = if self.sessions.session_selector_focused && highlighted_main {
            None
        } else {
            highlighted_task_id
                .as_ref()
                .filter(|_| self.sessions.session_selector_focused)
                .and_then(|task_id| {
                    self.sessions
                        .subagent_order
                        .iter()
                        .position(|id| id == task_id)
                })
                .or_else(|| {
                    self.sessions
                        .active_session_task_id
                        .as_ref()
                        .and_then(|task_id| {
                            self.sessions
                                .subagent_order
                                .iter()
                                .position(|id| id == task_id)
                        })
                })
        };
        self.sessions.session_selection_index = selected.map(|index| index + 1).unwrap_or(0);
    }
}

pub fn pick_start_screen_tip() -> String {
    let mut rng = rand::thread_rng();
    let idx = rng.gen_range(0..START_SCREEN_TIPS.len());
    START_SCREEN_TIPS[idx].to_string()
}

#[cfg(test)]
mod tests;

impl AppState {
    /// 布局反馈只更新当前会话的视口以及鼠标命中区域。
    pub fn apply_frame(&mut self, output: crate::ui::context::FrameState) {
        let session = match output.task_id.as_ref() {
            Some(id) => self.sessions.views.get_mut(id),
            None => Some(&mut self.sessions.views["main"]),
        };
        if let Some(session) = session {
            session.total_lines = output.viewport.total_lines;
            if let Some((start, lines)) = output.viewport.selectable_patch {
                session.selectable_message_lines.truncate(start);
                session.selectable_message_lines.extend(lines);
            }
            session.message_scroll_y = output.viewport.message_scroll_y;
            session.scroll_offset = output.viewport.scroll_offset;
            session.auto_scroll = output.viewport.auto_scroll;
        }
        self.geometry = output.geometry;
        self.dialogs.permission.permission_scroll_offset = output.drawer_scroll_offset;
    }
}

pub use crate::features::composer::model::{
    DEFAULT_INPUT_WRAP_WIDTH, InputImageAttachment, InputPasteMarker, InputVisualLine,
    MAX_INPUT_VISIBLE_LINES, PASTE_MARKER_THRESHOLD_CHARS, PASTE_MARKER_THRESHOLD_NEWLINES,
};
pub use crate::features::help::state::{HelpDrawerState, HelpTab};
pub use crate::features::sessions::model::{AgentStatus, SubagentNode};
pub use crate::features::status::state::StatusBar;
pub use crate::features::timeline::model::{UiMessage, UiSystemEvent};
pub use crate::ui::selection_model::{SelectableScreenLine, SelectionPoint, TextSelection};

pub use crate::features::sessions::model::SessionState;
