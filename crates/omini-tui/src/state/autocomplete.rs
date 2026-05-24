use crate::types::events::CommandSummary;

/// 命令自动补全状态。
#[derive(Debug, Clone, Default)]
pub struct CommandAutocomplete {
    /// 是否显示下拉列表
    pub visible: bool,
    /// Runtime 推送的全量命令列表
    pub all_commands: Vec<CommandSummary>,
    /// 经过当前输入过滤后的子集
    pub filtered: Vec<CommandSummary>,
    /// 当前选中的索引
    pub selected: usize,
}

impl CommandAutocomplete {
    /// 根据当前输入更新过滤后的命令列表。
    pub fn update(&mut self, input: &str) {
        if !input.starts_with('/') {
            self.visible = false;
            return;
        }
        if input[1..].contains(char::is_whitespace) {
            self.visible = false;
            return;
        }
        self.visible = true;

        let partial = input[1..].to_lowercase();
        self.filtered = self
            .all_commands
            .iter()
            .filter(|cmd| {
                cmd.name.to_lowercase().contains(&partial)
                    || cmd
                        .aliases
                        .iter()
                        .any(|a| a.to_lowercase().contains(&partial))
            })
            .cloned()
            .collect();
        self.filtered.sort_by(|a, b| {
            a.sort_weight
                .cmp(&b.sort_weight)
                .then_with(|| a.name.cmp(&b.name))
        });

        let max = self.filtered.len().saturating_sub(1);
        self.selected = self.selected.min(max);
    }

    /// 选中当前项（Enter 时调用）。
    pub fn selected_command(&self) -> Option<&CommandSummary> {
        if self.filtered.is_empty() {
            return None;
        }
        self.filtered.get(self.selected)
    }

    pub fn select_next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.filtered.len();
    }

    pub fn select_prev(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::events::{CommandKind, CommandSummary};

    fn command(name: &str) -> CommandSummary {
        CommandSummary {
            name: name.to_string(),
            aliases: Vec::new(),
            description: String::new(),
            sort_weight: 0,
            kind: CommandKind::Builtin,
            has_args: true,
            args_description: Some("[prompt]"),
        }
    }

    #[test]
    fn command_autocomplete_hides_in_argument_region() {
        let mut autocomplete = CommandAutocomplete {
            all_commands: vec![command("commit-message")],
            ..CommandAutocomplete::default()
        };

        autocomplete.update("/commit");
        assert!(autocomplete.visible);

        autocomplete.update("/commit-message @src");
        assert!(!autocomplete.visible);
    }
}
