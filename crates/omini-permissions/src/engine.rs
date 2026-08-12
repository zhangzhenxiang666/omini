//! `PermissionEngine` 主体：决策流程、内建工具默认策略、profile 策略。

use std::path::{Path, PathBuf};

use omini_config::permissions::PermissionSources;
use omini_domain::events::{
    ActiveProfile, BashPermissionPreview, PermissionPreview, PermissionSource,
};
use serde_json::Value;

use crate::bash_parser::{BashRule, RuleDecision};
use crate::bash_safety::check_builtin_safety_deny;
use crate::embedded::EMBEDDED_BASH_POLICY;
use crate::path_matcher;
use crate::shell::{shell_words, split_shell_commands};
use crate::tool_rules::{self, ToolRule};

/// 权限决策结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionDecision {
    Allow,
    Ask,
    Deny { reason: String },
}

/// 权限决策结果附带来源信息，用于 UI 展示决策依据。
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
}

fn stricter_check(current: PermissionCheck, next: PermissionCheck) -> PermissionCheck {
    if next.decision.rank() > current.decision.rank() {
        next
    } else {
        current
    }
}

/// 用户 `.rules` 文件解析后的 bash 策略，按首命令分桶的 HashMap 结构。
#[derive(Debug, Clone)]
struct UserBashPolicy {
    deny_by_cmd: std::collections::HashMap<String, Vec<BashRule>>,
    ask_by_cmd: std::collections::HashMap<String, Vec<BashRule>>,
    allow_by_cmd: std::collections::HashMap<String, Vec<BashRule>>,
}

/// 运行时权限决策引擎。
///
/// 持有 per-thread 的工具规则和用户 bash 规则，通过全局引用访问编译时内嵌策略。
#[derive(Debug, Clone)]
pub struct PermissionEngine {
    pub(crate) cwd: PathBuf,
    pub(crate) home: Option<PathBuf>,
    tool_rules: Vec<ToolRule>,
    user_bash: UserBashPolicy,
    diagnostics: Vec<String>,
}

