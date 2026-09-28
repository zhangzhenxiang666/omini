use super::Store;
use crate::store::StoreError;
use jiff::Timestamp;
use omini_domain::agent_run::{
    AgentRunSnapshot, AgentRunStatus, AgentStepSnapshot, AgentStepStatus, ToolUseExecutionSnapshot,
    ToolUseStatus,
};
use omini_entity::{AgentRun, AgentStep, ToolUseExecution};
use std::collections::HashMap;

// 模型与领域快照的字段一一对应,但协议层依赖领域类型而非存储模型,
// 映射集中在本层完成,路由侧维持领域→协议的 From 链。
fn run_snapshot(row: AgentRun) -> Result<AgentRunSnapshot, StoreError> {
    Ok(AgentRunSnapshot {
        id: row.id,
        thread_id: row.thread_id,
        parent_run_id: row.parent_run_id,
        status: row.status,
        created_at: row.created_at,
        started_at: row.started_at,
        finished_at: row.finished_at,
        total_tokens: row.total_tokens,
        archived_at: row.archived_at,
    })
}

fn step_snapshot(row: AgentStep) -> Result<AgentStepSnapshot, StoreError> {
    Ok(AgentStepSnapshot {
        id: row.id,
        run_id: row.run_id,
        step_no: u32::try_from(row.step_no)
            .map_err(|_| StoreError::InvalidData("invalid AgentStep number".to_string()))?,
        status: row.status,
        started_at: row.started_at,
        finished_at: row.finished_at,
        input_tokens: row.input_tokens,
        output_tokens: row.output_tokens,
    })
}

fn tool_use_snapshot(row: ToolUseExecution) -> Result<ToolUseExecutionSnapshot, StoreError> {
    Ok(ToolUseExecutionSnapshot {
        id: row.id,
        step_id: row.step_id,
        name: row.name,
        input: serde_json::from_str(&row.input_json)?,
        status: row.status,
        updated_at: row.updated_at,
    })
}

impl Store {
    pub async fn create_agent_run(&self, run: &AgentRunSnapshot) -> Result<(), StoreError> {
        let mut db = self.conn();
        toasty::create!(AgentRun {
            id: run.id.clone(),
            thread_id: run.thread_id.clone(),
            parent_run_id: run.parent_run_id.clone(),
            status: run.status,
            created_at: run.created_at,
            started_at: run.started_at,
            finished_at: run.finished_at,
            total_tokens: run.total_tokens,
            archived_at: run.archived_at,
        })
        .exec(&mut db)
        .await?;
        Ok(())
    }

