use crate::app::event::*;
use crate::app::state::*;

#[derive(Debug)]
pub struct StartState {
    /// TUI 刚启动、尚未进入任何线程/交互前展示的一次性启动页。
    pub show_start_screen: bool,
    /// 启动时配置的 MCP server 数量，用于首屏项目仪表盘。
    pub startup_mcp_server_count: usize,
    /// 当前项目目录下是否存在非空 AGENTS.md，用于首屏项目仪表盘。
    pub startup_has_project_instructions: bool,
    /// 启动时读取的最近线程，用于首屏提供可恢复的上下文线索。
    pub startup_recent_threads: Vec<ThreadSummary>,
    /// 启动页中文提示，初始化时从静态列表随机选择一次。
    pub startup_tip: String,
}

impl Default for StartState {
    fn default() -> Self {
        Self {
            show_start_screen: true,
            startup_mcp_server_count: 0,
            startup_has_project_instructions: false,
            startup_recent_threads: Vec::new(),
            startup_tip: pick_start_screen_tip(),
        }
    }
}
