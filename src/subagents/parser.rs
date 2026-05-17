use super::{AgentModelSpec, AgentSource, AgentSpec, AgentToolPolicy};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

pub(super) fn parse_agent_file(path: &Path) -> Result<AgentSpec, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("failed to read file: {e}"))?;
    let (frontmatter, body) = split_frontmatter(&content)?;
    let raw = parse_frontmatter(frontmatter)?;

    let name = required_field(&raw, "name")?;
    let description = required_field(&raw, "description")?;
    let allow = match raw.get("tools") {
        Some(value) => {
            Some(parse_tools_value(value).and_then(|tools| normalize_allow_tools(&tools))?)
        }
        None => None,
    };
    let deny = match raw.get("disallow_tools") {
        Some(value) => Some(parse_tools_value(value).and_then(|tools| normalize_tools(&tools))?),
        None => None,
    };
    let model = match raw.get("model") {
        Some(value) => Some(parse_model_spec(&parse_scalar(value)?)?),
        None => None,
    };

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

fn split_frontmatter(content: &str) -> Result<(&str, &str), String> {
    let content = content
        .strip_prefix("---")
        .ok_or_else(|| "missing frontmatter; expected file to start with '---'".to_string())?;
    let content = content.strip_prefix('\n').unwrap_or(content);
    let Some((frontmatter, body)) = content.split_once("\n---") else {
        return Err("missing closing frontmatter delimiter '---'".to_string());
    };
    let body = body.strip_prefix('\n').unwrap_or(body);
    Ok((frontmatter, body))
}

fn parse_frontmatter(frontmatter: &str) -> Result<HashMap<String, String>, String> {
    let mut values = HashMap::new();
    for (idx, line) in frontmatter.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            return Err(format!("invalid frontmatter line {}: {line}", idx + 1));
        };
        let key = key.trim();
        if key.is_empty() {
            return Err(format!("empty frontmatter key on line {}", idx + 1));
        }
        values.insert(key.to_string(), value.trim().to_string());
    }
    Ok(values)
}

fn required_field(raw: &HashMap<String, String>, key: &str) -> Result<String, String> {
    let value = raw
        .get(key)
        .ok_or_else(|| format!("missing required frontmatter field '{key}'"))?;
    let value = parse_scalar(value)?;
    if value.trim().is_empty() {
        return Err(format!("frontmatter field '{key}' must not be empty"));
    }
    Ok(value)
}

fn parse_scalar(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.starts_with('"') || value.starts_with('\'') {
        parse_quoted(value)
    } else {
        Ok(value.to_string())
    }
}

fn parse_quoted(value: &str) -> Result<String, String> {
    let quote = value
        .chars()
        .next()
        .ok_or_else(|| "empty quoted scalar".to_string())?;
    if !value.ends_with(quote) || value.len() < 2 {
        return Err(format!("unterminated quoted scalar: {value}"));
    }
    let inner = &value[quote.len_utf8()..value.len() - quote.len_utf8()];
    if quote == '"' {
        Ok(inner
            .replace("\\n", "\n")
            .replace("\\\"", "\"")
            .replace("\\\\", "\\"))
    } else {
        Ok(inner.to_string())
    }
}

fn parse_tools_value(value: &str) -> Result<Vec<String>, String> {
    let value = value.trim();
    if value.starts_with('[') {
        if !value.ends_with(']') {
            return Err("tools array must end with ']'".to_string());
        }
        let inner = &value[1..value.len() - 1];
        if inner.trim().is_empty() {
            return Ok(Vec::new());
        }
        inner
            .split(',')
            .map(|part| parse_scalar(part.trim()))
            .collect()
    } else {
        let value = parse_scalar(value)?;
        Ok(value
            .split(',')
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .map(ToOwned::to_owned)
            .collect())
    }
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
        "Read" | "read" => "read",
        "Edit" | "edit" => "edit",
        "Write" | "write" => "write",
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
