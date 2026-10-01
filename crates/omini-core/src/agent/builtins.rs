use super::{AgentSource, AgentSpec, AgentToolPolicy};

const EXPLORER_INSTRUCTIONS: &str = include_str!("agents/explorer.txt");

pub(super) fn built_in_agents() -> Vec<AgentSpec> {
    vec![explorer_agent(), general_agent()]
}

fn explorer_agent() -> AgentSpec {
    AgentSpec {
        name: "explorer".to_string(),
        description: "Read-only codebase exploration agent. Use for finding files by pattern, searching definitions/symbols, tracing dependencies, and understanding architecture across multiple files. Specify thoroughness: 'quick' (narrow), 'medium', or 'very thorough' (comprehensive cross-file analysis).".to_string(),
        short_description: Some("只读探索代码库，搜索文件、定义、依赖和架构".to_string()),
        instructions: EXPLORER_INSTRUCTIONS.trim().to_string(),
        tool_policy: AgentToolPolicy {
            allow: Some(vec![
                "search".to_string(),
                "read".to_string(),
                "bash".to_string(),
            ]),
            deny: None,
        },
        model: None,
        source: AgentSource::BuiltIn,
    }
}

fn general_agent() -> AgentSpec {
    AgentSpec {
        name: "general".to_string(),
        description: "General-purpose coding agent for multi-step implementation and research. Use for writing tests, refactoring modules, making code changes, or complex questions requiring multiple tools. Can parallelize independent subtasks. Unlike explorer, this agent can modify files.".to_string(),
        short_description: Some("通用编码 agent，多步骤实现、重构、测试、研究".to_string()),
        // general 与主 agent 共享同一份身份/行为/通用工具指引正文,避免两份文本漂移;
        // 其子代理特有约束由 agent_system_prompt 的外层框架补充。
        instructions: crate::prompts::sections::main_mode_body().to_string(),
        tool_policy: AgentToolPolicy::default(),
        model: None,
        source: AgentSource::BuiltIn,
    }
}
