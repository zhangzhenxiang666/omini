use super::*;
use omini_domain::conversation::{AgentMessage, ConversationEntry, SystemEvent};
use omini_runtime_contract::persistence::ClientMessage;
use omini_runtime_contract::thread_domain::DeliveryKey;

impl Database {
    pub async fn projected_delivery_keys(
        &self,
        owner_thread_id: &str,
    ) -> Result<Vec<DeliveryKey>, StoreError> {
        let rows: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT task_id, source_kind, source_key FROM agent_task_delivery
             WHERE owner_thread_id = ?",
        )
        .bind(owner_thread_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(task_id, source_kind, source_key)| DeliveryKey {
                task_id,
                source_kind,
                source_key,
            })
            .collect())
    }
    /// 返回是否首次登记；已有相同来源和内容的请求不应再次入队。
    pub async fn enqueue_client_message(
        &self,
        task_id: &str,
        owner_thread_id: &str,
        agent_thread_id: &str,
        message: &ClientMessage,
    ) -> Result<bool, StoreError> {
        let source_key =
            DeliveryKey::from_client(task_id, &message.client_id, &message.client_echo_id)
                .source_key;
        let payload = serde_json::to_string(&message.input)?;
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO agent_task_delivery (
                task_id, source_kind, source_key, owner_thread_id, agent_thread_id,
                payload, status, created_at, updated_at
            ) VALUES (?, 'client', ?, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(task_id)
        .bind(&source_key)
        .bind(owner_thread_id)
        .bind(agent_thread_id)
        .bind(&payload)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if inserted {
            let entry = ConversationEntry::UserInput(message.input.clone());
            sqlx::query(
                "INSERT INTO messages(thread_id, role, model_ref, content, kind, created_at)
                 VALUES (?, 'user', NULL, ?, 'conversation_entry', ?)",
            )
            .bind(agent_thread_id)
            .bind(serde_json::to_string(&entry)?)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        } else {
            let existing: (String, String) = sqlx::query_as(
                "SELECT payload, status FROM agent_task_delivery WHERE task_id = ? AND source_kind = 'client' AND source_key = ?",
            )
            .bind(task_id)
            .bind(&source_key)
            .fetch_one(&mut *tx)
            .await?;
            if existing.0 != payload {
                return Err(StoreError::InvalidData(
                    "client message source key was reused with different content".to_string(),
                ));
            }
            if existing.1 == "failed" {
                return Err(StoreError::InvalidData(
                    "client message was already marked undeliverable".to_string(),
                ));
            }
        }
        tx.commit().await?;
        Ok(inserted)
    }

    pub async fn client_delivery(
        &self,
        task_id: &str,
        message: &ClientMessage,
    ) -> Result<Option<(String, String)>, StoreError> {
        sqlx::query_as(
            "SELECT payload, status FROM agent_task_delivery WHERE task_id = ? AND source_kind = 'client' AND source_key = ?",
        )
        .bind(task_id)
        .bind(DeliveryKey::from_client(task_id, &message.client_id, &message.client_echo_id).source_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn fail_client_message(
        &self,
        task_id: &str,
        message: &ClientMessage,
        reason: &str,
    ) -> Result<u32, StoreError> {
        let key = DeliveryKey::from_client(task_id, &message.client_id, &message.client_echo_id);
        self.settle_deliveries(Some(task_id), Some("client"), Some(&key.source_key), reason)
            .await
            .map(|counts| counts.first().map_or(0, |(_, count)| *count))
    }
    pub async fn enqueue_agent_message(
        &self,
        task_id: &str,
        owner_thread_id: &str,
        agent_thread_id: &str,
        message: &AgentMessage,
    ) -> Result<bool, StoreError> {
        let source_key =
            DeliveryKey::from_agent(task_id, &message.source_run_id, &message.tool_use_id)
                .source_key;
        let payload = serde_json::to_string(message)?;
        let now = Utc::now();
        let mut tx = self.pool.begin().await?;
        let inserted = sqlx::query(
            "INSERT OR IGNORE INTO agent_task_delivery (
                task_id, source_kind, source_key, owner_thread_id, agent_thread_id,
                payload, status, created_at, updated_at
            ) VALUES (?, 'agent', ?, ?, ?, ?, 'pending', ?, ?)",
        )
        .bind(task_id)
        .bind(&source_key)
        .bind(owner_thread_id)
        .bind(agent_thread_id)
        .bind(&payload)
        .bind(now)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected()
            == 1;
        if inserted {
            let entry = ConversationEntry::SystemEvent(SystemEvent::AgentMessage(message.clone()));
            sqlx::query(
                "INSERT INTO messages(thread_id, role, model_ref, content, kind, created_at)
                 VALUES (?, 'user', NULL, ?, 'conversation_entry', ?)",
            )
            .bind(agent_thread_id)
            .bind(serde_json::to_string(&entry)?)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        } else {
            let existing: (String, String) = sqlx::query_as(
                "SELECT payload, status FROM agent_task_delivery WHERE task_id = ? AND source_kind = 'agent' AND source_key = ?",
            )
            .bind(task_id)
            .bind(&source_key)
            .fetch_one(&mut *tx)
            .await?;
            if existing.0 != payload {
                return Err(StoreError::InvalidData(
                    "agent message source key was reused with different content".to_string(),
                ));
            }
            if existing.1 == "failed" {
                return Err(StoreError::InvalidData(
                    "agent message was already marked undeliverable".to_string(),
                ));
            }
        }
        tx.commit().await?;
        Ok(inserted)
    }

    /// 模型历史和投递状态必须原子提交；重复注入不追加第二条模型消息。
    pub async fn inject_task_message(
        &self,
        agent_thread_id: &str,
        key: &DeliveryKey,
        model_message: &Message,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        let status: String = sqlx::query_scalar(
            "SELECT status FROM agent_task_delivery WHERE task_id = ? AND source_kind = ? AND source_key = ? AND agent_thread_id = ?",
        )
        .bind(&key.task_id)
        .bind(&key.source_kind)
        .bind(&key.source_key)
        .bind(agent_thread_id)
        .fetch_one(&mut *tx)
        .await?;
        if status == "injected" {
            return Ok(());
        }
        if status != "pending" {
            return Err(StoreError::InvalidData(
                "task message cannot be injected after delivery failed".to_string(),
            ));
        }
        let version: i64 =
            sqlx::query_scalar("SELECT llm_context_version FROM thread WHERE id = ?")
                .bind(agent_thread_id)
                .fetch_one(&mut *tx)
                .await?;
        let ordinal: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM llm_messages WHERE thread_id = ? AND context_version = ?",
        )
        .bind(agent_thread_id)
        .bind(version)
        .fetch_one(&mut *tx)
        .await?;
        let now = Utc::now();
        sqlx::query(
            "INSERT INTO llm_messages(thread_id, context_version, ordinal, role, content, created_at) VALUES (?, ?, ?, 'user', ?, ?)",
        )
        .bind(agent_thread_id)
        .bind(version)
        .bind(ordinal)
        .bind(serde_json::to_string(&model_message.content)?)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE agent_task_delivery SET status = 'injected', updated_at = ? WHERE task_id = ? AND source_kind = ? AND source_key = ?",
        )
        .bind(now)
        .bind(&key.task_id)
        .bind(&key.source_kind)
        .bind(&key.source_key)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn fail_task_messages(&self, task_id: &str, reason: &str) -> Result<u32, StoreError> {
        let counts = self
            .settle_deliveries(Some(task_id), None, None, reason)
            .await?;
        Ok(counts.first().map_or(0, |(_, count)| *count))
    }

    pub async fn fail_pending_deliveries(&self, reason: &str) -> Result<(), StoreError> {
        self.settle_deliveries(None, None, None, reason)
            .await
            .map(|_| ())
    }

    /// 结算待投递记录并返回每个任务累计失败数；终态结果据此给出摘要。
    async fn settle_deliveries(
        &self,
        only_task_id: Option<&str>,
        source_kind: Option<&str>,
        source_key: Option<&str>,
        reason: &str,
    ) -> Result<Vec<(String, u32)>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT task_id FROM agent_task_delivery
             WHERE status = 'pending' AND (? IS NULL OR task_id = ?)
               AND (? IS NULL OR source_kind = ?) AND (? IS NULL OR source_key = ?)",
        )
        .bind(only_task_id)
        .bind(only_task_id)
        .bind(source_kind)
        .bind(source_kind)
        .bind(source_key)
        .bind(source_key)
        .fetch_all(&mut *tx)
        .await?;
        let now = Utc::now();
        sqlx::query(
            "UPDATE agent_task_delivery SET status = 'failed', failure_reason = ?, updated_at = ?
             WHERE status = 'pending' AND (? IS NULL OR task_id = ?)
               AND (? IS NULL OR source_kind = ?) AND (? IS NULL OR source_key = ?)",
        )
        .bind(reason)
        .bind(now)
        .bind(only_task_id)
        .bind(only_task_id)
        .bind(source_kind)
        .bind(source_kind)
        .bind(source_key)
        .bind(source_key)
        .execute(&mut *tx)
        .await?;
        let mut tasks = rows;
        if let Some(task_id) = only_task_id
            && !tasks.iter().any(|id| id == task_id)
        {
            tasks.push(task_id.to_string());
        }
        let mut counts = Vec::with_capacity(tasks.len());
        for task_id in tasks {
            let count: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM agent_task_delivery WHERE task_id = ? AND status = 'failed'",
            )
            .bind(&task_id)
            .fetch_one(&mut *tx)
            .await?;
            let count = u32::try_from(count)
                .map_err(|_| StoreError::InvalidData("delivery count overflow".to_string()))?;
            if only_task_id.is_none() && count > 0 {
                let existing: Option<String> =
                    sqlx::query_scalar("SELECT result_json FROM agent_task WHERE task_id = ?")
                        .bind(&task_id)
                        .fetch_one(&mut *tx)
                        .await?;
                let mut result =
                    existing
                        .map(|json| {
                            serde_json::from_str::<
                                omini_runtime_contract::thread_domain::AgentTaskResult,
                            >(&json)
                        })
                        .transpose()?
                        .unwrap_or(omini_runtime_contract::thread_domain::AgentTaskResult {
                            output: None,
                            error: None,
                            warnings: Vec::new(),
                            undelivered_messages: None,
                        });
                result.undelivered_messages = Some(count);
                sqlx::query("UPDATE agent_task SET result_json = ? WHERE task_id = ?")
                    .bind(serde_json::to_string(&result)?)
                    .bind(&task_id)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query("UPDATE background_task SET result_summary = ? WHERE task_id = ?")
                    .bind(format!("{count} 条消息未进入子 Agent 模型上下文"))
                    .bind(&task_id)
                    .execute(&mut *tx)
                    .await?;
            }
            counts.push((task_id, count));
        }
        tx.commit().await?;
        Ok(counts)
    }
}
