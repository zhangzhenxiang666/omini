use crate::command::Command;
use crate::db;
use crate::runtime::AgentRuntime;
use crate::types::config::ThinkingEffort;
use crate::types::events::{CommandEffect, CommandResult, RuntimeToUiEvent};
use async_trait::async_trait;

pub struct EffortCommand;

#[async_trait]
impl Command for EffortCommand {
    fn name(&self) -> &str {
        "effort"
    }

    fn aliases(&self) -> &[&'static str] {
        &[]
    }

    fn description(&self) -> &str {
        "调整当前模型的思考程度"
    }

    fn sort_weight(&self) -> i32 {
        40
    }

    fn has_args(&self) -> bool {
        true
    }

    fn args_description(&self) -> Option<&'static str> {
        Some("<none | low | medium | high>")
    }

    async fn execute(&self, runtime: &mut AgentRuntime, args: &str) -> CommandResult {
        let mut parts = args.split_whitespace();
        let Some(value) = parts.next() else {
            return CommandResult::Error(
                "请提供思考程度，用法: /effort none | low | medium | high".to_string(),
            );
        };
        if parts.next().is_some() {
            return CommandResult::Error(
                "参数过多，用法: /effort none | low | medium | high".to_string(),
            );
        }

        let effort = match value.parse::<ThinkingEffort>() {
            Ok(effort) => effort,
            Err(()) => {
                return CommandResult::Error(format!(
                    "无效的思考程度 '{value}'，可用值: none | low | medium | high"
                ));
            }
        };
        if effort != ThinkingEffort::None {
            let supports_thinking = runtime
                .settings
                .providers
                .get(&runtime.settings.active_provider)
                .and_then(|profile| {
                    profile
                        .models
                        .iter()
                        .find(|model| model.id == runtime.settings.model)
                })
                .is_some_and(|model| model.thinking);
            if !supports_thinking {
                return CommandResult::Error(format!(
                    "当前模型 '{}' 不支持思考模式",
                    runtime.settings.model
                ));
            }
        }

        let stored_effort = effort.to_string();

        if let Some(session_id) = &runtime.session_id {
            if let Err(e) = db::global_db()
                .update_session_thinking_effort(session_id, Some(&stored_effort))
                .await
            {
                return CommandResult::Error(format!("更新思考程度失败: {e}"));
            }
        } else {
            let mut state = match runtime.project.load_state() {
                Ok(state) => state,
                Err(e) => return CommandResult::Error(format!("读取项目状态失败: {e}")),
            };
            state.thinking_effort = Some(effort);
            if let Err(e) = runtime.project.save_state(&state) {
                return CommandResult::Error(format!("保存项目状态失败: {e}"));
            }
        }

        runtime.settings.thinking_effort = Some(effort);
        CommandResult::Ok(vec![
            CommandEffect::emit(RuntimeToUiEvent::ModelChanged {
                provider: runtime.settings.active_provider.clone(),
                model: runtime.settings.model.clone(),
                thinking_effort: runtime.settings.thinking_effort,
                context_window: runtime
                    .settings
                    .providers
                    .get(&runtime.settings.active_provider)
                    .and_then(|provider| {
                        provider
                            .models
                            .iter()
                            .find(|model| model.id == runtime.settings.model)
                            .map(|model| model.limit)
                    }),
            }),
            CommandEffect::Notice(format!("思考程度已设置为 {effort}")),
        ])
    }
}
