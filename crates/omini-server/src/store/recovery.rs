use super::Store;
use crate::store::StoreError;
use jiff::Timestamp;
use omini_domain::agent_run::AgentRunStatus;
use omini_domain::task::TaskStatus;
use omini_entity::{AgentRun, AgentStep, AgentTask, BackgroundTask, ToolUseExecution};
use std::collections::HashMap;

impl Store {
    /// 启动状态归一化:把上次服务退出时未结束的任务与 Run 收敛为终态。
    ///
    /// 规则与 sqlx 时代的顺序化 UPDATE 集合等价——先收敛任务表,再依据
    /// (归一后的)Run 状态收敛 Step 与 ToolUse。load-check-update 全部
    /// 发生在同一事务内;连接池为单连接,天然排除并发写。
    pub(super) async fn recover_interrupted_state(&self) -> Result<(), StoreError> {
        let now = Timestamp::now();
        let mut conn = self.conn();
        let mut tx = conn.transaction().await?;

        // 1. 任务表:未结束任务一律按"被中断"收敛;完成时间保留已有值。
        let running_tasks = AgentTask::filter(
            AgentTask::fields()
                .status()
                .in_list(vec![TaskStatus::Running, TaskStatus::Cancelling]),
        )
        .exec(&mut tx)
        .await?;
        for mut task in running_tasks {
            let completed_at = task.completed_at.or(Some(now));
            toasty::update!(task {
                status: TaskStatus::Interrupted,
                completed_at,
                updated_at: now,
            })
            .exec(&mut tx)
            .await?;
        }
        let running_background = BackgroundTask::filter(
            BackgroundTask::fields()
                .status()
                .in_list(vec![TaskStatus::Running, TaskStatus::Cancelling]),
        )
        .exec(&mut tx)
        .await?;
        for mut task in running_background {
            let completed_at = task.completed_at.or(Some(now));
            toasty::update!(task {
                status: TaskStatus::Interrupted,
                completed_at,
                updated_at: now,
            })
            .exec(&mut tx)
            .await?;
        }

        // 2. Run 表:仅非终态 Run 参与归一。规则按原顺序化语句的优先级
        //    首个命中生效:
        //    a) id 对应已中断任务(含历史会话遗留)→ interrupted;
        //    b) 无父 Run 且 running → interrupted;
        //    c) 有父 Run 且仍活跃(queued/running/waiting_approval)→ cancelled;
        //    d) 无父 Run 且 queued → interrupted。
        let interrupted_task_ids: Vec<String> =
            AgentTask::filter(AgentTask::fields().status().eq(TaskStatus::Interrupted))
                .select(AgentTask::fields().task_id())
                .exec(&mut tx)
                .await?;
        let live_runs = AgentRun::filter(AgentRun::fields().status().in_list(vec![
            AgentRunStatus::Queued,
            AgentRunStatus::Running,
            AgentRunStatus::WaitingApproval,
        ]))
        .exec(&mut tx)
        .await?;
        for mut run in live_runs {
            let has_parent = run.parent_run_id.is_some();
            let next_status = if interrupted_task_ids.contains(&run.id)
                || (!has_parent
                    && matches!(run.status, AgentRunStatus::Running | AgentRunStatus::Queued))
            {
                AgentRunStatus::Interrupted
            } else if has_parent {
                AgentRunStatus::Cancelled
            } else {
                // 无父 Run 且 waiting_approval:保持等待审批,可继续审批流程。
                continue;
            };
            let finished_at = run.finished_at.or(Some(now));
            toasty::update!(run {
                status: next_status,
                finished_at,
            })
            .exec(&mut tx)
            .await?;
        }

        // 3. Step 表:依据归一后的 Run 状态收敛。
        //    a) step=running 且 run ∈ {interrupted, cancelled} → interrupted;
        //    b) step=running 且 run=waiting_approval(无父)→ waiting_approval;
        //    c) step=waiting_approval 且 run=cancelled(有父)→ cancelled。
        let live_steps = AgentStep::filter(AgentStep::fields().status().in_list(vec![
            omini_domain::agent_run::AgentStepStatus::Running,
            omini_domain::agent_run::AgentStepStatus::WaitingApproval,
        ]))
        .exec(&mut tx)
        .await?;
        let run_ids: Vec<String> = live_steps.iter().map(|step| step.run_id.clone()).collect();
        let runs_by_id = if run_ids.is_empty() {
            HashMap::new()
        } else {
            AgentRun::filter(AgentRun::fields().id().in_list(run_ids))
                .exec(&mut tx)
                .await?
                .into_iter()
                .map(|run| (run.id.clone(), run))
                .collect::<HashMap<_, _>>()
        };
        for mut step in live_steps {
            let Some(run) = runs_by_id.get(&step.run_id) else {
                continue;
            };
            let next = if step.status == omini_domain::agent_run::AgentStepStatus::Running {
                match run.status {
                    AgentRunStatus::Interrupted | AgentRunStatus::Cancelled => {
                        Some(omini_domain::agent_run::AgentStepStatus::Interrupted)
                    }
                    AgentRunStatus::WaitingApproval if run.parent_run_id.is_none() => {
                        Some(omini_domain::agent_run::AgentStepStatus::WaitingApproval)
                    }
                    _ => None,
                }
            } else if step.status == omini_domain::agent_run::AgentStepStatus::WaitingApproval
                && run.parent_run_id.is_some()
                && run.status == AgentRunStatus::Cancelled
            {
                Some(omini_domain::agent_run::AgentStepStatus::Cancelled)
            } else {
                None
            };
            let Some(next_status) = next else {
                continue;
            };
            if next_status == omini_domain::agent_run::AgentStepStatus::WaitingApproval {
                // 与旧语义一致:仅收敛状态,不写 finished_at(该步骤仍在等待审批)。
                toasty::update!(step {
                    status: next_status,
                })
                .exec(&mut tx)
                .await?;
            } else {
                let finished_at = step.finished_at.or(Some(now));
                toasty::update!(step {
                    status: next_status,
                    finished_at,
                })
                .exec(&mut tx)
                .await?;
            }
        }

        // 4. ToolUse:被取消的子 Agent Run 名下未终态工具调用一并取消。
        let cancelled_run_ids: Vec<String> = AgentRun::filter(
            AgentRun::fields()
                .parent_run_id()
                .is_some()
                .and(AgentRun::fields().status().eq(AgentRunStatus::Cancelled)),
        )
        .select(AgentRun::fields().id())
        .exec(&mut tx)
        .await?;
        if !cancelled_run_ids.is_empty() {
            let step_ids: Vec<String> =
                AgentStep::filter(AgentStep::fields().run_id().in_list(cancelled_run_ids))
                    .select(AgentStep::fields().id())
                    .exec(&mut tx)
                    .await?;
            if !step_ids.is_empty() {
                // 仅收敛未终态的工具调用;已完成/已失败的历史记录必须原样保留,
                // 否则每次重启都会破坏性改写历史。
                let executions = ToolUseExecution::filter(
                    ToolUseExecution::fields().step_id().in_list(step_ids).and(
                        ToolUseExecution::fields().status().in_list(vec![
                            omini_domain::agent_run::ToolUseStatus::Pending,
                            omini_domain::agent_run::ToolUseStatus::WaitingApproval,
                            omini_domain::agent_run::ToolUseStatus::Running,
                        ]),
                    ),
                )
                .exec(&mut tx)
                .await?;
                for mut execution in executions {
                    toasty::update!(execution {
                        status: omini_domain::agent_run::ToolUseStatus::Cancelled,
                        updated_at: now,
                    })
                    .exec(&mut tx)
                    .await?;
                }
            }
        }

        tx.commit().await?;
        self.fail_pending_deliveries("子任务因服务重启中断").await?;
        Ok(())
    }
}
