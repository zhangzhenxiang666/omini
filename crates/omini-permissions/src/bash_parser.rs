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
    while let Some(start) = find_prefix_rule_start(rest) {
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

/// 查找注释和引号之外的下一条规则，避免把示例文本或被注释的规则当成配置。
fn find_prefix_rule_start(input: &str) -> Option<usize> {
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;

    for (idx, ch) in input.char_indices() {
        if comment {
            if ch == '\n' {
                comment = false;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == '\\' {
                escaped = true;
            } else if ch == active_quote {
                quote = None;
            }
            continue;
        }

        match ch {
            '#' => comment = true,
            '"' | '\'' => quote = Some(ch),
            'p' if input[idx..].starts_with("prefix_rule(")
                && input[..idx]
                    .chars()
                    .next_back()
                    .is_none_or(|before| !before.is_ascii_alphanumeric() && before != '_') =>
            {
                return Some(idx);
            }
            _ => {}
        }
    }
    None
}

/// 在输入中查找与 `prefix_rule(` 的 `(` 相匹配的闭合 `)`。
/// 追踪括号深度，跳过引号内的字符（包括转义），返回闭括号在 `input` 中的字节偏移。
fn find_closing_paren(input: &str) -> Option<usize> {
    let mut depth = 1usize;
    let chars = input.char_indices();
    let mut in_quote: Option<char> = None;
    let mut escaped = false;
    let mut comment = false;

    for (idx, ch) in chars {
        if comment {
            if ch == '\n' {
                comment = false;
            }
            continue;
        }
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
            '#' => comment = true,
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
    if pattern.is_empty() {
        return Err("pattern must not be empty".to_string());
    }
    if pattern.iter().any(Vec::is_empty) {
        return Err("pattern alternatives must not be empty".to_string());
    }
    if pattern.iter().flatten().any(String::is_empty) {
        return Err("pattern values must not be empty".to_string());
    }
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
    let mut quote = None;
    let mut escaped = false;
    let mut comment = false;

    for (idx, ch) in body.char_indices() {
        if comment {
            if ch == '\n' {
                comment = false;
            }
            continue;
        }
        if escaped {
            escaped = false;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == '\\' {
                escaped = true;
            } else if ch == active_quote {
                quote = None;
            }
            continue;
        }
        match ch {
            '#' => comment = true,
            '"' | '\'' => quote = Some(ch),
            _ if body[idx..].starts_with(field) => {
                let before = body[..idx].chars().next_back();
                let after = &body[idx + field.len()..];
                let field_boundary_before =
                    before.is_none_or(|ch| !ch.is_ascii_alphanumeric() && ch != '_');
                if field_boundary_before && after.trim_start().starts_with('=') {
                    return Some(idx);
                }
            }
            _ => {}
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
        } else if rest.starts_with(']') {
            return Some((items, idx + 1));
        } else {
            return None;
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
        } else if rest.starts_with(']') {
            return Some((items, idx + 1));
        } else {
            return None;
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
    use crate::bash_parser::{
        RuleDecision, format_bash_pattern, parse_bash_rules_with_diagnostics,
    };
    use crate::shell::shell_words;

    #[test]
    fn rule_document_mixed_shapes_preserve_all_parsed_fields_and_order() {
        let (rules, diagnostics) = parse_bash_rules_with_diagnostics(
            r#"
# leading comment
prefix_rule(pattern = ["sudo"], decision = "forbidden", justification = "Privilege (escalation)")
prefix_rule(
  pattern = [["npm", "pnpm"], ["install", "add"]],
  decision = "prompt",
)
prefix_rule(pattern = ["echo"])
prefix_rule(pattern = ['git', 'status'], decision = 'allow')
"#,
            "policy.rules",
        );

        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(rules.len(), 4);
        assert_eq!(rules[0].pattern, vec![vec!["sudo"]]);
        assert_eq!(rules[0].decision, RuleDecision::Deny);
        assert_eq!(
            rules[0].justification.as_deref(),
            Some("Privilege (escalation)")
        );
        assert_eq!(
            rules[1].pattern,
            vec![vec!["npm", "pnpm"], vec!["install", "add"]]
        );
        assert_eq!(rules[1].decision, RuleDecision::Ask);
        assert_eq!(rules[1].justification, None);
        assert_eq!(rules[2].pattern, vec![vec!["echo"]]);
        assert_eq!(rules[2].decision, RuleDecision::Allow);
        assert_eq!(rules[3].pattern, vec![vec!["git"], vec!["status"]]);
        assert_eq!(rules[3].decision, RuleDecision::Allow);
        assert_eq!(
            rules
                .iter()
                .map(|rule| (rule.source.as_deref(), rule.rule_index))
                .collect::<Vec<_>>(),
            vec![
                (Some("policy.rules"), Some(1)),
                (Some("policy.rules"), Some(2)),
                (Some("policy.rules"), Some(3)),
                (Some("policy.rules"), Some(4)),
            ]
        );
    }

    #[test]
    fn rule_pattern_prefix_and_alternatives_accept_only_matching_positions() {
        let (rules, diagnostics) = parse_bash_rules_with_diagnostics(
            r#"prefix_rule(pattern = [["npm", "pnpm"], ["install", "add"]])"#,
            "policy.rules",
        );
        assert_eq!(diagnostics, Vec::<String>::new());
        let rule = &rules[0];

        assert_eq!(
            [
                "npm install",
                "pnpm add package",
                "npm",
                "npm test",
                "cargo install",
                "",
            ]
            .map(|command| rule.matches(&shell_words(command))),
            [true, true, false, false, false, false]
        );
    }

    #[test]
    fn empty_or_comment_only_document_produces_no_rules_or_diagnostics() {
        for content in [
            "",
            "\n  \n",
            "# prefix_rule(pattern = [\"sudo\"])\n# comment\n",
        ] {
            let (rules, diagnostics) = parse_bash_rules_with_diagnostics(content, "empty.rules");
            assert!(rules.is_empty(), "unexpected rules for {content:?}");
            assert!(
                diagnostics.is_empty(),
                "unexpected diagnostics for {content:?}"
            );
        }
    }

    #[test]
    fn commented_out_rules_are_ignored_without_changing_real_rule_indices() {
        let (rules, diagnostics) = parse_bash_rules_with_diagnostics(
            r#"
# prefix_rule(pattern = ["sudo"], decision = "forbidden")
not_prefix_rule(pattern = ["sudo"], decision = "forbidden")
prefix_rule(pattern = ["cargo", "test"], decision = "allow") # prefix_rule(pattern = ["rm"])
"#,
            "comments.rules",
        );

        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, vec![vec!["cargo"], vec!["test"]]);
        assert_eq!(rules[0].rule_index, Some(1));
    }

    #[test]
    fn commented_or_quoted_field_names_do_not_override_real_fields() {
        let (rules, diagnostics) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  # pattern = ["sudo"]
  pattern = ["echo"],
  # decision = "forbidden"
  justification = "decision = \"forbidden\"",
)
"#,
            "comments.rules",
        );

        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, vec![vec!["echo"]]);
        assert_eq!(rules[0].decision, RuleDecision::Allow);
        assert_eq!(
            rules[0].justification.as_deref(),
            Some("decision = \"forbidden\"")
        );
    }

    #[test]
    fn invalid_rule_categories_report_in_order_and_later_valid_rule_survives() {
        let (rules, diagnostics) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(decision = "allow")
prefix_rule(pattern = ["bad-decision"], decision = "sometimes")
prefix_rule(pattern = [], decision = "allow")
prefix_rule(pattern = [[]], decision = "allow")
prefix_rule(pattern = [""], decision = "allow")
prefix_rule(pattern = ["missing-comma" "second"], decision = "allow")
prefix_rule(pattern = ["good"], decision = "prompt")
"#,
            "invalid.rules",
        );

        assert_eq!(
            diagnostics,
            vec![
                "invalid.rules: 跳过 prefix_rule #1: missing or invalid pattern",
                "invalid.rules: 跳过 prefix_rule #2: invalid decision 'sometimes'",
                "invalid.rules: 跳过 prefix_rule #3: pattern must not be empty",
                "invalid.rules: 跳过 prefix_rule #4: pattern alternatives must not be empty",
                "invalid.rules: 跳过 prefix_rule #5: pattern values must not be empty",
                "invalid.rules: 跳过 prefix_rule #6: missing or invalid pattern",
            ]
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, vec![vec!["good"]]);
        assert_eq!(rules[0].decision, RuleDecision::Ask);
        assert_eq!(rules[0].rule_index, Some(7));
    }

    #[test]
    fn unclosed_rule_reports_one_stable_error_and_stops_at_incomplete_input() {
        let (rules, diagnostics) = parse_bash_rules_with_diagnostics(
            "prefix_rule(\n  pattern = [\"rm\"],\n",
            "truncated.rules",
        );

        assert!(rules.is_empty());
        assert_eq!(
            diagnostics,
            ["truncated.rules: prefix_rule #1 缺少闭合的 ')'".to_string()]
        );
    }

    #[test]
    fn match_and_not_match_examples_validate_both_success_and_failure_paths() {
        let (rules, diagnostics) = parse_bash_rules_with_diagnostics(
            r#"
prefix_rule(
  pattern = ["git", "push"],
  decision = "prompt",
  match = ["git push", "git push origin main"],
  not_match = ["git status", "cargo push"],
)
"#,
            "valid.rules",
        );
        assert_eq!(rules.len(), 1);
        assert_eq!(diagnostics, Vec::<String>::new());

        for (example_field, example, reason) in [
            (
                "match",
                "git status",
                "match example does not match pattern: git status",
            ),
            (
                "not_match",
                "git push",
                "not_match example matches pattern: git push",
            ),
        ] {
            let content = format!(
                "prefix_rule(pattern = [\"git\", \"push\"], {example_field} = [\"{example}\"] )"
            );
            let (rules, diagnostics) =
                parse_bash_rules_with_diagnostics(&content, "examples.rules");
            assert!(rules.is_empty());
            assert_eq!(
                diagnostics,
                [format!("examples.rules: 跳过 prefix_rule #1: {reason}")]
            );
        }
    }

    #[test]
    fn quoted_parentheses_and_escaped_quotes_do_not_close_a_rule_early() {
        let (rules, diagnostics) = parse_bash_rules_with_diagnostics(
            r#"prefix_rule(pattern = ["echo", "value)"], decision = "forbidden", justification = "say \"stop)\"")"#,
            "quoted.rules",
        );

        assert_eq!(diagnostics, Vec::<String>::new());
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].pattern, vec![vec!["echo"], vec!["value)"]]);
        assert_eq!(rules[0].justification.as_deref(), Some("say \"stop)\""));
    }

    #[test]
    fn formatted_pattern_preserves_single_and_alternative_positions() {
        assert_eq!(
            format_bash_pattern(&[vec!["git".into()], vec!["push".into()]]),
            "(git push)"
        );
        assert_eq!(
            format_bash_pattern(&[
                vec!["npm".into(), "pnpm".into()],
                vec!["install".into(), "add".into()],
            ]),
            "([npm|pnpm] [install|add])"
        );
    }
}
