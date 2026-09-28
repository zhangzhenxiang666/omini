use super::Store;
use crate::store::StoreError;
use jiff::Timestamp;
use omini_config::project::ProjectDir;
use omini_domain::usage::Usage;
use omini_entity::{
    AgentRun, AgentStep, AgentTask, AgentTaskDelivery, Attachment, BackgroundTask, LlmMessage,
    Message, Thread, ToolUseExecution,
};
use std::fs;

impl Store {
    pub async fn create_thread(&self, thread: &Thread) -> Result<(), StoreError> {
        let mut db = self.conn();
        toasty::create!(Thread {
            id: thread.id.clone(),
            project_id: thread.project_id.clone(),
            parent_thread_id: thread.parent_thread_id.clone(),
            spawn_tool_use_id: thread.spawn_tool_use_id.clone(),
            thread_type: thread.thread_type.clone(),
            agent_label: thread.agent_label.clone(),
            provider: thread.provider.clone(),
            model: thread.model.clone(),
            thinking_effort: thread.thinking_effort.clone(),
            title: thread.title.clone(),
            current_context_tokens: thread.current_context_tokens,
            total_tokens: thread.total_tokens,
            total_cached_tokens: thread.total_cached_tokens,
            llm_context_version: thread.llm_context_version,
            created_at: thread.created_at,
            updated_at: thread.updated_at,
        })
        .exec(&mut db)
        .await?;
        Ok(())
    }

    pub async fn get_thread(&self, id: &str) -> Result<Option<Thread>, StoreError> {
        let mut db = self.conn();
        Ok(Thread::filter_by_id(id).first().exec(&mut db).await?)
    }

    pub async fn list_threads(&self, project_id: &str) -> Result<Vec<Thread>, StoreError> {
        let mut db = self.conn();
        Ok(Thread::filter(
            Thread::fields()
                .project_id()
                .eq(project_id)
                .and(Thread::fields().thread_type().eq("main")),
        )
        .order_by((
            Thread::fields().updated_at().desc(),
            Thread::fields().created_at().desc(),
        ))
        .exec(&mut db)
        .await?)
    }

    pub async fn list_child_threads(&self, parent_id: &str) -> Result<Vec<Thread>, StoreError> {
        let mut db = self.conn();
        Ok(Thread::filter_by_parent_thread_id(parent_id)
            .order_by(Thread::fields().created_at().asc())
            .exec(&mut db)
            .await?)
    }

