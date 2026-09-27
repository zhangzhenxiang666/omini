use crate::app::event::*;
use crate::app::state::*;
use std::collections::VecDeque;

#[derive(Debug, Default)]
pub struct DialogsState {
    pub permission: crate::features::permissions::model::PermissionState,
    pub ask: crate::features::questions::state::AskState,
    pub plan: crate::features::plan::state::PlanState,
    /// 等待用户确认/输入的工具暂停队列，按到达顺序处理。
    pub pending_tool_pauses: VecDeque<ToolPauseRequest>,
    /// 待处理的交互请求（非 None 时渲染选择页）
    pub interaction_request: Option<InteractionRequest>,
    /// 交互选择页的当前步骤与选中索引（TUI 本地状态）
    pub interaction_step: Option<InteractionStep>,
    /// /help 底部抽屉状态。
    pub help_drawer: Option<HelpDrawerState>,
}
