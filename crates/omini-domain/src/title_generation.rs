use serde::{Deserialize, Serialize};

/// LLM 在后台 session 标题生成任务中输出的 JSON payload。
/// 顶层 schema 故意保持单一字段，解析和后处理在
/// `omini_core::title_generation` 里集中完成。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct GeneratedSessionTitle {
    pub title: String,
}
