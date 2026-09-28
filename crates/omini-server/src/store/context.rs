use super::Store;
use crate::store::StoreError;
use jiff::Timestamp;
use omini_config::project::ThreadDir;
use omini_entity::{LlmMessage, Thread, cleanup_created_files, prepare_blocks};
use omini_model::message::Message;
use toasty::db::Executor;

/// 计算线程当前上下文版本的下一个 ordinal。
/// toasty 没有 MAX 聚合,用降序投影取首行等价替代 `COALESCE(MAX(ordinal) + 1, 0)`。
pub(super) async fn next_llm_ordinal(
    executor: &mut impl Executor,
    thread_id: &str,
    version: i64,
) -> Result<i64, StoreError> {
    let max = LlmMessage::filter(
        LlmMessage::fields()
            .thread_id()
            .eq(thread_id)
            .and(LlmMessage::fields().context_version().eq(version)),
    )
    .select(LlmMessage::fields().ordinal())
    .order_by(LlmMessage::fields().ordinal().desc())
    .first()
    .exec(executor)
    .await?;
    Ok(max.map_or(0, |ordinal| ordinal + 1))
}

impl Store {
    pub async fn append_llm_message(
        &self,
        thread_id: &str,
        message: &Message,
        created_at: Timestamp,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let prepared = prepare_blocks(&message.content, thread_dir)?;
        let content = serde_json::to_string(&prepared.values)?;
        let result: Result<(), StoreError> = async {
            let mut conn = self.conn();
            let mut tx = conn.transaction().await?;
            let version = current_context_version(&mut tx, thread_id).await?;
            let ordinal = next_llm_ordinal(&mut tx, thread_id, version).await?;
            toasty::create!(LlmMessage {
                thread_id: thread_id.to_string(),
                context_version: version,
                ordinal,
                role: message.role,
                content,
                created_at: created_at,
            })
            .exec(&mut tx)
            .await?;
            tx.commit().await?;
            Ok(())
        }
        .await;
        // 事务内任一步失败(含提交失败)都要清理本次落盘的 sidecar/资产文件。
        if result.is_err() {
            cleanup_created_files(&prepared.created_files);
        }
        result
    }

    /// 乐观并发替换上下文:事务内校验版本、写入新版本、递增线程版本,
    /// 任一步与预期不符即以冲突错误回滚。
    pub async fn replace_llm_context(
        &self,
        thread_id: &str,
        expected_version: i64,
        messages: &[Message],
        created_at: Timestamp,
        thread_dir: &ThreadDir,
    ) -> Result<i64, StoreError> {
        let mut prepared_messages = Vec::with_capacity(messages.len());
        let mut created_files = Vec::new();
        for message in messages {
            match prepare_blocks(&message.content, thread_dir) {
                Ok(prepared) => {
                    created_files.extend(prepared.created_files);
                    prepared_messages.push((message.role, prepared.values));
                }
                Err(error) => {
                    cleanup_created_files(&created_files);
                    return Err(error);
                }
            }
        }

        let result: Result<i64, StoreError> = async {
            let mut conn = self.conn();
            let mut tx = conn.transaction().await?;
            let mut thread = Thread::filter_by_id(thread_id)
                .first()
                .exec(&mut tx)
                .await?
                .ok_or_else(|| StoreError::MissingRow(format!("thread '{thread_id}'")))?;
            if thread.llm_context_version != expected_version {
                return Err(StoreError::ContextVersionConflict {
                    expected: expected_version,
                    actual: thread.llm_context_version,
                });
            }
            let next_version = expected_version + 1;
            for (ordinal, (role, blocks)) in prepared_messages.iter().enumerate() {
                toasty::create!(LlmMessage {
                    thread_id: thread_id.to_string(),
                    context_version: next_version,
                    ordinal: i64::try_from(ordinal).unwrap_or(i64::MAX),
                    role: *role,
                    content: serde_json::to_string(blocks)?,
                    created_at: created_at,
                })
                .exec(&mut tx)
                .await?;
            }
            thread.llm_context_version = next_version;
            toasty::update!(thread {
                llm_context_version: next_version
            })
            .exec(&mut tx)
            .await?;
            tx.commit().await?;
            Ok(next_version)
        }
        .await;

        if result.is_err() {
            cleanup_created_files(&created_files);
        }
        result
    }

    pub async fn load_current_llm_messages(
        &self,
        thread_id: &str,
        thread_dir: &ThreadDir,
    ) -> Result<Vec<Message>, StoreError> {
        self.load_current_llm_context(thread_id, thread_dir)
            .await
            .map(|(messages, _)| messages)
    }

    pub(crate) async fn load_current_llm_context(
        &self,
        thread_id: &str,
        thread_dir: &ThreadDir,
    ) -> Result<(Vec<Message>, i64), StoreError> {
        loop {
            let mut db = self.conn();
            let mut thread = Thread::filter_by_id(thread_id)
                .first()
                .exec(&mut db)
                .await?
                .ok_or_else(|| StoreError::MissingRow(format!("thread '{thread_id}'")))?;
            let context_version = thread.llm_context_version;
            let rows = LlmMessage::filter(
                LlmMessage::fields()
                    .thread_id()
                    .eq(thread_id)
                    .and(LlmMessage::fields().context_version().eq(context_version)),
            )
            .order_by(LlmMessage::fields().ordinal().asc())
            .exec(&mut db)
            .await?;

            let mut messages = Vec::with_capacity(rows.len());
            let mut oversized = None;
            for row in rows {
                let stored = serde_json::from_str::<Vec<serde_json::Value>>(&row.content)?;
                match omini_entity::load_blocks(&stored, thread_dir) {
                    Ok(blocks) => messages.push(Message::new(row.role, blocks)),
                    Err(StoreError::OversizedSidecar {
                        actual_bytes,
                        limit_bytes,
                    }) => {
                        oversized = Some((actual_bytes, limit_bytes));
                        break;
                    }
                    Err(error) => return Err(error),
                }
            }

            let Some((actual_bytes, limit_bytes)) = oversized else {
                return Ok((messages, context_version));
            };

            // 保留旧版本供检查,同时切换活动上下文版本,
            // 避免后续请求反复遇到同一大文件。
            if thread.llm_context_version == context_version {
                thread.llm_context_version = context_version + 1;
                toasty::update!(thread {
                    llm_context_version: context_version + 1
                })
                .exec(&mut db)
                .await?;
                tracing::warn!(
                    thread_id,
                    actual_bytes,
                    limit_bytes,
                    "cleared active LLM context containing an oversized sidecar"
                );
                return Ok((Vec::new(), context_version + 1));
            }
            // 版本已被并发替换:重读新版本。
        }
    }
}

/// 读取线程当前上下文版本;线程必须存在,缺行按内部错误处理。
async fn current_context_version(
    executor: &mut impl Executor,
    thread_id: &str,
) -> Result<i64, StoreError> {
    Thread::filter_by_id(thread_id)
        .first()
        .exec(executor)
        .await?
        .map(|thread| thread.llm_context_version)
        .ok_or_else(|| StoreError::MissingRow(format!("thread '{thread_id}'")))
}