    /// 时间戳保持 COALESCE 语义(只在首次/已有值时写入)、token 只增不减。
    /// 读-改-写在单连接池上等价于原单语句原子性;缺行时静默无操作。
    pub async fn update_agent_run(
        &self,
        run_id: &str,
        status: AgentRunStatus,
        started_at: Option<Timestamp>,
        finished_at: Option<Timestamp>,
        add_tokens: i64,
    ) -> Result<(), StoreError> {
        let mut db = self.conn();
        if let Some(mut run) = AgentRun::filter_by_id(run_id).first().exec(&mut db).await? {
            let next_started = run.started_at.or(started_at);
            let next_finished = finished_at.or(run.finished_at);
            toasty::update!(run {
                status: status,
                started_at: next_started,
                finished_at: next_finished,
                total_tokens.add(add_tokens),
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }

    pub async fn upsert_agent_step(&self, step: &AgentStepSnapshot) -> Result<(), StoreError> {
        let mut db = self.conn();
        // 冲突时只收敛状态与统计:run_id/step_no/started_at 是身份列,
        // 放 on_create 确保不进入 DO UPDATE SET(与原 SQL 列集合一致)。
        AgentStep::upsert_by_id(&step.id)
            .on_create(|create| {
                create
                    .run_id(step.run_id.clone())
                    .step_no(i64::from(step.step_no))
                    .started_at(step.started_at)
            })
            .status(step.status)
            .finished_at(step.finished_at)
            .input_tokens(step.input_tokens)
            .output_tokens(step.output_tokens)
            .exec(&mut db)
            .await?;
        Ok(())
    }

    /// 状态收敛 + token 累加;finished_at 保持 COALESCE 语义。
    pub async fn update_agent_step(
        &self,
        step_id: &str,
        status: AgentStepStatus,
        finished_at: Option<Timestamp>,
        add_input_tokens: i64,
        add_output_tokens: i64,
    ) -> Result<(), StoreError> {
        let mut db = self.conn();
        if let Some(mut step) = AgentStep::filter_by_id(step_id)
            .first()
            .exec(&mut db)
            .await?
        {
            let next_finished = finished_at.or(step.finished_at);
            toasty::update!(step {
                status: status,
                finished_at: next_finished,
                input_tokens.add(add_input_tokens),
                output_tokens.add(add_output_tokens),
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }

    pub async fn upsert_tool_use_execution(
        &self,
        item: &ToolUseExecutionSnapshot,
        status: ToolUseStatus,
    ) -> Result<(), StoreError> {
        let mut db = self.conn();
        let input_json = serde_json::to_string(&item.input)?;
        // 冲突时只更新状态与更新时间;step_id/name/input_json 是首写事实,
        // 放 on_create 保持原 SQL 的列集合语义。
        ToolUseExecution::upsert_by_id(&item.id)
            .on_create(|create| {
                create
                    .step_id(item.step_id.clone())
                    .name(item.name.clone())
                    .input_json(input_json)
            })
            .status(status)
            .updated_at(item.updated_at)
            .exec(&mut db)
            .await?;
        Ok(())
    }

    /// 仅终态 Run 可归档;读-判-写在单连接池上等价于原条件更新。
    pub async fn set_agent_run_archived(
        &self,
        run_id: &str,
        archived_at: Option<Timestamp>,
    ) -> Result<bool, StoreError> {
        let mut db = self.conn();
        let Some(mut run) = AgentRun::filter_by_id(run_id).first().exec(&mut db).await? else {
            return Ok(false);
        };
        if !run.status.is_terminal() {
            return Ok(false);
        }
        toasty::update!(run { archived_at }).exec(&mut db).await?;
        Ok(true)
    }

    pub async fn list_agent_runs(
        &self,
        thread_id: &str,
        include_archived: bool,
    ) -> Result<Vec<AgentRunSnapshot>, StoreError> {
        let mut db = self.conn();
        let mut rows = AgentRun::filter_by_thread_id(thread_id)
            .order_by((
                AgentRun::fields().created_at().asc(),
                AgentRun::fields().id().asc(),
            ))
            .exec(&mut db)
            .await?;
        if !include_archived {
            rows.retain(|row| row.archived_at.is_none());
        }
        rows.into_iter().map(run_snapshot).collect()
    }

    pub async fn get_agent_run(
        &self,
        thread_id: &str,
        run_id: &str,
    ) -> Result<Option<AgentRunSnapshot>, StoreError> {
        let mut db = self.conn();
        // 主键点查后比对归属线程,等价于原 WHERE thread_id = ? AND id = ?。
        let row = AgentRun::filter_by_id(run_id)
            .first()
            .exec(&mut db)
            .await?
            .filter(|row| row.thread_id == thread_id);
        row.map(run_snapshot).transpose()
    }

    pub async fn list_agent_steps(
        &self,
        run_id: &str,
    ) -> Result<Vec<AgentStepSnapshot>, StoreError> {
        let mut db = self.conn();
        AgentStep::filter(AgentStep::fields().run_id().eq(run_id))
            .order_by(AgentStep::fields().step_no().asc())
            .exec(&mut db)
            .await?
            .into_iter()
            .map(step_snapshot)
            .collect()
    }

    pub async fn list_tool_use_executions(
        &self,
        run_id: &str,
    ) -> Result<Vec<ToolUseExecutionSnapshot>, StoreError> {
        let mut db = self.conn();
        let steps = AgentStep::filter(AgentStep::fields().run_id().eq(run_id))
            .order_by(AgentStep::fields().step_no().asc())
            .exec(&mut db)
            .await?;
        if steps.is_empty() {
            return Ok(Vec::new());
        }
        let step_ids: Vec<String> = steps.iter().map(|step| step.id.clone()).collect();
        // 全局按 (updated_at, id) 排序后,再按 Step 顺序做稳定排序;
        // 组内顺序保持 (updated_at, id),等价于原 JOIN 查询的
        // ORDER BY s.step_no, t.updated_at, t.id。
        let mut executions =
            ToolUseExecution::filter(ToolUseExecution::fields().step_id().in_list(step_ids))
                .order_by((
                    ToolUseExecution::fields().updated_at().asc(),
                    ToolUseExecution::fields().id().asc(),
                ))
                .exec(&mut db)
                .await?;
        let step_position: HashMap<&str, usize> = steps
            .iter()
            .enumerate()
            .map(|(index, step)| (step.id.as_str(), index))
            .collect();
        executions.sort_by_key(|execution| step_position[execution.step_id.as_str()]);
        executions.into_iter().map(tool_use_snapshot).collect()
    }
}
