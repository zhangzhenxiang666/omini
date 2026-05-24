pub mod agents;
pub mod compact;
pub mod effort;
pub mod exit;
pub mod help;
pub mod init;
pub mod model;
pub mod new;
pub mod plan;
pub mod rename;
pub mod sessions;
pub mod skill;
pub mod thinking;

use crate::runtime::AgentRuntime;
use crate::types::events::{CommandKind, CommandResult, CommandSummary};
use async_trait::async_trait;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// 从用户输入中解析命令。
#[derive(Debug)]
pub struct ParsedCommand<'a> {
    pub name: &'a str,
    pub args: &'a str,
}

/// 解析用户输入行，如果以 `/` 开头则返回 `Some(ParsedCommand)`。
pub fn parse(input: &str) -> Option<ParsedCommand<'_>> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }
    let rest = &input[1..];
    if rest.is_empty() {
        return None;
    }
    let (name, args) = match rest.split_once(char::is_whitespace) {
        Some((n, a)) => (n, a.trim()),
        None => (rest, ""),
    };
    Some(ParsedCommand { name, args })
}

/// 命令实现 trait。
#[async_trait]
pub trait Command: Send + Sync {
    fn name(&self) -> &str;
    fn aliases(&self) -> &[&'static str];
    fn description(&self) -> &str;
    fn args_description(&self) -> Option<&'static str> {
        None
    }
    /// 是否需要额外参数（false = 选中即执行，true = 补全后让用户输入）。
    fn has_args(&self) -> bool {
        false
    }
    /// 命令抽屉排序权重，数值越小越靠前。
    fn sort_weight(&self) -> i32 {
        100
    }
    /// 命令类别，用于 UI 将内置命令和 skill 命令分组展示。
    fn kind(&self) -> CommandKind {
        CommandKind::Builtin
    }
    /// 执行命令。
    async fn execute(&self, runtime: &mut AgentRuntime, args: &str) -> CommandResult;
}

/// 命令注册表，按名称和别名索引。
pub struct CommandRegistry {
    by_name: HashMap<String, Arc<dyn Command>>,
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            by_name: HashMap::new(),
        }
    }

    pub fn register(&mut self, cmd: Arc<dyn Command>) {
        self.by_name.insert(cmd.name().to_string(), cmd.clone());
        for alias in cmd.aliases() {
            self.by_name.insert(alias.to_string(), cmd.clone());
        }
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Command>> {
        self.by_name.get(name)
    }

    /// 返回去重后的所有命令（按主名称）。
    pub fn all_commands(&self) -> Vec<&dyn Command> {
        let mut seen = HashSet::new();
        let mut cmds = Vec::new();
        for cmd in self.by_name.values() {
            if seen.insert(cmd.name()) {
                cmds.push(cmd.as_ref());
            }
        }
        cmds.sort_by(|a, b| {
            a.sort_weight()
                .cmp(&b.sort_weight())
                .then_with(|| a.name().cmp(b.name()))
        });
        cmds
    }

    /// 返回所有命令的摘要（供 TUI 自动补全 / 帮助使用）。
    pub fn summaries(&self) -> Vec<CommandSummary> {
        self.all_commands()
            .iter()
            .map(|cmd| CommandSummary {
                name: cmd.name().to_string(),
                aliases: cmd.aliases().iter().map(|s| s.to_string()).collect(),
                description: cmd.description().to_string(),
                sort_weight: cmd.sort_weight(),
                kind: cmd.kind(),
                has_args: cmd.has_args(),
                args_description: cmd.args_description(),
            })
            .collect()
    }
}

/// 注册所有内置命令。
pub fn register_default_commands(registry: &mut CommandRegistry) {
    registry.register(Arc::new(exit::ExitCommand));
    registry.register(Arc::new(effort::EffortCommand));
    registry.register(Arc::new(model::ModelCommand));
    registry.register(Arc::new(agents::AgentsCommand));
    registry.register(Arc::new(thinking::ThinkingCommand));
    registry.register(Arc::new(sessions::SessionsCommand));
    registry.register(Arc::new(new::NewCommand));
    registry.register(Arc::new(plan::PlanCommand));
    registry.register(Arc::new(compact::CompactCommand));
    registry.register(Arc::new(rename::RenameCommand));
    registry.register(Arc::new(init::InitCommand));
    registry.register(Arc::new(help::HelpCommand));
}

/// 注册所有动态 skill 命令。
pub fn register_skill_commands(
    registry: &mut CommandRegistry,
    skills: &crate::skills::SkillRegistry,
) {
    for spec in skills.skills.values() {
        if !spec.user_invocable {
            continue;
        }
        registry.register(Arc::new(skill::SkillCommand::new(spec.clone())));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_summaries_are_sorted_by_weight_then_name() {
        let mut registry = CommandRegistry::new();
        register_default_commands(&mut registry);

        let names: Vec<_> = registry
            .summaries()
            .into_iter()
            .map(|cmd| cmd.name)
            .collect();

        assert_eq!(
            names,
            vec![
                "sessions", "new", "plan", "compact", "model", "agents", "effort", "init",
                "rename", "thinking", "help", "exit"
            ]
        );
    }

    #[test]
    fn skill_commands_are_registered_from_skill_registry() {
        let mut registry = CommandRegistry::new();
        let cwd = std::env::temp_dir().join(format!("omini-command-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&cwd).unwrap();
        let skills = crate::skills::load_skill_registry(&cwd);

        register_skill_commands(&mut registry, &skills);

        let command = registry.get("skill-creator").unwrap();
        assert!(command.has_args());
        assert_eq!(command.args_description(), Some("[prompt]"));
        assert_eq!(command.kind(), CommandKind::Skill);
        assert!(
            command
                .description()
                .contains("Create or update Omini skills")
        );
        let _ = std::fs::remove_dir_all(cwd);
    }

    #[test]
    fn user_invocable_false_skills_are_not_registered_as_commands() {
        let mut registry = CommandRegistry::new();
        let cwd = std::env::temp_dir().join(format!(
            "omini-hidden-skill-command-test-{}",
            uuid::Uuid::new_v4()
        ));
        let skill_dir = cwd.join(".omini").join("skills").join("background");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            r#"---
name: background
description: Background knowledge
user-invocable: false
---
Body
"#,
        )
        .unwrap();
        let skills = crate::skills::load_skill_registry(&cwd);

        register_skill_commands(&mut registry, &skills);

        assert!(skills.get("background").is_some());
        assert!(registry.get("background").is_none());
        let _ = std::fs::remove_dir_all(cwd);
    }
}
