pub mod exit;
pub mod help;
pub mod model;
pub mod new;
pub mod rename;
pub mod sessions;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;

use crate::runtime::AgentRuntime;
use crate::types::events::{CommandResult, CommandSummary};

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
    fn name(&self) -> &'static str;
    fn aliases(&self) -> &[&'static str];
    fn description(&self) -> &'static str;
    fn args_description(&self) -> Option<&'static str> {
        None
    }
    /// 是否需要额外参数（false = 选中即执行，true = 补全后让用户输入）。
    fn has_args(&self) -> bool {
        false
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
                has_args: cmd.has_args(),
                args_description: cmd.args_description(),
            })
            .collect()
    }
}

/// 注册所有内置命令。
pub fn register_default_commands(registry: &mut CommandRegistry) {
    registry.register(Arc::new(exit::ExitCommand));
    registry.register(Arc::new(model::ModelCommand));
    registry.register(Arc::new(sessions::SessionsCommand));
    registry.register(Arc::new(new::NewCommand));
    registry.register(Arc::new(rename::RenameCommand));
    registry.register(Arc::new(help::HelpCommand));
}
