use crate::app::state::*;
use omini_domain::agent_run::AgentRunSnapshot;
use omini_domain::task::TaskStatus;
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Default)]
pub struct SessionsState {
    pub agent_runs: HashMap<String, AgentRunSnapshot>,
    /// 子 agent 视图模型，按 thread id 存储完整消息。
    pub subagents: HashMap<String, SubagentNode>,
    /// 当前主线程未结束的后台任务；按任务 ID 去重，同时覆盖 Agent 与 Bash。
    pub background_tasks: HashMap<String, TaskStatus>,
    /// 子 agent 节点回收后仍用于完成通知的运行时长。
    pub subagent_completion_durations: HashMap<String, Duration>,
    /// 子 agent 节点回收后仍用于 `send_message` 条目解析的显示标题。
    pub subagent_title_memory: HashMap<String, String>,
    /// 父 tool_use_id 到子 agent thread id 的映射。
    pub subagents_by_tool_use: HashMap<String, String>,
    /// 当前主线程直接异步子任务的显示顺序；索引 0 始终保留给 main。
    pub subagent_order: Vec<String>,
    pub views: SessionStore,
    pub active_session_task_id: Option<String>,
    pub session_selector_focused: bool,
    pub session_selection_index: usize,
}

/// 主会话和直接子任务通过同一个访问入口解析，渲染不临时移动数据。
impl SessionsState {
    pub fn active(&self) -> &SessionState {
        self.active_session_task_id
            .as_ref()
            .and_then(|id| self.views.get(id))
            .unwrap_or(&self.views["main"])
    }
    pub fn active_mut(&mut self) -> &mut SessionState {
        match &self.active_session_task_id {
            Some(id) if self.views.contains_key(id) => {
                self.views.get_mut(id).expect("活动会话存在")
            }
            _ => &mut self.views["main"],
        }
    }
}

/// 所有会话使用同一种状态并按 ID 索引；main 是当前主线程的稳定本地 ID。
#[derive(Debug)]
pub struct SessionStore {
    pub entries: HashMap<String, SessionState>,
}
impl Default for SessionStore {
    fn default() -> Self {
        Self {
            entries: HashMap::from([("main".to_string(), SessionState::default())]),
        }
    }
}
impl std::ops::Deref for SessionStore {
    type Target = HashMap<String, SessionState>;
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}
impl std::ops::DerefMut for SessionStore {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.entries
    }
}
impl std::ops::Index<&str> for SessionStore {
    type Output = SessionState;
    fn index(&self, id: &str) -> &Self::Output {
        &self.entries[id]
    }
}
impl std::ops::IndexMut<&str> for SessionStore {
    fn index_mut(&mut self, id: &str) -> &mut Self::Output {
        self.entries.get_mut(id).expect("会话 ID 必须先注册")
    }
}
impl SessionStore {
    /// 重放刷新子会话时保留已经恢复的主会话内容。
    pub fn clear_children(&mut self) {
        self.entries.retain(|id, _| id == "main");
    }
}
