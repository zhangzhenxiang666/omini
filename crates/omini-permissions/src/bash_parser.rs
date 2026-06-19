use crate::shell::shell_words;
/// `.rules` DSL 解析器：解析 `prefix_rule(...)` 语法的 bash 权限规则。
use std::fmt::Display;

/// 单条 bash 前缀规则，解析自 `.rules` 文件中的 `prefix_rule(...)` 块。
#[derive(Debug, Clone)]
pub(crate) struct BashRule {
    /// 模式位置序列，每个位置是一组可选字符串（alternatives）。
    pub pattern: Vec<Vec<String>>,
    pub decision: RuleDecision,
    pub justification: Option<String>,
    /// 规则来源文件路径（用于 diagnostics 和 PermissionSource 追踪）。
    pub source: Option<String>,
    /// 在源文件中的序号（从 1 开始）。
    pub rule_index: Option<usize>,
}

/// 规则决策类型，对应 `.rules` DSL 中的 `decision` 字段值。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RuleDecision {
    /// DSL 中 `decision = "allow"`。
    Allow,
    /// DSL 中 `decision = "prompt"`。
    Ask,
    /// DSL 中 `decision = "forbidden"`。
    Deny,
}

impl RuleDecision {
    pub(crate) fn label(self) -> &'static str {
        match self {
            RuleDecision::Allow => "allow",
            RuleDecision::Ask => "ask",
            RuleDecision::Deny => "deny",
        }
    }
}

impl BashRule {
    /// 后缀匹配：跳过 args[0]（已通过 HashMap key 匹配首命令），
    /// 从 args[1..] 开始对比 pattern[0..]（已剥离首命令位置）。
    pub(crate) fn matches_suffix(&self, args: &[String]) -> bool {
        let suffix_args = if args.is_empty() { args } else { &args[1..] };
        if suffix_args.len() < self.pattern.len() {
            return false;
        }
        self.pattern
            .iter()
            .zip(suffix_args.iter())
            .all(|(allowed, arg)| allowed.iter().any(|candidate| candidate == arg))
    }

    /// 完整模式匹配（用于加载时验证 match/not_match 示例）。
    pub(crate) fn matches(&self, args: &[String]) -> bool {
        if args.len() < self.pattern.len() {
            return false;
        }
        self.pattern
            .iter()
            .zip(args.iter())
            .all(|(allowed, arg)| allowed.iter().any(|candidate| candidate == arg))
    }
}