impl PermissionEngine {
    pub fn empty(cwd: impl Into<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            home: None,
            tool_rules: Vec::new(),
            user_bash: UserBashPolicy {
                deny_by_cmd: std::collections::HashMap::new(),
                ask_by_cmd: std::collections::HashMap::new(),
                allow_by_cmd: std::collections::HashMap::new(),
            },
            diagnostics: Vec::new(),
        }
    }

    pub fn from_sources(
        cwd: impl Into<PathBuf>,
        home: Option<PathBuf>,
        sources: PermissionSources,
    ) -> Self {
        let cwd = cwd.into();
        let mut tool_rules = Vec::new();
        let mut diagnostics = sources.diagnostics().to_vec();

        if let Some((raw, path)) = sources.user_raw {
            tool_rules::extend_tool_rules(
                &mut tool_rules,
                raw,
                Some(path.display().to_string()),
                &mut diagnostics,
            );
        }
        if let Some((raw, path)) = sources.project_raw {
            tool_rules::extend_tool_rules(
                &mut tool_rules,
                raw,
                Some(path.display().to_string()),
                &mut diagnostics,
            );
        }

        // 解析用户 bash 规则并按首命令分桶。
        let mut user_deny_rules = Vec::new();
        let mut user_ask_rules = Vec::new();
        let mut user_allow_rules = Vec::new();

        for file in sources.bash_rule_files {
            let (parsed, mut warnings) = crate::bash_parser::parse_bash_rules_with_diagnostics(
                &file.content,
                file.path.display(),
            );
            for rule in parsed {
                match rule.decision {
                    RuleDecision::Deny => user_deny_rules.push(rule),
                    RuleDecision::Ask => user_ask_rules.push(rule),
                    RuleDecision::Allow => user_allow_rules.push(rule),
                }
            }
            diagnostics.append(&mut warnings);
        }

        let user_bash = UserBashPolicy {
            deny_by_cmd: group_by_first_command(user_deny_rules),
            ask_by_cmd: group_by_first_command(user_ask_rules),
            allow_by_cmd: group_by_first_command(user_allow_rules),
        };

        Self {
            cwd,
            home,
            tool_rules,
            user_bash,
            diagnostics,
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
        // Plan profile 硬禁 edit/write/todo_write。
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

        // 工具规则匹配。
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

        // 内建工具默认策略。
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

    /// Plan profile 策略：硬禁 edit/write/todo_write。
    pub fn profile_policy(
        &self,
        active_profile: ActiveProfile,
        tool_name: &str,
    ) -> Option<PermissionCheck> {
        let tool = tool_rules::normalize_tool_name(tool_name);
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

    fn decide_tool_rules(
        &self,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &Value,
    ) -> Option<PermissionCheck> {
        let mut decision: Option<PermissionCheck> = None;
        for rule in &self.tool_rules {
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

    /// 内建工具默认策略：read/search 按路径判断，edit/write 永远 ask，
    /// `todo_write`、`ask_user`、`skill` 和 Agent task 控制工具默认始终允许。
    fn decide_builtin(
        &self,
        tool_name: &str,
        preview: Option<&PermissionPreview>,
        raw_input: &Value,
    ) -> PermissionDecision {
        match tool_name {
            "read" | "view_image" => match path_matcher::read_path(preview, raw_input) {
                Some(path) if path_matcher::is_private_path(&path) => PermissionDecision::Ask,
                Some(path) if self.is_under_cwd_or_tmp(&path) => PermissionDecision::Allow,
                Some(_) => PermissionDecision::Ask,
                None => PermissionDecision::Ask,
            },
            "search" => {
                match path_matcher::search_path(preview, raw_input)
                    .map(|path| self.input_path(path))
                {
                    Some(path) if path_matcher::is_private_path(&path) => PermissionDecision::Ask,
                    Some(path) if self.is_under_cwd_or_tmp(&path) => PermissionDecision::Allow,
                    Some(_) => PermissionDecision::Ask,
                    None => PermissionDecision::Allow,
                }
            }
            "edit" | "write" => PermissionDecision::Ask,
            "todo_write" => PermissionDecision::Allow,
            "ask_user" | "skill" | "spawn_agent" | "run_agent" | "get_task" | "cancel_task" => {
                PermissionDecision::Allow
            }
            _ => PermissionDecision::Ask,
        }
    }

    /// Bash 决策流程：
    /// 0. 代码层安全底线检查（不可覆盖）
    /// 1. 对每个子命令按优先级链查找匹配规则
    /// 2. 多子命令取 strictest 结果
    fn decide_bash(&self, preview: &BashPermissionPreview) -> PermissionCheck {
        // 步骤 0：代码层安全底线 — rm -rf /、curl|sh、fork bomb、mkfs.* 等。
        if let Some(deny) = check_builtin_safety_deny(&preview.command) {
            return PermissionCheck {
                decision: deny,
                source: None,
            };
        }

        let embedded = &*EMBEDDED_BASH_POLICY;
        let mut result: Option<PermissionCheck> = None;

        for command in split_shell_commands(&preview.command) {
            let args = shell_words(&command);
            if args.is_empty() {
                continue;
            }
            let cmd = &args[0];
            let decision = self.decide_bash_single(cmd, &args, embedded);
            result = Some(match result {
                Some(current) => stricter_check(current, decision),
                None => decision,
            });
        }

        result.unwrap_or(PermissionCheck {
            decision: PermissionDecision::Ask, // 默认行为：未知命令 ask
            source: None,
        })
    }

    /// 单个子命令的决策链：
    /// a. 内嵌 deny → Deny
    /// b. 用户 deny → Deny
    /// c. 用户 allow → Allow（覆盖内嵌 ask）
    /// d. 用户 ask → Ask
    /// e. 内嵌 ask → Ask
    /// f. 内嵌 allow → Allow
    /// g. 无匹配 → Ask（默认行为）
    fn decide_bash_single(
        &self,
        cmd: &str,
        args: &[String],
        embedded: &crate::embedded::EmbeddedBashPolicy,
    ) -> PermissionCheck {
        // a. 内嵌 deny
        if let Some(rules) = embedded.deny_by_cmd.get(cmd) {
            for rule in rules {
                if rule.matches_suffix(args) {
                    return PermissionCheck {
                        decision: PermissionDecision::Deny {
                            reason: rule
                                .justification
                                .clone()
                                .unwrap_or_else(|| "Permission denied by embedded rule".into()),
                        },
                        source: None, // 内嵌规则不暴露 source
                    };
                }
            }
        }

        // b. 用户 deny
        if let Some(rules) = self.user_bash.deny_by_cmd.get(cmd) {
            for rule in rules {
                if rule.matches_suffix(args) {
                    return PermissionCheck {
                        decision: PermissionDecision::Deny {
                            reason: rule
                                .justification
                                .clone()
                                .unwrap_or_else(|| "Permission denied by bash rule".into()),
                        },
                        source: bash_rule_permission_source(rule),
                    };
                }
            }
        }

        // c. 用户 allow（覆盖内嵌 ask）
        if let Some(rules) = self.user_bash.allow_by_cmd.get(cmd) {
            for rule in rules {
                if rule.matches_suffix(args) {
                    return PermissionCheck {
                        decision: PermissionDecision::Allow,
                        source: bash_rule_permission_source(rule),
                    };
                }
            }
        }

        // d. 用户 ask
        if let Some(rules) = self.user_bash.ask_by_cmd.get(cmd) {
            for rule in rules {
                if rule.matches_suffix(args) {
                    return PermissionCheck {
                        decision: PermissionDecision::Ask,
                        source: bash_rule_permission_source(rule),
                    };
                }
            }
        }

        // e. 内嵌 ask
        if let Some(rules) = embedded.ask_by_cmd.get(cmd) {
            for rule in rules {
                if rule.matches_suffix(args) {
                    return PermissionCheck {
                        decision: PermissionDecision::Ask,
                        source: None,
                    };
                }
            }
        }

        // f. 内嵌 allow
        if let Some(rules) = embedded.allow_by_cmd.get(cmd) {
            for rule in rules {
                if rule.matches_suffix(args) {
                    return PermissionCheck {
                        decision: PermissionDecision::Allow,
                        source: None,
                    };
                }
            }
        }

        // g. 无匹配 → Ask（默认行为，从 Allow 改为 Ask）
        PermissionCheck {
            decision: PermissionDecision::Ask,
            source: None,
        }
    }

    fn is_under_cwd_or_tmp(&self, path: &Path) -> bool {
        let path = path_matcher::normalize_lexically(path);
        path.starts_with(path_matcher::normalize_lexically(&self.cwd))
            || path.starts_with(Path::new("/tmp"))
    }

    fn input_path(&self, path: PathBuf) -> PathBuf {
        if path.is_absolute() {
            path
        } else {
            self.cwd.join(path)
        }
    }
}

/// 从 bash 规则构建 `PermissionSource`，用于 UI 展示决策依据。
fn bash_rule_permission_source(rule: &BashRule) -> Option<PermissionSource> {
    rule.source.as_ref().map(|source| {
        let rule_text = rule
            .rule_index
            .map(|index| format!("prefix_rule #{index}"))
            .unwrap_or_else(|| {
                format!(
                    "prefix_rule {}",
                    crate::bash_parser::format_bash_pattern(&rule.pattern)
                )
            });
        PermissionSource {
            decision: match rule.decision {
                RuleDecision::Allow => "allow",
                RuleDecision::Ask => "prompt",
                RuleDecision::Deny => "forbidden",
            }
            .to_string(),
            source: source.clone(),
            rule: rule_text,
        }
    })
}

/// 将 `BashRule` 列表按 `pattern[0]` 的所有 alternatives 分桶到 HashMap。
/// 与 `embedded::group_by_first_command` 逻辑相同，但用于用户规则。
fn group_by_first_command(
    rules: Vec<BashRule>,
) -> std::collections::HashMap<String, Vec<BashRule>> {
    let mut map: std::collections::HashMap<String, Vec<BashRule>> =
        std::collections::HashMap::new();
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
