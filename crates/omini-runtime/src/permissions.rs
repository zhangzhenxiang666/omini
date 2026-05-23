use crate::types::events::{
    ActiveProfile, BashPermissionPreview, PermissionPreview, PermissionSource,
};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};

pub use omini_types::permissions::RawPermissionConfig;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Ask,
    Deny { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionCheck {
    pub decision: PermissionDecision,
    pub source: Option<PermissionSource>,
}

impl PermissionDecision {
    fn rank(&self) -> u8 {
        match self {
            PermissionDecision::Allow => 0,
            PermissionDecision::Ask => 1,
            PermissionDecision::Deny { .. } => 2,
        }
    }

    fn stricter(self, other: PermissionDecision) -> PermissionDecision {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

fn stricter_check(current: PermissionCheck, next: PermissionCheck) -> PermissionCheck {
    if next.decision.rank() > current.decision.rank() {
        next
    } else {
        current
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuleDecision {
    Allow,
    Ask,
    Deny,
}

impl RuleDecision {
    fn label(self) -> &'static str {
        match self {
            RuleDecision::Allow => "allow",
            RuleDecision::Ask => "ask",
            RuleDecision::Deny => "deny",
        }
    }
}

#[derive(Debug, Clone)]
struct ToolRule {
    tool: String,
    specifier: Option<String>,
    decision: RuleDecision,
    source: Option<String>,
    raw: String,
}

#[derive(Debug, Clone)]
struct BashRule {
    pattern: Vec<Vec<String>>,
    decision: RuleDecision,
    justification: Option<String>,
    source: Option<String>,
    rule_index: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct CompiledPermissions {
    tool_rules: Vec<ToolRule>,
    bash_rules: Vec<BashRule>,
}

#[derive(Debug, Clone)]
pub struct PermissionEngine {
    cwd: PathBuf,
    home: Option<PathBuf>,
    rules: CompiledPermissions,
    diagnostics: Vec<String>,
}

impl PermissionEngine {
    pub fn empty(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            home: None,
            rules: CompiledPermissions::default(),
            diagnostics: Vec::new(),
        }
    }

    pub fn load(
        cwd: impl Into<PathBuf>,
        home: Option<PathBuf>,
        user_permissions: Option<RawPermissionConfig>,
    ) -> Self {
        let cwd = cwd.into();
        let mut rules = CompiledPermissions::default();
        let mut diagnostics = Vec::new();
        if let Some(raw) = user_permissions {
            let source = user_config_path(home.as_deref());
            rules.extend_tool_rules(raw, Some(source.display().to_string()), &mut diagnostics);
        }
        if let Some((raw, source)) = load_project_permissions(&cwd) {
            rules.extend_tool_rules(raw, Some(source.display().to_string()), &mut diagnostics);
        }
        rules.bash_rules.extend(load_bash_rules(
            home.as_deref()
                .map(|home| home.join(".omini").join("rules")),
            &mut diagnostics,
        ));
        rules.bash_rules.extend(load_bash_rules(
            Some(cwd.join(".omini").join("rules")),
            &mut diagnostics,
        ));

        Self {
            cwd,
            home,
            rules,
            diagnostics,
        }
    }

    #[cfg(test)]
    pub fn for_test(cwd: impl Into<PathBuf>, raw: RawPermissionConfig) -> Self {
        Self {
            cwd: cwd.into(),
            home: None,
            rules: {
                let mut rules = CompiledPermissions::default();
                let mut diagnostics = Vec::new();
                rules.extend_tool_rules(raw, None, &mut diagnostics);
                rules
            },
            diagnostics: Vec::new(),
        }
    }

    pub fn diagnostics(&self) -> &[String] {
        &self.diagnostics
    }

    pub fn decide(
        &self,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &Value,
    ) -> PermissionDecision {
        self.check(tool_name, preview, raw_input).decision
    }

    pub fn decide_for_profile(
        &self,
        active_profile: ActiveProfile,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &Value,
    ) -> PermissionDecision {
        self.check_for_profile(active_profile, tool_name, preview, raw_input)
            .decision
    }

    pub fn check(
        &self,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &Value,
    ) -> PermissionCheck {
        self.check_for_profile(ActiveProfile::Main, tool_name, preview, raw_input)
    }

    pub fn check_for_profile(
        &self,
        active_profile: ActiveProfile,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &Value,
    ) -> PermissionCheck {
        if let Some(check) = self.profile_policy(active_profile, tool_name) {
            return check;
        }

        if tool_name == "bash" {
            let Some(PermissionPreview::Bash(preview)) = preview else {
                return PermissionCheck {
                    decision: PermissionDecision::Ask,
                    source: None,
                };
            };
            return self.decide_bash(preview);
        }

        let configured = self.decide_tool_rules(tool_name, preview, raw_input);
        if matches!(
            configured,
            Some(PermissionCheck {
                decision: PermissionDecision::Deny { .. },
                ..
            })
        ) {
            return configured.unwrap();
        }

        let builtin = self.decide_builtin(tool_name, preview, raw_input);
        if matches!(builtin, PermissionDecision::Deny { .. }) {
            return PermissionCheck {
                decision: builtin,
                source: None,
            };
        }

        configured.unwrap_or(PermissionCheck {
            decision: builtin,
            source: None,
        })
    }

    fn decide_tool_rules(
        &self,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &Value,
    ) -> Option<PermissionCheck> {
        let mut decision: Option<PermissionCheck> = None;
        for rule in &self.rules.tool_rules {
            if !rule.matches(tool_name, preview, raw_input, self) {
                continue;
            }
            let next = match rule.decision {
                RuleDecision::Allow => PermissionDecision::Allow,
                RuleDecision::Ask => PermissionDecision::Ask,
                RuleDecision::Deny => PermissionDecision::Deny {
                    reason: format!("Permission denied by rule: {}", rule.display()),
                },
            };
            let next = PermissionCheck {
                decision: next,
                source: rule.permission_source(),
            };
            decision = Some(match decision {
                Some(current) => stricter_check(current, next),
                None => next,
            });
        }
        decision
    }

    fn decide_builtin(
        &self,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &Value,
    ) -> PermissionDecision {
        match tool_name {
            "read" => match read_path(preview, raw_input) {
                Some(path) if is_private_path(&path) => PermissionDecision::Ask,
                Some(path) if self.is_under_cwd_or_tmp(&path) => PermissionDecision::Allow,
                Some(_) => PermissionDecision::Ask,
                None => PermissionDecision::Ask,
            },
            "search" => match search_path(preview, raw_input).map(|path| self.input_path(path)) {
                Some(path) if is_private_path(&path) => PermissionDecision::Ask,
                Some(path) if self.is_under_cwd_or_tmp(&path) => PermissionDecision::Allow,
                Some(_) => PermissionDecision::Ask,
                None => PermissionDecision::Allow,
            },
            "edit" | "write" => PermissionDecision::Ask,
            "todo_write" => PermissionDecision::Allow,
            "ask_user" | "skill" | "subagent" => PermissionDecision::Allow,
            _ => PermissionDecision::Ask,
        }
    }

    pub fn profile_policy(
        &self,
        active_profile: ActiveProfile,
        tool_name: &str,
    ) -> Option<PermissionCheck> {
        let tool = normalize_tool_name(tool_name);
        let decision = match active_profile {
            ActiveProfile::Plan if matches!(tool.as_str(), "edit" | "write" | "todo_write") => {
                PermissionDecision::Deny {
                    reason: format!("{tool_name} is not available in plan profile"),
                }
            }
            _ => return None,
        };

        Some(PermissionCheck {
            decision,
            source: None,
        })
    }

    fn decide_bash(&self, preview: &BashPermissionPreview) -> PermissionCheck {
        let builtin = builtin_bash_decision(&preview.command);
        if matches!(builtin, PermissionDecision::Deny { .. }) {
            return PermissionCheck {
                decision: builtin,
                source: None,
            };
        }

        let mut configured: Option<PermissionCheck> = None;
        for command in split_shell_commands(&preview.command) {
            let args = shell_words(&command);
            if args.is_empty() {
                continue;
            }
            for rule in &self.rules.bash_rules {
                if rule.matches(&args) {
                    let next = match rule.decision {
                        RuleDecision::Allow => PermissionDecision::Allow,
                        RuleDecision::Ask => PermissionDecision::Ask,
                        RuleDecision::Deny => PermissionDecision::Deny {
                            reason: rule
                                .justification
                                .clone()
                                .unwrap_or_else(|| "Permission denied by bash rule".to_string()),
                        },
                    };
                    let next = PermissionCheck {
                        decision: next,
                        source: rule.permission_source(),
                    };
                    configured = Some(match configured {
                        Some(current) => stricter_check(current, next),
                        None => next,
                    });
                }
            }
        }

        if matches!(
            configured,
            Some(PermissionCheck {
                decision: PermissionDecision::Deny { .. },
                ..
            })
        ) {
            return configured.unwrap();
        }
        if matches!(builtin, PermissionDecision::Ask) {
            return PermissionCheck {
                decision: builtin,
                source: None,
            };
        }
        configured.unwrap_or(PermissionCheck {
            decision: PermissionDecision::Allow,
            source: None,
        })
    }

    fn is_under_cwd_or_tmp(&self, path: &Path) -> bool {
        path.starts_with(&self.cwd) || path.starts_with("/tmp")
    }

    fn input_path(&self, path: PathBuf) -> PathBuf {
        if path.is_absolute() {
            path
        } else {
            self.cwd.join(path)
        }
    }

    fn normalize_rule_path(&self, specifier: &str) -> PathMatcher {
        let raw = specifier.trim();
        if let Some(rest) = raw.strip_prefix("//") {
            PathMatcher::new(PathBuf::from("/").join(rest))
        } else if let Some(rest) = raw.strip_prefix("~/") {
            let base = self.home.clone().unwrap_or_else(|| PathBuf::from("~"));
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

fn user_config_path(home: Option<&Path>) -> PathBuf {
    home.map(Path::to_path_buf)
        .or_else(dirs::home_dir)
        .map(|home| home.join(".omini").join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("~/.omini/config.toml"))
}

impl CompiledPermissions {
    fn extend_tool_rules(
        &mut self,
        raw: RawPermissionConfig,
        source: Option<String>,
        diagnostics: &mut Vec<String>,
    ) {
        self.tool_rules.extend(parse_tool_rules(
            raw.allow,
            RuleDecision::Allow,
            source.clone(),
            diagnostics,
        ));
        self.tool_rules.extend(parse_tool_rules(
            raw.ask,
            RuleDecision::Ask,
            source.clone(),
            diagnostics,
        ));
        self.tool_rules.extend(parse_tool_rules(
            raw.deny,
            RuleDecision::Deny,
            source,
            diagnostics,
        ));
    }
}

impl ToolRule {
    fn matches(
        &self,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &Value,
        engine: &PermissionEngine,
    ) -> bool {
        if normalize_tool_name(&self.tool) != normalize_tool_name(tool_name) {
            return false;
        }
        let Some(specifier) = &self.specifier else {
            return true;
        };
        match normalize_tool_name(tool_name).as_str() {
            "read" | "search" | "edit" | "write" => {
                let Some(path) = permission_path(preview, raw_input) else {
                    return false;
                };
                engine.normalize_rule_path(specifier).matches(&path)
            }
            "subagent" => raw_input
                .get("name")
                .and_then(Value::as_str)
                .is_some_and(|name| name == specifier),
            _ => specifier == "*" || specifier == tool_name,
        }
    }

    fn display(&self) -> String {
        match &self.specifier {
            Some(specifier) => format!("{}({specifier})", self.tool),
            None => self.tool.clone(),
        }
    }

    fn permission_source(&self) -> Option<PermissionSource> {
        self.source.as_ref().map(|source| PermissionSource {
            decision: self.decision.label().to_string(),
            source: source.clone(),
            rule: self.raw.clone(),
        })
    }
}

impl BashRule {
    fn matches(&self, args: &[String]) -> bool {
        if args.len() < self.pattern.len() {
            return false;
        }
        self.pattern
            .iter()
            .zip(args.iter())
            .all(|(allowed, arg)| allowed.iter().any(|candidate| candidate == arg))
    }

    fn permission_source(&self) -> Option<PermissionSource> {
        self.source.as_ref().map(|source| {
            let rule = self
                .rule_index
                .map(|index| format!("prefix_rule #{index}"))
                .unwrap_or_else(|| format!("prefix_rule {}", format_bash_pattern(&self.pattern)));
            PermissionSource {
                decision: match self.decision {
                    RuleDecision::Allow => "allow",
                    RuleDecision::Ask => "prompt",
                    RuleDecision::Deny => "forbidden",
                }
                .to_string(),
                source: source.clone(),
                rule,
            }
        })
    }
}

fn format_bash_pattern(pattern: &[Vec<String>]) -> String {
    let parts = pattern
        .iter()
        .map(|options| {
            if options.len() == 1 {
                options[0].clone()
            } else {
                format!("[{}]", options.join("|"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!("({parts})")
}

#[derive(Debug, Clone)]
struct PathMatcher {
    pattern: String,
}

impl PathMatcher {
    fn new(path: PathBuf) -> Self {
        Self {
            pattern: normalize_path_string(&path),
        }
    }

    fn matches(&self, path: &Path) -> bool {
        let text = normalize_path_string(path);
        wildcard_match(&self.pattern, &text)
            || self
                .pattern
                .contains("/**/")
                .then(|| self.pattern.replace("/**/", "/"))
                .is_some_and(|pattern| wildcard_match(&pattern, &text))
    }
}

fn load_project_permissions(cwd: &Path) -> Option<(RawPermissionConfig, PathBuf)> {
    let path = cwd.join(".omini").join("permissions.toml");
    let content = fs::read_to_string(&path).ok()?;
    toml::from_str(&content).ok().map(|raw| (raw, path))
}

fn load_bash_rules(dir: Option<PathBuf>, diagnostics: &mut Vec<String>) -> Vec<BashRule> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "rules") {
            paths.push(path);
        }
    }
    paths.sort();
    let mut rules = Vec::new();
    for path in paths {
        match fs::read_to_string(&path) {
            Ok(content) => {
                let (parsed, mut warnings) = parse_bash_rules_with_diagnostics(&content, &path);
                rules.extend(parsed);
                diagnostics.append(&mut warnings);
            }
            Err(e) => diagnostics.push(format!(
                "{}: failed to read rules file: {e}",
                path.display()
            )),
        }
    }
    rules
}

fn parse_tool_rules(
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

#[cfg(test)]
fn parse_bash_rules(content: &str) -> Vec<BashRule> {
    parse_bash_rules_with_diagnostics(content, Path::new("<inline>")).0
}

fn parse_bash_rules_with_diagnostics(content: &str, source: &Path) -> (Vec<BashRule>, Vec<String>) {
    let mut rules = Vec::new();
    let mut diagnostics = Vec::new();
    let mut rest = content;
    let mut rule_index = 0usize;
    while let Some(start) = rest.find("prefix_rule(") {
        rule_index += 1;
        rest = &rest[start + "prefix_rule(".len()..];
        let Some(end) = rest.find("\n)") else {
            diagnostics.push(format!(
                "{}: prefix_rule #{} has no closing ')'",
                source.display(),
                rule_index
            ));
            break;
        };
        let body = &rest[..end];
        match parse_bash_rule_body(body) {
            Ok(mut rule) => {
                rule.source = Some(source.display().to_string());
                rule.rule_index = Some(rule_index);
                rules.push(rule);
            }
            Err(reason) => diagnostics.push(format!(
                "{}: skipped prefix_rule #{}: {}",
                source.display(),
                rule_index,
                reason
            )),
        }
        rest = &rest[end + 2..];
    }
    (rules, diagnostics)
}

fn parse_bash_rule_body(body: &str) -> Result<BashRule, String> {
    let pattern =
        parse_pattern_field(body).ok_or_else(|| "missing or invalid pattern".to_string())?;
    let decision = match parse_string_field(body, "decision").as_deref() {
        Some("forbidden") => RuleDecision::Deny,
        Some("prompt") => RuleDecision::Ask,
        Some("allow") | None => RuleDecision::Allow,
        Some(value) => return Err(format!("invalid decision '{value}'")),
    };
    let justification = parse_string_field(body, "justification");
    let rule = BashRule {
        pattern,
        decision,
        justification,
        source: None,
        rule_index: None,
    };
    validate_bash_rule_examples(&rule, body)?;
    Ok(rule)
}

fn parse_pattern_field(body: &str) -> Option<Vec<Vec<String>>> {
    let start = find_field_assignment(body, "pattern")?;
    let after = &body[start..];
    let equals = after.find('=')?;
    let after = after[equals + 1..].trim_start();
    parse_pattern_array(after).map(|(items, _)| items)
}

fn parse_string_field(body: &str, field: &str) -> Option<String> {
    let start = find_field_assignment(body, field)?;
    let after = &body[start + field.len()..];
    let equals = after.find('=')?;
    parse_quoted(after[equals + 1..].trim_start()).map(|(value, _)| value)
}

fn parse_string_array_field(body: &str, field: &str) -> Option<Vec<String>> {
    let start = find_field_assignment(body, field)?;
    let after = &body[start + field.len()..];
    let equals = after.find('=')?;
    parse_string_array(after[equals + 1..].trim_start()).map(|(items, _)| items)
}

fn find_field_assignment(body: &str, field: &str) -> Option<usize> {
    for (idx, _) in body.match_indices(field) {
        let before = body[..idx].chars().next_back();
        let after_idx = idx + field.len();
        let after = &body[after_idx..];
        let field_boundary_before =
            before.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
        if field_boundary_before && after.trim_start().starts_with('=') {
            return Some(idx);
        }
    }
    None
}

fn validate_bash_rule_examples(rule: &BashRule, body: &str) -> Result<(), String> {
    let matches = parse_string_array_field(body, "match").unwrap_or_default();
    let not_matches = parse_string_array_field(body, "not_match").unwrap_or_default();

    for command in matches {
        if !rule.matches(&shell_words(&command)) {
            return Err(format!("match example does not match pattern: {command}"));
        }
    }
    for command in not_matches {
        if rule.matches(&shell_words(&command)) {
            return Err(format!("not_match example matches pattern: {command}"));
        }
    }
    Ok(())
}

fn parse_string_array(input: &str) -> Option<(Vec<String>, usize)> {
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if first != '[' {
        return None;
    }
    let mut idx = 1;
    let mut items = Vec::new();
    loop {
        let rest = input[idx..].trim_start();
        idx = input.len() - rest.len();
        if rest.starts_with(']') {
            return Some((items, idx + 1));
        }
        let (value, consumed) = parse_quoted(rest)?;
        items.push(value);
        idx += consumed;
        let rest = input[idx..].trim_start();
        idx = input.len() - rest.len();
        if rest.starts_with(',') {
            idx += 1;
        }
    }
}

fn parse_pattern_array(input: &str) -> Option<(Vec<Vec<String>>, usize)> {
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if first != '[' {
        return None;
    }
    let mut idx = 1;
    let mut items = Vec::new();
    loop {
        let rest = input[idx..].trim_start();
        idx = input.len() - rest.len();
        if rest.starts_with(']') {
            return Some((items, idx + 1));
        }
        if rest.starts_with('[') {
            let (nested, consumed) = parse_string_array(rest)?;
            items.push(nested);
            idx += consumed;
        } else {
            let (value, consumed) = parse_quoted(rest)?;
            items.push(vec![value]);
            idx += consumed;
        }
        let rest = input[idx..].trim_start();
        idx = input.len() - rest.len();
        if rest.starts_with(',') {
            idx += 1;
        }
    }
}

fn parse_quoted(input: &str) -> Option<(String, usize)> {
    let quote = input.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let mut out = String::new();
    let mut escaped = false;
    for (idx, ch) in input.char_indices().skip(1) {
        if escaped {
            out.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == quote {
            return Some((out, idx + ch.len_utf8()));
        } else {
            out.push(ch);
        }
    }
    None
}

// 这些命令默认禁止自动执行，因为它们可能提权、修改系统状态或破坏磁盘。
const BASH_FORBIDDEN_PREFIXES: &[&str] = &[
    "sudo",
    "su",
    "doas",
    "mkfs",
    "fdisk",
    "parted",
    "systemctl",
    "launchctl",
];

// 这些片段代表明确危险的 shell 写法；保持列表精简，只放确定的高危误操作。
const BASH_FORBIDDEN_SUBSTRINGS: &[&str] = &[
    "rm -rf /",
    "rm -fr /",
    "rm -r -f /",
    "rm -f -r /",
    "rm -rf ~",
    "rm -rf $home",
    ":(){ :|:& };:",
];

// 这些命令原则上可运行，但可能修改文件、状态、网络、进程或远端系统，默认先询问。
const BASH_PROMPT_COMMANDS: &[&str] = &[
    "rm", "rmdir", "unlink", "shred", "truncate", "dd", "kill", "pkill", "killall", "ssh", "scp",
    "rsync",
];

// 递归权限/属主修改可能影响工作区或宿主机的大范围文件，因此递归形式默认询问。
const BASH_RECURSIVE_PERMISSION_COMMANDS: &[&str] = &["chmod", "chown", "chgrp"];

// 会修改历史、远端、分支或工作区的 Git 操作。
const BASH_PROMPT_GIT_SUBCOMMANDS: &[&str] = &[
    "commit", "push", "pull", "reset", "rebase", "merge", "checkout", "switch", "clean", "restore",
];

// 会安装、升级或移除依赖的包管理器操作。
const BASH_PROMPT_JS_PACKAGE_SUBCOMMANDS: &[&str] =
    &["install", "i", "add", "upgrade", "remove", "uninstall"];
const BASH_PROMPT_BUN_SUBCOMMANDS: &[&str] = &["add", "install", "remove"];
const BASH_PROMPT_CARGO_SUBCOMMANDS: &[&str] = &["add", "install"];
const BASH_PROMPT_PY_PACKAGE_SUBCOMMANDS: &[&str] = &["install", "add", "remove"];
const BASH_PROMPT_UV_SUBCOMMANDS: &[&str] = &[
    "add", "remove", "sync", "lock", "run", "tool", "python", "venv", "build", "publish",
];
const BASH_PROMPT_UV_PIP_SUBCOMMANDS: &[&str] =
    &["install", "uninstall", "sync", "compile", "tree", "check"];
const BASH_PROMPT_SYSTEM_PACKAGE_COMMANDS: &[&str] =
    &["apt", "apt-get", "brew", "dnf", "yum", "pacman"];

// 网络下载命令写入磁盘时默认询问；下载后直接执行的管道在原始命令层默认禁止。
const BASH_DOWNLOAD_COMMANDS: &[&str] = &["curl", "wget"];
const BASH_DOWNLOAD_OUTPUT_FLAGS: &[&str] = &["-o", "-O", "--output"];

// 会创建/删除容器或清理本地资源的容器操作。
const BASH_PROMPT_DOCKER_SUBCOMMANDS: &[&str] = &["run", "rm"];

// 数据库或 schema 迁移工具可能修改持久化状态。
const BASH_MIGRATION_COMMANDS: &[&str] = &["diesel", "sqlx", "prisma", "sea-orm-cli"];
const BASH_MIGRATION_MARKERS: &[&str] = &["migrate", "migration"];

fn builtin_bash_decision(command: &str) -> PermissionDecision {
    let lower = command.to_ascii_lowercase();
    let compact = lower.split_whitespace().collect::<Vec<_>>().join(" ");
    if is_forbidden_bash_command(&compact) {
        return PermissionDecision::Deny {
            reason: "Blocked high-risk shell command".to_string(),
        };
    }

    let mut decision = PermissionDecision::Allow;
    for part in split_shell_commands(command) {
        let args = shell_words(&part);
        if bash_args_need_prompt(&args) {
            decision = decision.stricter(PermissionDecision::Ask);
        }
    }
    decision
}

fn is_forbidden_bash_command(command: &str) -> bool {
    is_download_and_execute(command)
        || BASH_FORBIDDEN_PREFIXES.iter().any(|prefix| {
            command == *prefix
                || command
                    .strip_prefix(prefix)
                    .is_some_and(|rest| rest.starts_with(' '))
        })
        || BASH_FORBIDDEN_SUBSTRINGS
            .iter()
            .any(|needle| command.contains(needle))
}

fn is_download_and_execute(command: &str) -> bool {
    BASH_DOWNLOAD_COMMANDS.iter().any(|cmd| {
        command.contains(&format!("{cmd} "))
            && (command.contains("| sh") || command.contains("| bash"))
    })
}

fn bash_args_need_prompt(args: &[String]) -> bool {
    let Some(cmd) = args.first().map(|arg| arg.as_str()) else {
        return false;
    };
    if BASH_PROMPT_COMMANDS.contains(&cmd) {
        return true;
    }
    match cmd {
        cmd if BASH_RECURSIVE_PERMISSION_COMMANDS.contains(&cmd) => {
            args.iter().any(|arg| arg == "-R" || arg == "--recursive")
        }
        "git" => args
            .get(1)
            .is_some_and(|sub| BASH_PROMPT_GIT_SUBCOMMANDS.contains(&sub.as_str())),
        "npm" | "pnpm" | "yarn" => args
            .get(1)
            .is_some_and(|sub| BASH_PROMPT_JS_PACKAGE_SUBCOMMANDS.contains(&sub.as_str())),
        "bun" => args
            .get(1)
            .is_some_and(|sub| BASH_PROMPT_BUN_SUBCOMMANDS.contains(&sub.as_str())),
        "cargo" => args
            .get(1)
            .is_some_and(|sub| BASH_PROMPT_CARGO_SUBCOMMANDS.contains(&sub.as_str())),
        "pip" => args
            .get(1)
            .is_some_and(|sub| BASH_PROMPT_PY_PACKAGE_SUBCOMMANDS.contains(&sub.as_str())),
        "uv" => uv_args_need_prompt(args),
        cmd if BASH_PROMPT_SYSTEM_PACKAGE_COMMANDS.contains(&cmd) => true,
        cmd if BASH_DOWNLOAD_COMMANDS.contains(&cmd) => args
            .iter()
            .any(|arg| BASH_DOWNLOAD_OUTPUT_FLAGS.contains(&arg.as_str())),
        "docker" => args.get(1).is_some_and(|sub| {
            BASH_PROMPT_DOCKER_SUBCOMMANDS.contains(&sub.as_str())
                || args.windows(2).any(|w| w == ["system", "prune"])
        }),
        cmd if BASH_MIGRATION_COMMANDS.contains(&cmd) => args
            .iter()
            .any(|arg| BASH_MIGRATION_MARKERS.contains(&arg.as_str())),
        _ => false,
    }
}

fn uv_args_need_prompt(args: &[String]) -> bool {
    let Some(sub) = args.get(1).map(|arg| arg.as_str()) else {
        return false;
    };
    if sub == "pip" {
        return args
            .get(2)
            .is_some_and(|pip_sub| BASH_PROMPT_UV_PIP_SUBCOMMANDS.contains(&pip_sub.as_str()));
    }
    BASH_PROMPT_UV_SUBCOMMANDS.contains(&sub)
}

fn split_shell_commands(command: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut chars = command.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            current.push(ch);
            if ch == q {
                quote = None;
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
            current.push(ch);
        } else if ch == ';' || ch == '|' || ch == '\n' {
            push_part(&mut parts, &mut current);
            if ch == '|' && chars.peek() == Some(&'|') {
                chars.next();
            }
        } else if ch == '&' && chars.peek() == Some(&'&') {
            chars.next();
            push_part(&mut parts, &mut current);
        } else {
            current.push(ch);
        }
    }
    push_part(&mut parts, &mut current);
    if parts.is_empty() {
        vec![command.to_string()]
    } else {
        parts
    }
}

fn push_part(parts: &mut Vec<String>, current: &mut String) {
    let trimmed = current.trim();
    if !trimmed.is_empty() {
        parts.push(trimmed.to_string());
    }
    current.clear();
}

fn shell_words(command: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;
    let mut escaped = false;
    for ch in command.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                current.push(ch);
            }
            continue;
        }
        if ch == '"' || ch == '\'' {
            quote = Some(ch);
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                words.push(current.clone());
                current.clear();
            }
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

fn permission_path(preview: Option<&PermissionPreview>, raw_input: &Value) -> Option<PathBuf> {
    match preview {
        Some(PermissionPreview::Read(preview)) => Some(PathBuf::from(&preview.file_path)),
        Some(PermissionPreview::Search(preview)) => Some(PathBuf::from(&preview.path)),
        Some(PermissionPreview::Edit(preview)) | Some(PermissionPreview::Write(preview)) => {
            Some(PathBuf::from(&preview.path))
        }
        _ => raw_input
            .get("file_path")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    }
}

fn search_path(preview: Option<&PermissionPreview>, raw_input: &Value) -> Option<PathBuf> {
    match preview {
        Some(PermissionPreview::Search(preview)) => Some(PathBuf::from(&preview.path)),
        _ => raw_input
            .get("path")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    }
}

fn read_path(preview: Option<&PermissionPreview>, raw_input: &Value) -> Option<PathBuf> {
    match preview {
        Some(PermissionPreview::Read(preview)) => Some(PathBuf::from(&preview.file_path)),
        _ => raw_input
            .get("file_path")
            .and_then(Value::as_str)
            .map(PathBuf::from),
    }
}

fn is_private_path(path: &Path) -> bool {
    let normalized = normalize_path_string(path).to_ascii_lowercase();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == ".env"
        || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name == "id_rsa"
        || name == "id_ed25519"
        || normalized.contains("/.ssh/")
        || (name.contains("token") || name.contains("secret") || name.contains("credential"))
}

fn normalize_tool_name(tool: &str) -> String {
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

fn normalize_path_string(path: &Path) -> String {
    path.components()
        .as_path()
        .to_string_lossy()
        .replace('\\', "/")
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star = None;
    let mut match_i = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            match_i = ti;
            pi += 1;
        } else if let Some(star_i) = star {
            pi = star_i + 1;
            match_i += 1;
            ti = match_i;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(allow: &[&str], ask: &[&str], deny: &[&str]) -> RawPermissionConfig {
        RawPermissionConfig {
            allow: allow.iter().map(|s| s.to_string()).collect(),
            ask: ask.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn engine_with_tool_source(
        cwd: impl Into<PathBuf>,
        raw: RawPermissionConfig,
        source: &str,
    ) -> PermissionEngine {
        let mut rules = CompiledPermissions::default();
        let mut diagnostics = Vec::new();
        rules.extend_tool_rules(raw, Some(source.to_string()), &mut diagnostics);
        PermissionEngine {
            cwd: cwd.into(),
            home: None,
            rules,
            diagnostics,
        }
    }

    #[test]
    fn read_defaults_allow_workspace_and_tmp_but_ask_private_or_external() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &[]));
        let input = serde_json::json!({"file_path": "/repo/src/lib.rs"});
        assert_eq!(
            engine.decide("read", None, &input),
            PermissionDecision::Allow
        );
        let input = serde_json::json!({"file_path": "/tmp/log.txt"});
        assert_eq!(
            engine.decide("read", None, &input),
            PermissionDecision::Allow
        );
        let input = serde_json::json!({"file_path": "/repo/.env"});
        assert_eq!(engine.decide("read", None, &input), PermissionDecision::Ask);
        let input = serde_json::json!({"file_path": "/etc/hosts"});
        assert_eq!(engine.decide("read", None, &input), PermissionDecision::Ask);
    }

    #[test]
    fn deny_rule_overrides_allow_rule() {
        let engine = PermissionEngine::for_test("/repo", raw(&["Read"], &[], &["Read(**/.env)"]));
        let input = serde_json::json!({"file_path": "/repo/.env"});
        assert!(matches!(
            engine.decide("read", None, &input),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn configured_tool_ask_keeps_effective_rule_source() {
        let engine = engine_with_tool_source(
            "/repo",
            raw(&[], &["Read(**/*.toml)"], &[]),
            "/repo/.omini/permissions.toml",
        );
        let input = serde_json::json!({"file_path": "/repo/.omini/permissions.toml"});
        let check = engine.check("read", None, &input);
        assert_eq!(check.decision, PermissionDecision::Ask);
        assert_eq!(
            check.source,
            Some(PermissionSource {
                decision: "ask".to_string(),
                source: "/repo/.omini/permissions.toml".to_string(),
                rule: "Read(**/*.toml)".to_string(),
            })
        );
    }

    #[test]
    fn plan_profile_hard_denies_write_execution_tools() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &[]));

        for tool in ["edit", "write", "todo_write"] {
            assert!(matches!(
                engine.decide_for_profile(ActiveProfile::Plan, tool, None, &serde_json::json!({})),
                PermissionDecision::Deny { .. }
            ));
        }

        assert_eq!(
            engine.decide_for_profile(
                ActiveProfile::Plan,
                "subagent",
                None,
                &serde_json::json!({})
            ),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn builtin_ask_has_no_permission_source() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &[]));
        let input = serde_json::json!({"file_path": "/repo/.env"});
        let check = engine.check("read", None, &input);
        assert_eq!(check.decision, PermissionDecision::Ask);
        assert_eq!(check.source, None);
    }

    #[test]
    fn skill_defaults_allow_without_permission_prompt() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &[]));
        let input = serde_json::json!({"name": "commit-message"});
        assert_eq!(
            engine.decide("skill", None, &input),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn unsupported_tool_rules_emit_diagnostics() {
        let engine = engine_with_tool_source(
            "/repo",
            raw(
                &["Fetch(*)", "Bash(cargo test)", "Read(**/*.toml"],
                &[],
                &[],
            ),
            "/repo/.omini/permissions.toml",
        );
        assert_eq!(engine.rules.tool_rules.len(), 0);
        assert_eq!(engine.diagnostics.len(), 3);
        assert!(engine.diagnostics[0].contains("unsupported tool 'Fetch'"));
        assert!(engine.diagnostics[1].contains("Bash rules must be configured"));
        assert!(engine.diagnostics[2].contains("invalid permission rule syntax"));
    }

    #[test]
    fn bash_defaults_allow_safe_ask_git_pull_and_deny_root_remove() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &[]));
        let preview = PermissionPreview::Bash(BashPermissionPreview {
            command: "cargo test".to_string(),
            description: None,
            workdir: None,
            timeout: 120000,
        });
        assert_eq!(
            engine.decide("bash", Some(&preview), &serde_json::json!({})),
            PermissionDecision::Allow
        );
        let preview = PermissionPreview::Bash(BashPermissionPreview {
            command: "git pull".to_string(),
            description: None,
            workdir: None,
            timeout: 120000,
        });
        assert_eq!(
            engine.decide("bash", Some(&preview), &serde_json::json!({})),
            PermissionDecision::Ask
        );
        let preview = PermissionPreview::Bash(BashPermissionPreview {
            command: "rm -rf /".to_string(),
            description: None,
            workdir: None,
            timeout: 120000,
        });
        assert!(matches!(
            engine.decide("bash", Some(&preview), &serde_json::json!({})),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn bash_uv_mutating_or_executing_commands_ask() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &[]));
        for command in ["uv run main.py", "uv sync", "uv pip install pytest"] {
            let preview = PermissionPreview::Bash(BashPermissionPreview {
                command: command.to_string(),
                description: None,
                workdir: None,
                timeout: 120000,
            });
            assert_eq!(
                engine.decide("bash", Some(&preview), &serde_json::json!({})),
                PermissionDecision::Ask,
                "{command}"
            );
        }
    }

    #[test]
    fn bash_prompt_rule_keeps_rules_file_source() {
        let (bash_rules, diagnostics) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
 pattern = ["cargo", "test"],
 decision = "prompt",
)
"#,
            Path::new("/repo/.omini/rules/default.rules"),
        );
        assert!(diagnostics.is_empty());
        let engine = PermissionEngine {
            cwd: PathBuf::from("/repo"),
            home: None,
            rules: CompiledPermissions {
                tool_rules: Vec::new(),
                bash_rules,
            },
            diagnostics: Vec::new(),
        };
        let preview = PermissionPreview::Bash(BashPermissionPreview {
            command: "cargo test".to_string(),
            description: None,
            workdir: None,
            timeout: 120000,
        });
        let check = engine.check("bash", Some(&preview), &serde_json::json!({}));
        assert_eq!(check.decision, PermissionDecision::Ask);
        assert_eq!(
            check.source,
            Some(PermissionSource {
                decision: "prompt".to_string(),
                source: "/repo/.omini/rules/default.rules".to_string(),
                rule: "prefix_rule #1".to_string(),
            })
        );
    }

    #[test]
    fn subagent_rule_matches_agent_name() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &["Subagent(explorer)"]));
        let input = serde_json::json!({"name": "explorer"});
        assert!(matches!(
            engine.decide("subagent", None, &input),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn subagent_defaults_to_allow() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &[]));
        let input = serde_json::json!({"name": "explorer"});
        assert_eq!(
            engine.decide("subagent", None, &input),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn ask_user_defaults_to_allow() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &[]));
        let input = serde_json::json!({
            "questions": [{
                "id": "choice",
                "header": "Choice",
                "question": "Which option should be used?",
                "options": [
                    {"label": "A", "description": "Use A."},
                    {"label": "B", "description": "Use B."}
                ]
            }]
        });
        assert_eq!(
            engine.decide("ask_user", None, &input),
            PermissionDecision::Allow
        );
    }

    #[test]
    fn search_defaults_to_allow_but_honors_deny_rules() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &[]));
        let input = serde_json::json!({"query": "ToolRegistry", "mode": "content"});
        assert_eq!(
            engine.decide("search", None, &input),
            PermissionDecision::Allow
        );
        let input = serde_json::json!({"query": "ToolRegistry", "path": "/repo/src"});
        assert_eq!(
            engine.decide("search", None, &input),
            PermissionDecision::Allow
        );
        let input = serde_json::json!({"query": "ToolRegistry", "path": "/tmp/cache"});
        assert_eq!(
            engine.decide("search", None, &input),
            PermissionDecision::Allow
        );
        let input = serde_json::json!({"query": "KEY", "path": "/repo/.env"});
        assert_eq!(
            engine.decide("search", None, &input),
            PermissionDecision::Ask
        );
        let input = serde_json::json!({"query": "ToolRegistry", "path": "/home/user/.omini"});
        assert_eq!(
            engine.decide("search", None, &input),
            PermissionDecision::Ask
        );

        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &["Search"]));
        assert!(matches!(
            engine.decide("search", None, &input),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn search_path_rules_match_search_preview_path() {
        let engine = PermissionEngine::for_test("/repo", raw(&[], &[], &["Search(**/.env)"]));
        let preview = PermissionPreview::Search(crate::types::events::SearchPermissionPreview {
            query: "KEY".to_string(),
            mode: "content".to_string(),
            path: "/repo/.env".to_string(),
        });
        let input = serde_json::json!({"query": "KEY", "path": "/repo/.env"});

        assert!(matches!(
            engine.decide("search", Some(&preview), &input),
            PermissionDecision::Deny { .. }
        ));
    }

    #[test]
    fn parses_bash_prefix_rule() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
 pattern = ["git", "status"],
 decision = "allow",
)
"#,
        );
        assert_eq!(rules.len(), 1);
        assert!(rules[0].matches(&["git".to_string(), "status".to_string()]));
    }

    #[test]
    fn bash_rule_examples_validate_on_load() {
        let (rules, diagnostics) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
 pattern = ["git", "status"],
 decision = "allow",
 match = [
   "git status",
 ],
 not_match = [
   "git push",
 ],
)

prefix_rule(
 pattern = ["git", "push"],
 decision = "allow",
 match = [
   "git status",
 ],
)
"#,
            Path::new("example.rules"),
        );
        assert_eq!(rules.len(), 1);
        assert!(rules[0].matches(&["git".to_string(), "status".to_string()]));
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("match example does not match pattern"));
    }
}
