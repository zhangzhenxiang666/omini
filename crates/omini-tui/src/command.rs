use crate::types::events::{CommandKind, CommandSummary};

pub fn builtin_command_summaries() -> Vec<CommandSummary> {
    vec![
        builtin("sessions", &["resume"], "切换会话", 10, false, None),
        builtin(
            "new",
            &["clear"],
            "清空当前会话，开始新对话",
            20,
            false,
            None,
        ),
        builtin("plan", &[], "切换到 plan mode", 25, false, None),
        builtin(
            "compact",
            &[],
            "压缩当前会话上下文",
            30,
            true,
            Some("[custom summarization instructions]"),
        ),
        builtin("model", &[], "切换模型", 30, false, None),
        builtin("agents", &[], "管理 agent", 35, false, None),
        builtin(
            "effort",
            &[],
            "调整当前模型的思考程度",
            40,
            true,
            Some("<none | low | medium | high | xhigh | max>"),
        ),
        builtin("init", &[], "分析项目并生成 AGENTS.md", 50, true, None),
        builtin("rename", &[], "重命名当前会话", 60, true, Some("<name>")),
        builtin(
            "thinking",
            &[],
            "开启/关闭消息区 thinking 块展示",
            80,
            true,
            Some("[on | off]"),
        ),
        builtin("help", &["?"], "显示帮助", 900, false, None),
        builtin("exit", &["quit"], "退出程序", 1000, false, None),
    ]
}

pub fn commands_with_runtime_skills(runtime_commands: Vec<CommandSummary>) -> Vec<CommandSummary> {
    let mut commands = builtin_command_summaries();
    commands.extend(
        runtime_commands
            .into_iter()
            .filter(|command| command.kind == CommandKind::Skill),
    );
    commands.sort_by(|a, b| {
        a.sort_weight
            .cmp(&b.sort_weight)
            .then_with(|| a.name.cmp(&b.name))
    });
    commands
}

fn builtin(
    name: &str,
    aliases: &[&str],
    description: &str,
    sort_weight: i32,
    has_args: bool,
    args_description: Option<&str>,
) -> CommandSummary {
    CommandSummary {
        name: name.to_string(),
        aliases: aliases.iter().map(|alias| alias.to_string()).collect(),
        description: description.to_string(),
        sort_weight,
        kind: CommandKind::Builtin,
        has_args,
        args_description: args_description.map(str::to_string),
    }
}
