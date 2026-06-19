//! 编译时嵌入的 `.rules` 文件 + `LazyLock` 全局预编译策略。
//!
//! 三个内嵌规则文件通过 `include_str!` 在编译时嵌入，首次访问时解析一次，
//! 后续所有 session 共享 `&EmbeddedBashPolicy` 引用，零 clone。

use std::collections::HashMap;
use std::sync::LazyLock;

use crate::bash_parser::{BashRule, parse_bash_rules_with_diagnostics};

// 编译时嵌入，`&'static str`，零 I/O。
static EMBEDDED_DENY_RAW: &str = include_str!("embedded_rules/deny.rules");
static EMBEDDED_ASK_RAW: &str = include_str!("embedded_rules/ask.rules");
static EMBEDDED_ALLOW_RAW: &str = include_str!("embedded_rules/allow.rules");

/// 全局共享的内嵌 bash 策略。首次访问时解析一次，后续所有 session 共享引用，零 clone。
pub(crate) struct EmbeddedBashPolicy {
    pub deny_by_cmd: HashMap<String, Vec<BashRule>>,
    pub ask_by_cmd: HashMap<String, Vec<BashRule>>,
    pub allow_by_cmd: HashMap<String, Vec<BashRule>>,
}

pub(crate) static EMBEDDED_BASH_POLICY: LazyLock<EmbeddedBashPolicy> = LazyLock::new(|| {
    // 只解析一次。内嵌规则由我们维护，diagnostics 应该为空。
    let (deny_rules, _) = parse_bash_rules_with_diagnostics(EMBEDDED_DENY_RAW, "<embedded:deny>");
    let (ask_rules, _) = parse_bash_rules_with_diagnostics(EMBEDDED_ASK_RAW, "<embedded:ask>");
    let (allow_rules, _) =
        parse_bash_rules_with_diagnostics(EMBEDDED_ALLOW_RAW, "<embedded:allow>");

    EmbeddedBashPolicy {
        deny_by_cmd: group_by_first_command(deny_rules),
        ask_by_cmd: group_by_first_command(ask_rules),
        allow_by_cmd: group_by_first_command(allow_rules),
    }
});

/// 将 `BashRule` 列表按 `pattern[0]` 的所有 alternatives 分桶到 HashMap。
/// 每条规则的首命令位置从 pattern 中剥离，只存 suffix pattern，
/// 因为首命令已通过 HashMap key 索引。
fn group_by_first_command(rules: Vec<BashRule>) -> HashMap<String, Vec<BashRule>> {
    let mut map: HashMap<String, Vec<BashRule>> = HashMap::new();
    for rule in rules {
        let first_alternatives = rule.pattern.first().cloned().unwrap_or_default();
        let suffix_rule = BashRule {
            pattern: rule.pattern.iter().skip(1).cloned().collect(),
            ..rule
        };
        for cmd in first_alternatives {
            map.entry(cmd).or_default().push(suffix_rule.clone());
        }
    }
    map
}
