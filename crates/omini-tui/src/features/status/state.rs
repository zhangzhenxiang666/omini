use crate::app::event::ActiveProfile;
use crate::client::catalog::ThinkingEffort;
use std::path::PathBuf;

#[derive(Debug, Default)]
pub struct ProjectState {
    /// 底部状态栏信息
    pub status_bar: StatusBar,
    /// 当前线程标题（显示在头部栏）
    pub current_thread_title: Option<String>,
    /// 当前线程 ID（新建或切换时设置）
    pub current_thread_id: Option<String>,
}

/// 底部状态栏展示的信息
#[derive(Debug, Clone)]
pub struct StatusBar {
    /// 当前使用的模型 ID（如 "deepseek-v4-pro"）
    pub model: String,
    /// 思考程度（如果有）
    pub thinking_effort: Option<ThinkingEffort>,
    /// 当前活跃的供应商名称（如 "Bifrost"）
    pub active_provider: String,
    /// 当前工作目录路径
    pub cwd: PathBuf,
    /// 当前运行 profile
    pub active_profile: ActiveProfile,
    /// 最近一次请求的上下文 token 数。
    pub current_context_tokens: i64,
    /// 当前线程历史累计 token 数。
    pub total_tokens: i64,
    /// 当前线程历史累计缓存命中 token 数。
    pub total_cached_tokens: i64,
    /// 当前模型上下文窗口。
    pub context_window: Option<u32>,
    /// 当前 git 分支名（detached HEAD 显示 "HEAD"，不在仓库中为 None）。
    pub git_branch: Option<String>,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self {
            model: String::new(),
            thinking_effort: None,
            active_provider: String::new(),
            cwd: PathBuf::new(),
            active_profile: ActiveProfile::Main,
            current_context_tokens: 0,
            total_tokens: 0,
            total_cached_tokens: 0,
            context_window: None,
            git_branch: None,
        }
    }
}
