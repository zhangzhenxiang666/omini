use std::{sync::Arc, time::Duration};

use crate::{
    event::bridge::{fallback_thread_title_from_user_input, thread_title_changed_protocol_event},
    thread::ThreadRuntime,
};
use omini_core::CoreError;
use omini_protocol as client_proto;
use tracing::Instrument;

impl ThreadRuntime {
    pub async fn rename_thread(&self, title: String) -> Result<(), CoreError> {
        self.db
            .update_thread_title(&self.thread_id, &title)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to rename thread", error.to_string())
            })?;
        self.broadcast_thread_title_changed(title)?;
        Ok(())
    }

    /// 同步落库 300 字符兜底 title。返回 `true` 表示这次实际写入了新
    /// title (供路由层据此决定是否 spawn 后台 LLM 升级任务);`false`
    /// 表示 text 为空、title 已被设置过或 thread 已经有 messages,SQL
    /// 软写条件被跳过。
    pub async fn set_initial_title_from_input(
        &self,
        input: &client_proto::UserInput,
    ) -> Result<bool, CoreError> {
        let Some(title) = fallback_thread_title_from_user_input(input) else {
            return Ok(false);
        };
        let updated = self
            .db
            .set_initial_thread_title(&self.thread_id, &title)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to set initial thread title", error.to_string())
            })?;
        if updated {
            self.broadcast_thread_title_changed(title)?;
        }
        Ok(updated)
    }

    /// 首条消息提交后，在后台异步用 `model_tiers.small` 生成一个更可读的
    /// 标题。`fallback_title` 是路由层刚刚同步落库的 300 字符兜底 title；
    /// LLM 跑完后只有当 DB 中当前 title 仍等于这个兜底（即用户在期间
    /// 没有 /rename、没有 fork 预设冲突），才用 `update_thread_title`
    /// 覆盖成 LLM 生成版本并广播，避免覆盖用户主动改名或 fork 预设。
    pub fn spawn_background_title_generation(
        self: &Arc<Self>,
        project_id: String,
        manager: Arc<crate::daemon::GlobalDaemonManager>,
        fallback_title: String,
        user_input: String,
    ) {
        let db = Arc::clone(&self.db);
        let thread_id = self.thread_id.clone();
        let span_thread_id = thread_id.clone();
        let inbox_tx = self.server_event_inbox_tx.clone();
        tokio::spawn(
            async move {
                let log_thread_id = &thread_id;
                // 1. 拉一次最新 settings，再调 LLM，带超时。
                let project = match manager.get_or_load_project(&project_id).await {
                    Ok(project) => project,
                    Err(error) => {
                        tracing::warn!(thread_id = %log_thread_id, ?error, "failed to load project");
                        return;
                    }
                };
                let settings = match project.fresh_settings_with_state() {
                    Ok(settings) => settings,
                    Err(error) => {
                        tracing::warn!(thread_id = %log_thread_id, %error, "failed to load fresh settings");
                        return;
                    }
                };
                let result = tokio::time::timeout(
                    Duration::from_secs(15),
                    omini_core::generate_thread_title(&settings, &user_input),
                )
                .await;

                let title = match result {
                    Ok(Ok(title)) => title,
                    Ok(Err(error)) => {
                        tracing::warn!(thread_id = %log_thread_id, %error, "background title generation failed");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!(thread_id = %log_thread_id, "background title generation timed out");
                        return;
                    }
                };

                // 2. 写库前再读一次当前 title：仅当仍等于我们刚写入的兜底时才覆盖。这样:
                //      a) 用户在 LLM 跑完前 /rename → 当前 title 已变 → 跳过;
                //      b) fork 派生 thread 的预设 title (非空) 仍存在 → 跳过;
                //      c) 兜底没被改 → 写入 LLM 生成版本并广播。
                let current = match db.get_thread(&thread_id).await {
                    Ok(Some(row)) => row.title,
                    Ok(None) => {
                        tracing::warn!(thread_id = %log_thread_id, "thread disappeared during background title generation");
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(thread_id = %log_thread_id, %error, "background title recheck failed");
                        return;
                    }
                };
                if current.as_deref() != Some(fallback_title.as_str()) {
                    tracing::debug!(
                        thread_id = %log_thread_id,
                        current_title = ?current,
                        "thread title changed during background generation, skipping update"
                    );
                    return;
                }
                if let Err(error) = db.update_thread_title(&thread_id, &title).await {
                    tracing::warn!(thread_id = %log_thread_id, %error, "background title write failed");
                    return;
                }
                let _ = inbox_tx.send(thread_title_changed_protocol_event(Some(title)));
            }
            .instrument(tracing::debug_span!(
                "thread",
                thread_id = %span_thread_id,
                task_kind = "background_title_generation"
            )),
        );
    }

    fn broadcast_thread_title_changed(&self, title: String) -> Result<(), CoreError> {
        // 新架构下 title 由 server 自己管理,直接构造 `RuntimeEvent` 走 server
        // 本地事件通道,不再借 `RuntimeToServerEvent::ThreadTitleChanged` 中转。
        self.broadcast_server_local_event(thread_title_changed_protocol_event(Some(title)));
        Ok(())
    }
}
