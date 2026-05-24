use crate::command::Command;
use crate::config::project::ProjectDir;
use crate::runtime::AgentRuntime;
use crate::types::events::{CommandEffect, CommandResult, RuntimeToUiEvent};
use async_trait::async_trait;

pub struct ThinkingCommand;

#[async_trait]
impl Command for ThinkingCommand {
    fn name(&self) -> &str {
        "thinking"
    }

    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    fn description(&self) -> &str {
        "开启/关闭消息区 thinking 块展示"
    }

    fn sort_weight(&self) -> i32 {
        80
    }

    fn has_args(&self) -> bool {
        true
    }

    fn args_description(&self) -> Option<&'static str> {
        Some("[on | off]")
    }

    async fn execute(&self, runtime: &mut AgentRuntime, args: &str) -> CommandResult {
        match apply_thinking_display(&runtime.project, args) {
            Ok(show) => CommandResult::Ok(vec![
                CommandEffect::emit(RuntimeToUiEvent::ThinkingDisplayChanged { show }),
                CommandEffect::Notice(thinking_display_notice(show)),
            ]),
            Err(error) => CommandResult::Error(error),
        }
    }
}

pub(crate) fn apply_thinking_display(project: &ProjectDir, args: &str) -> Result<bool, String> {
    let mut state = project
        .load_state()
        .map_err(|e| format!("读取项目状态失败: {e}"))?;
    let show = parse_thinking_display(args, state.show_thinking_blocks)?;
    state.show_thinking_blocks = show;
    project
        .save_state(&state)
        .map_err(|e| format!("保存项目状态失败: {e}"))?;
    Ok(show)
}

pub(crate) fn thinking_display_notice(show: bool) -> String {
    if show {
        "thinking 块展示已开启".to_string()
    } else {
        "thinking 块展示已关闭".to_string()
    }
}

fn parse_thinking_display(args: &str, current: bool) -> Result<bool, String> {
    let mut parts = args.split_whitespace();
    let Some(value) = parts.next() else {
        return Ok(!current);
    };
    if parts.next().is_some() {
        return Err("参数过多，用法: /thinking [on | off]".to_string());
    }

    match value.to_ascii_lowercase().as_str() {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err(format!(
            "无效的 thinking 展示设置 '{value}'，可用值: on | off"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::project::ProjectsDir;
    use crate::config::settings::{ModelEntry, ProviderConfig, UserConfig};
    use crate::types::config::ProviderType;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_root(test_name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("omini-{test_name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("failed to create temp test root");
        dir
    }

    fn test_user_config() -> UserConfig {
        let mut models = HashMap::new();
        models.insert(
            "gpt-test".to_string(),
            ModelEntry {
                name: None,
                limit: Some(256000),
                thinking: Some(true),
            },
        );

        let mut providers = HashMap::new();
        providers.insert(
            "openai".to_string(),
            ProviderConfig {
                name: Some("OpenAI".to_string()),
                endpoint: ProviderType::OpenAI,
                base_url: "https://openai.example".to_string(),
                api_key: "test-key".to_string(),
                models: Some(models),
            },
        );

        UserConfig {
            providers,
            language: None,
            permissions: None,
            compact: None,
        }
    }

    #[test]
    fn parse_toggles_without_args() {
        assert!(!parse_thinking_display("", true).unwrap());
        assert!(parse_thinking_display("   ", false).unwrap());
    }

    #[test]
    fn parse_accepts_explicit_values() {
        assert!(parse_thinking_display("on", false).unwrap());
        assert!(!parse_thinking_display("off", true).unwrap());
        assert!(parse_thinking_display("ON", false).unwrap());
    }

    #[test]
    fn parse_rejects_invalid_values() {
        assert!(parse_thinking_display("maybe", true).is_err());
        assert!(parse_thinking_display("on extra", true).is_err());
    }

    #[test]
    fn apply_persists_project_display_preference() {
        let root = unique_temp_root("thinking-display");
        let cwd = root.join("workspace");
        std::fs::create_dir_all(&cwd).expect("failed to create cwd");
        let config = test_user_config();
        let project = ProjectsDir::new(&root)
            .for_cwd(&cwd, &config)
            .expect("failed to create project dir");

        assert!(project.load_state().unwrap().show_thinking_blocks);

        let show = apply_thinking_display(&project, "off").unwrap();

        assert!(!show);
        assert!(!project.load_state().unwrap().show_thinking_blocks);

        let show = apply_thinking_display(&project, "").unwrap();

        assert!(show);
        assert!(project.load_state().unwrap().show_thinking_blocks);
    }
}
