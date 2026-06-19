//! TOML 内联 tool rule 解析和匹配（如 `Read(**/...)`、`Subagent(explorer)` 语法）。

use omini_config::RawPermissionConfig;
use omini_domain::events::{PermissionPreview, PermissionSource};

use crate::bash_parser::RuleDecision;
use crate::engine::PermissionEngine;
use crate::path_matcher::{self, PathMatcher};

/// 单条 TOML 内联工具规则，解析自 `[permissions]` 配置段。
#[derive(Debug, Clone)]
pub(crate) struct ToolRule {
    pub tool: String,
    pub specifier: Option<String>,
    pub decision: RuleDecision,
    pub source: Option<String>,
    pub raw: String,
}

/// 将一组规则字符串解析为 `ToolRule` 列表，不支持的工具和语法错误写入 diagnostics。
pub(crate) fn parse_tool_rules(
    values: Vec<String>,
    decision: RuleDecision,
    source: Option<String>,
    diagnostics: &mut Vec<String>,
) -> Vec<ToolRule> {
    values
        .into_iter()
        .filter_map(|value| parse_tool_rule(&value, decision, source.clone(), diagnostics))
        .collect()
}

fn parse_tool_rule(
    value: &str,
    decision: RuleDecision,
    source: Option<String>,
    diagnostics: &mut Vec<String>,
) -> Option<ToolRule> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let (tool, specifier) = parse_tool_rule_parts(value, diagnostics, source.as_deref())?;
    let normalized = normalize_tool_name(&tool);
    if normalized == "bash" {
        diagnostics.push(format!(
            "{}: ignored permission rule '{}': Bash rules must be configured in .omini/rules/*.rules",
            source.as_deref().unwrap_or("<inline>"),
            value
        ));
        return None;
    }
    if !is_supported_permission_tool(&normalized) {
        diagnostics.push(format!(
            "{}: ignored permission rule '{}': unsupported tool '{}'",
            source.as_deref().unwrap_or("<inline>"),
            value,
            tool
        ));
        return None;
    }

    let Some(specifier) = specifier else {
        return Some(ToolRule {
            tool,
            specifier: None,
            decision,
            source,
            raw: value.to_string(),
        });
    };

    Some(ToolRule {
        tool,
        specifier: Some(specifier),
        decision,
        source,
        raw: value.to_string(),
    })
}

fn parse_tool_rule_parts(
    value: &str,
    diagnostics: &mut Vec<String>,
    source: Option<&str>,
) -> Option<(String, Option<String>)> {
    let Some(open_idx) = value.find('(') else {
        return Some((value.to_string(), None));
    };
    let tool = value[..open_idx].trim();
    let rest = &value[open_idx + 1..];
    if !value.ends_with(')') || rest[..rest.len().saturating_sub(1)].contains(')') {
        diagnostics.push(format!(
            "{}: ignored permission rule '{}': invalid permission rule syntax",
            source.unwrap_or("<inline>"),
            value
        ));
        return None;
    }
    if tool.is_empty() {
        diagnostics.push(format!(
            "{}: ignored permission rule '{}': missing tool name",
            source.unwrap_or("<inline>"),
            value
        ));
        return None;
    }
    Some((
        tool.to_string(),
        Some(rest[..rest.len().saturating_sub(1)].trim().to_string()),
    ))
}

fn is_supported_permission_tool(tool: &str) -> bool {
    matches!(
        tool,
        "read" | "search" | "edit" | "write" | "subagent" | "ask_user" | "todo_write"
    )
}

pub(crate) fn normalize_tool_name(tool: &str) -> String {
    match tool.trim() {
        "AskUser" | "ask_user" => "ask_user".to_string(),
        "Bash" | "bash" => "bash".to_string(),
        "Search" | "search" => "search".to_string(),
        "Read" | "read" => "read".to_string(),
        "Edit" | "edit" => "edit".to_string(),
        "Write" | "write" => "write".to_string(),
        "Subagent" | "subagent" => "subagent".to_string(),
        "TodoWrite" | "todo_write" => "todo_write".to_string(),
        other => other.to_ascii_lowercase(),
    }
}

impl ToolRule {
    pub(crate) fn matches(
        &self,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &serde_json::Value,
        engine: &PermissionEngine,
    ) -> bool {
        if normalize_tool_name(&self.tool) != normalize_tool_name(tool_name) {
            return false;
        }
        let Some(specifier) = &self.specifier else {
            return true;
        };
        match normalize_tool_name(tool_name).as_str() {
            "read" | "view_image" | "search" | "edit" | "write" => {
                let Some(path) = path_matcher::permission_path(preview, raw_input) else {
                    return false;
                };
                engine.normalize_rule_path(specifier).matches(&path)
            }
            "subagent" => raw_input
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| name == specifier),
            _ => specifier == "*" || specifier == tool_name,
        }
    }

    pub(crate) fn display(&self) -> String {
        match &self.specifier {
            Some(specifier) => format!("{}({specifier})", self.tool),
            None => self.tool.clone(),
        }
    }

    pub(crate) fn permission_source(&self) -> Option<PermissionSource> {
        self.source.as_ref().map(|source| PermissionSource {
            decision: self.decision.label().to_string(),
            source: source.clone(),
            rule: self.raw.clone(),
        })
    }
}

/// 从 `RawPermissionConfig` 解析工具规则并追加到 tool_rules 列表。
pub(crate) fn extend_tool_rules(
    tool_rules: &mut Vec<ToolRule>,
    raw: RawPermissionConfig,
    source: Option<String>,
    diagnostics: &mut Vec<String>,
) {
    tool_rules.extend(parse_tool_rules(
        raw.allow,
        RuleDecision::Allow,
        source.clone(),
        diagnostics,
    ));
    tool_rules.extend(parse_tool_rules(
        raw.ask,
        RuleDecision::Ask,
        source.clone(),
        diagnostics,
    ));
    tool_rules.extend(parse_tool_rules(
        raw.deny,
        RuleDecision::Deny,
        source,
        diagnostics,
    ));
}

impl PermissionEngine {
    pub(crate) fn normalize_rule_path(&self, specifier: &str) -> PathMatcher {
        let raw = specifier.trim();
        if let Some(rest) = raw.strip_prefix("//") {
            PathMatcher::new(std::path::PathBuf::from("/").join(rest))
        } else if let Some(rest) = raw.strip_prefix("~/") {
            let base = self
                .home
                .clone()
                .unwrap_or_else(|| std::path::PathBuf::from("~"));
            PathMatcher::new(base.join(rest))
        } else if let Some(rest) = raw.strip_prefix('/') {
            PathMatcher::new(self.cwd.join(rest))
        } else if let Some(rest) = raw.strip_prefix("./") {
            PathMatcher::new(self.cwd.join(rest))
        } else {
            PathMatcher::new(self.cwd.join(raw))
        }
    }
}