/// 将 bash pattern 格式化为可读字符串，用于 diagnostics 显示。
pub(crate) fn format_bash_pattern(pattern: &[Vec<String>]) -> String {
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

/// 测试辅助：解析 bash 规则内容，忽略 diagnostics。
#[cfg(test)]
pub(crate) fn parse_bash_rules(content: &str) -> Vec<BashRule> {
    parse_bash_rules_with_diagnostics(content, "<inline>").0
}

/// 解析 `.rules` 文件内容中的所有 `prefix_rule(...)` 块。
/// `source` 参数接受 `impl Display`，支持文件路径和 `"<embedded:deny>"` 等字符串标识。
/// 支持单行和多行格式，通过括号深度追踪定位闭合 `)`（跳过引号内的字符）。
pub(crate) fn parse_bash_rules_with_diagnostics(
    content: &str,
    source: impl Display,
) -> (Vec<BashRule>, Vec<String>) {
    let mut rules = Vec::new();
    let mut diagnostics = Vec::new();
    let mut rest = content;
    let mut rule_index = 0usize;
    while let Some(start) = rest.find("prefix_rule(") {
        rule_index += 1;
        rest = &rest[start + "prefix_rule(".len()..];
        // 用括号深度匹配找闭合 `)`，而非硬编码 `\n)`，同时支持单行和多行格式。
        let Some(end) = find_closing_paren(rest) else {
            diagnostics.push(format!(
                "{source}: prefix_rule #{rule_index} 缺少闭合的 ')'"
            ));
            break;
        };
        let body = &rest[..end];
        match parse_bash_rule_body(body) {
            Ok(mut rule) => {
                rule.source = Some(source.to_string());
                rule.rule_index = Some(rule_index);
                rules.push(rule);
            }
            Err(reason) => diagnostics.push(format!(
                "{source}: 跳过 prefix_rule #{rule_index}: {reason}"
            )),
        }
        rest = &rest[end + 1..];
    }
    (rules, diagnostics)
}

/// 在输入中查找与 `prefix_rule(` 的 `(` 相匹配的闭合 `)`。
/// 追踪括号深度，跳过引号内的字符（包括转义），返回闭括号在 `input` 中的字节偏移。
fn find_closing_paren(input: &str) -> Option<usize> {
    let mut depth = 1usize;
    let chars = input.char_indices();
    let mut in_quote: Option<char> = None;
    let mut escaped = false;

    for (idx, ch) in chars {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' {
            if in_quote.is_some() {
                escaped = true;
            }
            continue;
        }
        if let Some(q) = in_quote {
            if ch == q {
                in_quote = None;
            }
            continue;
        }
        match ch {
            '"' | '\'' => in_quote = Some(ch),
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    // === 基础解析测试 ===

    #[test]
    fn parses_single_line_rule() {
        let rules = parse_bash_rules(
            r#"prefix_rule(pattern = ["sudo"], decision = "forbidden", justification = "Privilege escalation")"#,
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, vec![vec!["sudo"]]);
        assert_eq!(rules[0].decision, RuleDecision::Deny);
        assert_eq!(
            rules[0].justification.as_deref(),
            Some("Privilege escalation")
        );
    }

    #[test]
    fn parses_multiple_single_line_rules() {
        let rules = parse_bash_rules(
            r#"prefix_rule(pattern = ["sudo"], decision = "forbidden")
prefix_rule(pattern = ["ls"], decision = "allow")
prefix_rule(pattern = ["curl"], decision = "prompt")
"#,
        );
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].decision, RuleDecision::Deny);
        assert_eq!(rules[1].decision, RuleDecision::Allow);
        assert_eq!(rules[2].decision, RuleDecision::Ask);
    }

    #[test]
    fn parses_mixed_single_and_multi_line_rules() {
        let rules = parse_bash_rules(
            r#"prefix_rule(pattern = ["rm"], decision = "prompt")
prefix_rule(
  pattern = ["git", "push"],
  decision = "prompt",
)
prefix_rule(pattern = ["ls"], decision = "allow")
"#,
        );
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].decision, RuleDecision::Ask);
        assert_eq!(rules[1].decision, RuleDecision::Ask);
        assert_eq!(rules[1].pattern, vec![vec!["git"], vec!["push"]]);
        assert_eq!(rules[2].decision, RuleDecision::Allow);
    }

    #[test]
    fn parses_single_simple_rule() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = ["git", "status"],
  decision = "allow",
)
"#,
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, vec![vec!["git"], vec!["status"]]);
        assert_eq!(rules[0].decision, RuleDecision::Allow);
    }

    #[test]
    fn parses_multiple_rules() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = ["rm"],
  decision = "forbidden",
  justification = "File deletion",
)
prefix_rule(
  pattern = ["ls"],
  decision = "allow",
)
prefix_rule(
  pattern = ["curl"],
  decision = "prompt",
  justification = "Network download",
)
"#,
        );
        assert_eq!(rules.len(), 3);
        assert_eq!(rules[0].decision, RuleDecision::Deny);
        assert_eq!(rules[0].justification.as_deref(), Some("File deletion"));
        assert_eq!(rules[1].decision, RuleDecision::Allow);
        assert_eq!(rules[2].decision, RuleDecision::Ask);
        assert_eq!(rules[2].justification.as_deref(), Some("Network download"));
    }

    // === alternatives 模式测试 ===

    #[test]
    fn parses_alternatives_in_pattern() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = [["npm", "pnpm", "yarn"], ["install", "add"]],
  decision = "prompt",
)
"#,
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].pattern,
            vec![vec!["npm", "pnpm", "yarn"], vec!["install", "add"],]
        );
    }

    #[test]
    fn parses_mixed_single_and_alternatives() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = ["git", ["commit", "push", "pull"]],
  decision = "prompt",
)
"#,
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(
            rules[0].pattern,
            vec![vec!["git"], vec!["commit", "push", "pull"]],
        );
    }

    #[test]
    fn alternatives_match_correctly() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = [["npm", "pnpm", "yarn"], ["install", "add"]],
  decision = "prompt",
)
"#,
        );
        assert!(rules[0].matches(&s("npm install")));
        assert!(rules[0].matches(&s("yarn add")));
        assert!(rules[0].matches(&s("pnpm install")));
        assert!(!rules[0].matches(&s("npm test")));
        assert!(!rules[0].matches(&s("cargo install")));
    }

    // === 决策类型测试 ===

    #[test]
    fn parses_forbidden_decision() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = ["sudo"],
  decision = "forbidden",
)
"#,
        );
        assert_eq!(rules[0].decision, RuleDecision::Deny);
    }

    #[test]
    fn parses_prompt_decision() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = ["rm"],
  decision = "prompt",
)
"#,
        );
        assert_eq!(rules[0].decision, RuleDecision::Ask);
    }

    #[test]
    fn parses_allow_decision() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = ["ls"],
  decision = "allow",
)
"#,
        );
        assert_eq!(rules[0].decision, RuleDecision::Allow);
    }

    #[test]
    fn defaults_to_allow_when_decision_omitted() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = ["echo"],
)
"#,
        );
        assert_eq!(rules[0].decision, RuleDecision::Allow);
    }

    // === 诊断测试 ===

    #[test]
    fn diagnostic_for_missing_closing_paren() {
        let (_, diag) = parse_bash_rules_with_diagnostics(
            "prefix_rule(\n  pattern = [\"rm\"],\n",
            "test.rules",
        );
        assert_eq!(diag.len(), 1);
        assert!(diag[0].contains("缺少闭合的 ')'"));
    }

    #[test]
    fn diagnostic_for_invalid_decision() {
        let (_, diag) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  pattern = ["rm"],
  decision = "invalid_value",
)
"#,
            "test.rules",
        );
        assert_eq!(diag.len(), 1);
        assert!(diag[0].contains("invalid decision"));
    }

    #[test]
    fn diagnostic_for_missing_pattern() {
        let (_, diag) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  decision = "allow",
)
"#,
            "test.rules",
        );
        assert_eq!(diag.len(), 1);
        assert!(diag[0].contains("missing or invalid pattern"));
    }

    #[test]
    fn diagnostic_skips_bad_rule_and_continues() {
        let (rules, diag) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  pattern = ["bad"],
  decision = "nope",
)
prefix_rule(
  pattern = ["good"],
  decision = "allow",
)
"#,
            "test.rules",
        );
        assert_eq!(rules.len(), 1, "should parse the second valid rule");
        assert_eq!(rules[0].pattern, vec![vec!["good"]]);
        assert_eq!(diag.len(), 1);
        assert!(diag[0].contains("invalid decision"));
    }

    // === source 追踪测试 ===

    #[test]
    fn source_tracked_per_rule() {
        let rules = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  pattern = ["ls"],
  decision = "allow",
)
"#,
            "/home/user/.rules/test.rules",
        )
        .0;
        assert_eq!(
            rules[0].source.as_deref(),
            Some("/home/user/.rules/test.rules")
        );
        assert_eq!(rules[0].rule_index, Some(1));
    }

    #[test]
    fn source_accepts_string_identifier() {
        // 验证 source 参数支持 "<embedded:deny>" 等非路径字符串。
        let rules = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  pattern = ["sudo"],
  decision = "forbidden",
)
"#,
            "<embedded:deny>",
        )
        .0;
        assert_eq!(rules[0].source.as_deref(), Some("<embedded:deny>"));
    }

    // === match / not_match 示例验证测试 ===

    #[test]
    fn valid_match_examples_pass() {
        let (rules, diag) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  pattern = ["git", "push"],
  decision = "prompt",
  match = ["git push", "git push origin main"],
  not_match = ["git status", "git log"],
)
"#,
            "test.rules",
        );
        assert_eq!(rules.len(), 1);
        assert!(
            diag.is_empty(),
            "valid examples should not produce diagnostics"
        );
    }

    #[test]
    fn invalid_match_example_produces_diagnostic() {
        let (_, diag) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  pattern = ["git", "push"],
  decision = "prompt",
  match = ["git status"],
)
"#,
            "test.rules",
        );
        assert_eq!(diag.len(), 1);
        assert!(diag[0].contains("match example does not match pattern"));
    }

    #[test]
    fn invalid_not_match_example_produces_diagnostic() {
        let (_, diag) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  pattern = ["git", "push"],
  decision = "prompt",
  not_match = ["git push"],
)
"#,
            "test.rules",
        );
        assert_eq!(diag.len(), 1);
        assert!(diag[0].contains("not_match example matches pattern"));
    }

    // === 空输入和边界测试 ===

    #[test]
    fn empty_content_produces_no_rules() {
        let (rules, diag) = parse_bash_rules_with_diagnostics("", "empty.rules");
        assert!(rules.is_empty());
        assert!(diag.is_empty());
    }

    #[test]
    fn comments_only_produces_no_rules() {
        let (rules, diag) = parse_bash_rules_with_diagnostics(
            "# this is a comment\n# another comment\n",
            "comments.rules",
        );
        assert!(rules.is_empty());
        assert!(diag.is_empty());
    }

    #[test]
    fn single_quoted_strings_supported() {
        let rules = parse_bash_rules(
            r#"
prefix_rule(
  pattern = ['ls'],
  decision = 'allow',
)
"#,
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, vec![vec!["ls"]]);
        assert_eq!(rules[0].decision, RuleDecision::Allow);
    }

    // === format_bash_pattern 测试 ===

    #[test]
    fn format_bash_pattern_single_alternatives() {
        let pattern = vec![vec!["git".into()], vec!["push".into()]];
        assert_eq!(format_bash_pattern(&pattern), "(git push)");
    }

    #[test]
    fn format_bash_pattern_multiple_alternatives() {
        let pattern = vec![vec!["npm".into(), "pnpm".into()], vec!["install".into()]];
        assert_eq!(format_bash_pattern(&pattern), "([npm|pnpm] install)");
    }

    // === 辅助函数 ===

    fn s(command: &str) -> Vec<String> {
        crate::shell::shell_words(command)
    }
}
