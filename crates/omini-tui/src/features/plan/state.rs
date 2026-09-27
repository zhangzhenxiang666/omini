use crate::app::event::*;

#[derive(Debug, Default)]
pub struct PlanState {
    pub queued: std::collections::VecDeque<SubmittedPlan>,
    /// 待审批的计划。
    pub plan_approval: Option<SubmittedPlan>,
    /// 计划审批抽屉当前选中的操作。
    pub plan_approval_selected: usize,
    /// 计划审批抽屉是否使用 Auto 模式执行。
    pub plan_approval_auto: bool,
}
