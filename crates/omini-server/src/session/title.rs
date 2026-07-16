use std::{sync::Arc, time::Duration};

use crate::{
    event::bridge::{fallback_session_title_from_user_input, session_title_changed_protocol_event},
    session::SessionRuntime,
};
use omini_core::CoreError;
use omini_protocol as client_proto;
use tracing::Instrument;

impl SessionRuntime {
    pub async fn rename_session(&self, title: String) -> Result<(), CoreError> {
        self.db
            .update_session_title(&self.session_id, &title)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to rename session", error.to_string())
            })?;
        self.broadcast_session_title_changed(title)?;
        Ok(())
    }

    /// 同步落库 300 字符兜底 title。返回 `true` 表示这次实际写入了新
    /// title (供路由层据此决定是否 spawn 后台 LLM 升级任务);`false`
    /// 表示 text 为空、title 已被设置过或 session 已经有 messages,SQL
    /// 软写条件被跳过。
    pub async fn set_initial_title_from_input(
        &self,
        input: &client_proto::UserInput,
    ) -> Result<bool, CoreError> {
        let Some(title) = fallback_session_title_from_user_input(input) else {
            return Ok(false);
        };
        let updated = self
            .db
            .set_initial_session_title(&self.session_id, &title)
            .await
            .map_err(|error| {
                CoreError::persistence("failed to set initial session title", error.to_string())
            })?;
        if updated {
            self.broadcast_session_title_changed(title)?;
        }
        Ok(updated)
    }

    /// 首条消息提交后，在后台异步用 `model_tiers.small` 生成一个更可读的
    /// 标题。`fallback_title` 是路由层刚刚同步落库的 300 字符兜底 title；
    /// LLM 跑完后只有当 DB 中当前 title 仍等于这个兜底（即用户在期间
    /// 没有 /rename、没有 fork 预设冲突），才用 `update_session_title`
    /// 覆盖成 LLM 生成版本并广播，避免覆盖用户主动改名或 fork 预设。
    pub fn spawn_background_title_generation(
        self: &Arc<Self>,
        project_id: String,
        manager: Arc<crate::daemon::GlobalDaemonManager>,
        fallback_title: String,
        user_input: String,
    ) {
        let db = Arc::clone(&self.db);
        let session_id = self.session_id.clone();
        let span_session_id = session_id.clone();
        let inbox_tx = self.server_event_inbox_tx.clone();
        tokio::spawn(
            async move {
                let log_session_id = &session_id;
                // 1. 拉一次最新 settings，再调 LLM，带超时。
                let settings = match manager.project(&project_id).map_err(|err| {
                    CoreError::new(format!("failed to lookup project {project_id} for background settings load: {err:?}"
                    ))}
                ).and_then(|project|{
                    project.fresh_settings_with_state()

                })
                {
                    Ok(settings) => settings,
                    Err(error) => {
                        tracing::warn!(session_id = %log_session_id, %error, "failed to load fresh settings");
                        return;
                    }
                };
                let result = tokio::time::timeout(
                    Duration::from_secs(15),
                    omini_core::generate_session_title(&settings, &user_input),
                )
                .await;

                let title = match result {
                    Ok(Ok(title)) => title,
                    Ok(Err(error)) => {
                        tracing::warn!(session_id = %log_session_id, %error, "background title generation failed");
                        return;
                    }
                    Err(_) => {
                        tracing::warn!(session_id = %log_session_id, "background title generation timed out");
                        return;
                    }
                };

                // 2. 写库前再读一次当前 title：仅当仍等于我们刚写入的兜底时才覆盖。这样:
                //      a) 用户在 LLM 跑完前 /rename → 当前 title 已变 → 跳过;
                //      b) fork 派生 session 的预设 title (非空) 仍存在 → 跳过;
                //      c) 兜底没被改 → 写入 LLM 生成版本并广播。
                let current = match db.get_session(&session_id).await {
                    Ok(Some(row)) => row.title,
                    Ok(None) => {
                        tracing::warn!(session_id = %log_session_id, "session disappeared during background title generation");
                        return;
                    }
                    Err(error) => {
                        tracing::warn!(session_id = %log_session_id, %error, "background title recheck failed");
                        return;
                    }
                };
                if current.as_deref() != Some(fallback_title.as_str()) {
                    tracing::debug!(
                        session_id = %log_session_id,
                        current_title = ?current,
                        "session title changed during background generation, skipping update"
                    );
                    return;
                }
                if let Err(error) = db.update_session_title(&session_id, &title).await {
                    tracing::warn!(session_id = %log_session_id, %error, "background title write failed");
                    return;
                }
                let _ = inbox_tx.send(session_title_changed_protocol_event(Some(title)));
            }
            .instrument(tracing::debug_span!(
                "session",
                session_id = %span_session_id,
                task_kind = "background_title_generation"
            )),
        );
    }

    fn broadcast_session_title_changed(&self, title: String) -> Result<(), CoreError> {
        // 新架构下 title 由 server 自己管理,直接构造 `RuntimeEvent` 走 server
        // 本地事件通道,不再借 `RuntimeToServerEvent::SessionTitleChanged` 中转。
        self.broadcast_server_local_event(session_title_changed_protocol_event(Some(title)));
        Ok(())
    }
}
