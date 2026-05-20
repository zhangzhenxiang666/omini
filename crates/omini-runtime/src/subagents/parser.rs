use super::{AgentModelSpec, AgentSource, AgentSpec, AgentToolPolicy};
use crate::frontmatter;
use std::fs;
use std::path::Path;

pub(super) fn parse_agent_file(path: &Path) -> Result<AgentSpec, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("failed to read file: {e}"))?;
    let (raw, body) = frontmatter::parse(&content)?;

    let name = frontmatter::required_string(&raw, "name")?;
    let description = frontmatter::required_string(&raw, "description")?;
    let allow = frontmatter::optional_string_list(&raw, "tools")?
        .map(|tools| normalize_allow_tools(&tools))
        .transpose()?;
    let deny = frontmatter::optional_string_list(&raw, "disallow_tools")?
        .map(|tools| normalize_tools(&tools))
        .transpose()?;
    let model = frontmatter::optional_string(&raw, "model")?
        .map(|value| parse_model_spec(&value))
        .transpose()?;

    let instructions = body.trim().to_string();
    if instructions.is_empty() {
        return Err("agent instructions body must not be empty".to_string());
    }

    Ok(AgentSpec {
        name,
        description,
        instructions,
        tool_policy: AgentToolPolicy { allow, deny },
        model,
        source: AgentSource::File(path.to_path_buf()),
    })
}

fn normalize_tools(tools: &[String]) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    for tool in tools {
        let name = normalize_tool_name(tool)?;
        if !normalized.iter().any(|existing| existing == &name) {
            normalized.push(name);
        }
    }
    Ok(normalized)
}

fn normalize_allow_tools(tools: &[String]) -> Result<Vec<String>, String> {
    let tools = normalize_tools(tools)?;
    Ok(tools
        .into_iter()
        .filter(|tool| tool != "subagent")
        .collect())
}

fn normalize_tool_name(tool: &str) -> Result<String, String> {
    let trimmed = tool.trim();
    if trimmed.is_empty() {
        return Err("tool name must not be empty".to_string());
    }
    let normalized = match trimmed {
        "AskUser" | "ask_user" => "ask_user",
        "Bash" | "bash" => "bash",
        "Search" | "search" => "search",
        "Read" | "read" => "read",
        "Edit" | "edit" => "edit",
        "Write" | "write" => "write",
        "Skill" | "skill" => "skill",
        "Subagent" | "subagent" => "subagent",
        _ => trimmed,
    };
    Ok(normalized.to_ascii_lowercase())
}

fn parse_model_spec(value: &str) -> Result<AgentModelSpec, String> {
    let Some((provider, model)) = value.split_once('/') else {
        return Err("model must use 'provider/model-name' format".to_string());
    };
    let provider = provider.trim();
    let model = model.trim();
    if provider.is_empty() || model.is_empty() {
        return Err("model must use 'provider/model-name' format".to_string());
    }
    Ok(AgentModelSpec {
        provider: provider.to_string(),
        model: model.to_string(),
    })
}
