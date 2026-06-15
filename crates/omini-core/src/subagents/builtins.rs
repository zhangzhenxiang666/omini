use super::{AgentSource, AgentSpec, AgentToolPolicy};

const EXPLORER_INSTRUCTIONS: &str = include_str!("agents/explorer.md");
const GENERAL_INSTRUCTIONS: &str = include_str!("agents/general.md");

pub(super) fn built_in_agents() -> Vec<AgentSpec> {
    vec![explorer_agent(), general_agent()]
}

fn explorer_agent() -> AgentSpec {
    AgentSpec {
        name: "explorer".to_string(),
        description: "Read-only codebase exploration agent.".to_string(),
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
        description: "General purpose isolated coding agent.".to_string(),
        instructions: GENERAL_INSTRUCTIONS.trim().to_string(),
        tool_policy: AgentToolPolicy::default(),
        model: None,
        source: AgentSource::BuiltIn,
    }
}
