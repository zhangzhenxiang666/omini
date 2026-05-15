use crate::types::config::ModelConfig;

/// 交互选择页的当前步骤。
#[derive(Debug, Clone)]
pub enum ModelSelectionEntry {
    /// Provider 标题（不可选中）
    ProviderHeader { name: String },
    /// 某个 provider 下的模型（可选中）
    Model {
        provider_key: String,
        model: ModelConfig,
    },
}

/// 交互选择页的当前步骤。
#[derive(Debug, Clone)]
pub enum InteractionStep {
    /// 模型选择 — 按 provider 分组的扁平列表
    ModelSelection {
        /// 展平后的条目（ProviderHeader + Model 交替）
        entries: Vec<ModelSelectionEntry>,
        /// 当前选中索引，只指向 Model 条目
        selected: usize,
        /// 当前思考程度：0=None 1=Low 2=Medium 3=High
        thinking_idx: usize,
        /// 打开面板时正在使用的 provider key（用于标记 ✔）
        active_provider: String,
        /// 打开面板时正在使用的 model id
        active_model: String,
    },
    /// 会话选择
    Session {
        sessions: Vec<crate::types::events::SessionSummary>,
        /// 原始全量列表（用于过滤后恢复）
        all_sessions: Vec<crate::types::events::SessionSummary>,
        /// 当前搜索关键词
        search: String,
        selected: usize,
    },
}