    pub async fn record_thread_usage(&self, id: &str, usage: Usage) -> Result<(), StoreError> {
        let total_tokens = usage_tokens_i64(usage);
        let cached_tokens = usage_usize_to_i64(usage.cached_tokens);
        let mut db = self.conn();
        // 缺行时静默无操作,与原 UPDATE 未命中即无操作的行为一致;
        // updated_at 由模型声明自动触碰。
        if let Some(mut thread) = Thread::filter_by_id(id).first().exec(&mut db).await? {
            toasty::update!(thread {
                current_context_tokens: total_tokens,
                total_tokens.add(total_tokens),
                total_cached_tokens.add(cached_tokens),
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }

    pub async fn record_thread_total_usage(
        &self,
        id: &str,
        usage: Usage,
    ) -> Result<(), StoreError> {
        let mut db = self.conn();
        if let Some(mut thread) = Thread::filter_by_id(id).first().exec(&mut db).await? {
            toasty::update!(thread {
                total_tokens.add(usage_tokens_i64(usage)),
                total_cached_tokens.add(usage_usize_to_i64(usage.cached_tokens)),
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }

    pub async fn update_thread_updated_at(&self, id: &str) -> Result<(), StoreError> {
        let mut db = self.conn();
        if let Some(mut thread) = Thread::filter_by_id(id).first().exec(&mut db).await? {
            toasty::update!(thread {
                updated_at: Timestamp::now()
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }

    pub async fn update_thread_config(
        &self,
        id: &str,
        provider: &str,
        model: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), StoreError> {
        let mut db = self.conn();
        if let Some(mut thread) = Thread::filter_by_id(id).first().exec(&mut db).await? {
            toasty::update!(thread {
                provider: provider.to_string(),
                model: model.to_string(),
                thinking_effort: thinking_effort.map(ToString::to_string),
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }

    pub async fn update_thread_thinking_effort(
        &self,
        id: &str,
        thinking_effort: Option<&str>,
    ) -> Result<(), StoreError> {
        let mut db = self.conn();
        if let Some(mut thread) = Thread::filter_by_id(id).first().exec(&mut db).await? {
            toasty::update!(thread {
                thinking_effort: thinking_effort.map(ToString::to_string),
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }

    pub async fn update_thread_title(&self, id: &str, title: &str) -> Result<(), StoreError> {
        let mut db = self.conn();
        if let Some(mut thread) = Thread::filter_by_id(id).first().exec(&mut db).await? {
            toasty::update!(thread {
                title: Some(title.to_string())
            })
            .exec(&mut db)
            .await?;
        }
        Ok(())
    }

    /// 仅在标题为空且尚无任何消息时写入初始标题;返回是否首次写入。
    /// 读-判-写在单事务内完成,单连接池使并发写不可能穿插。
    pub async fn set_initial_thread_title(
        &self,
        id: &str,
        title: &str,
    ) -> Result<bool, StoreError> {
        let mut conn = self.conn();
        let mut tx = conn.transaction().await?;
        let Some(mut thread) = Thread::filter_by_id(id).first().exec(&mut tx).await? else {
            tx.commit().await?;
            return Ok(false);
        };
        let has_title = thread
            .title
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty());
        let has_messages = Message::filter_by_thread_id(id)
            .first()
            .exec(&mut tx)
            .await?
            .is_some();
        if has_title || has_messages {
            tx.commit().await?;
            return Ok(false);
        }
        toasty::update!(thread {
            title: Some(title.to_string()),
            updated_at: Timestamp::now(),
        })
        .exec(&mut tx)
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// 删除线程及其全部后代(数据行与目录)。
    ///
    /// 数据库层没有级联:先逐层收集后代线程集合,再在单事务内按依赖序
    /// 显式删除全部关联行(被引用的父表最后删)。任务行按
    /// owner/agent/parent 三个线程外键的并集删除——任务的派生线程
    /// 必在树内,该并集覆盖任务链上的全部后代任务。目录清理在事务
    /// 提交后进行。
    pub async fn delete_thread_tree(
        &self,
        thread_id: &str,
        project: &ProjectDir,
    ) -> Result<(), StoreError> {
        let mut db = self.conn();
        let ids = collect_descendant_threads(&mut db, thread_id).await?;
        if ids.is_empty() {
            return Ok(());
        }

        let mut tx = db.transaction().await?;
        let run_ids = AgentRun::filter(AgentRun::fields().thread_id().in_list(ids.clone()))
            .select(AgentRun::fields().id())
            .exec(&mut tx)
            .await?;
        if !run_ids.is_empty() {
            let step_ids = AgentStep::filter(AgentStep::fields().run_id().in_list(run_ids.clone()))
                .select(AgentStep::fields().id())
                .exec(&mut tx)
                .await?;
            if !step_ids.is_empty() {
                ToolUseExecution::filter(
                    ToolUseExecution::fields()
                        .step_id()
                        .in_list(step_ids.clone()),
                )
                .delete()
                .exec(&mut tx)
                .await?;
                AgentStep::filter(AgentStep::fields().run_id().in_list(run_ids.clone()))
                    .delete()
                    .exec(&mut tx)
                    .await?;
            }
        }
        let task_filter = |ids: &[String]| {
            AgentTask::fields()
                .owner_thread_id()
                .in_list(ids.to_vec())
                .or(AgentTask::fields().agent_thread_id().in_list(ids.to_vec()))
                .or(AgentTask::fields().parent_thread_id().in_list(ids.to_vec()))
        };
        let task_ids = AgentTask::filter(task_filter(&ids))
            .select(AgentTask::fields().task_id())
            .exec(&mut tx)
            .await?;
        if !task_ids.is_empty() {
            AgentTaskDelivery::filter(
                AgentTaskDelivery::fields()
                    .task_id()
                    .in_list(task_ids.clone()),
            )
            .delete()
            .exec(&mut tx)
            .await?;
        }
        AgentTask::filter(task_filter(&ids))
            .delete()
            .exec(&mut tx)
            .await?;
        BackgroundTask::filter(
            BackgroundTask::fields()
                .owner_thread_id()
                .in_list(ids.clone()),
        )
        .delete()
        .exec(&mut tx)
        .await?;
        AgentRun::filter(AgentRun::fields().thread_id().in_list(ids.clone()))
            .delete()
            .exec(&mut tx)
            .await?;
        Message::filter(Message::fields().thread_id().in_list(ids.clone()))
            .delete()
            .exec(&mut tx)
            .await?;
        LlmMessage::filter(LlmMessage::fields().thread_id().in_list(ids.clone()))
            .delete()
            .exec(&mut tx)
            .await?;
        Attachment::filter(Attachment::fields().thread_id().in_list(ids.clone()))
            .delete()
            .exec(&mut tx)
            .await?;
        Thread::filter(Thread::fields().id().in_list(ids.clone()))
            .delete()
            .exec(&mut tx)
            .await?;
        tx.commit().await?;

        for id in ids {
            let path = project.thread(&id).path().to_path_buf();
            if path.exists() {
                fs::remove_dir_all(path)?;
            }
        }
        Ok(())
    }
}

/// 逐层收集线程树全部后代(含根);根不存在时返回空集。
async fn collect_descendant_threads(
    db: &mut toasty::Db,
    thread_id: &str,
) -> Result<Vec<String>, StoreError> {
    let mut all = Vec::new();
    let mut frontier = Vec::new();
    if Thread::filter_by_id(thread_id)
        .first()
        .exec(db)
        .await?
        .is_some()
    {
        frontier.push(thread_id.to_string());
        all.push(thread_id.to_string());
    }
    while !frontier.is_empty() {
        let mut next = Thread::filter(
            Thread::fields()
                .parent_thread_id()
                .in_list(frontier.clone()),
        )
        .select(Thread::fields().id())
        .exec(db)
        .await?;
        frontier.clear();
        next.retain(|id| !all.contains(id));
        if next.is_empty() {
            break;
        }
        all.extend(next.iter().cloned());
        frontier = next;
    }
    Ok(all)
}

fn usage_tokens_i64(usage: Usage) -> i64 {
    usage_usize_to_i64(usage.total_tokens())
}

fn usage_usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}
