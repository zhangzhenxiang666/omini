//! 本地 Omini daemon 的核心实现。
//!
//! 对 `omini-server` 的公开边界是 [`execution`] 模块：`AgentInstance` 表示执行实例，
//! `AgentHandle` 提供可克隆的命令句柄，`AgentEvents` 是独占输出接收端，`AgentHost`
//! 是 core 依赖宿主的窄接口。`pub(crate) mod` 用于建立 crate 内部的子系统边界；
//! 子系统中的成员使用 `pub`，除非有意设置更窄的子模块边界。

pub(crate) mod agent;
pub(crate) mod engine;
pub(crate) mod error;
pub mod execution;
pub(crate) mod frontmatter;
pub(crate) mod mcp;
pub(crate) mod prompts;
pub(crate) mod proposed_plan;
pub(crate) mod runtime;
pub(crate) mod skills;
pub(crate) mod tasks;
pub(crate) mod title_generation;
pub(crate) mod tools;
pub(crate) mod types;
pub(crate) mod util;

#[cfg(test)]
pub(crate) mod test_support;

use omini_config::Settings;
use omini_domain::subagents as subagent_types;
use omini_runtime_contract::project as project_types;
use omini_runtime_contract::thread as thread_types;
use std::path::{Path, PathBuf};

pub use crate::error::CoreError;
pub use crate::title_generation::{TitleGenError, generate_thread_title};

pub fn compacted_plan_context(plan_content: &str) -> String {
    crate::runtime::compacted_plan_context(plan_content)
}

pub fn project_agents_snapshot(settings: &Settings) -> thread_types::AgentsSnapshot {
    let records = project_agent_records(&settings.cwd);
    let models = models_snapshot_from_settings(settings);
    thread_types::AgentsSnapshot {
        records,
        providers: models.providers,
        current_provider: models.current_provider,
        current_model: models.current_model,
    }
}

pub fn project_skill_summaries(cwd: &Path) -> Vec<thread_types::SkillSummarySnapshot> {
    let registry = crate::skills::load_skill_registry(cwd);
    user_invocable_skill_summaries(&registry)
}

pub fn save_project_agent(
    cwd: &Path,
    command: project_types::SaveProjectAgentCommand,
) -> Result<project_types::AgentManagementUpdate, CoreError> {
    if command.source_kind == subagent_types::AgentSourceKind::BuiltIn {
        return Err(CoreError::new("内置 agent 不能写入"));
    }
    let original_path = command
        .original_agent_id
        .as_deref()
        .map(|agent_id| resolve_editable_agent_path(cwd, agent_id))
        .transpose()?;
    if crate::agent::agent_name_exists(cwd, &command.draft.name, original_path.as_deref()) {
        return Err(CoreError::new(format!(
            "agent '{}' 已存在",
            command.draft.name
        )));
    }
    let written_path = crate::agent::write_agent_file(cwd, command.source_kind, &command.draft)
        .map_err(CoreError::new)?;
    if let Some(path) = original_path
        && path != written_path
    {
        crate::agent::delete_agent_file(&path).map_err(CoreError::new)?;
    }
    Ok(project_types::AgentManagementUpdate {
        records: project_agent_records(cwd),
    })
}

pub fn delete_project_agent(
    cwd: &Path,
    command: project_types::DeleteProjectAgentCommand,
) -> Result<project_types::AgentManagementUpdate, CoreError> {
    let path = resolve_editable_agent_path(cwd, &command.agent_id)?;
    crate::agent::delete_agent_file(&path).map_err(CoreError::new)?;
    Ok(project_types::AgentManagementUpdate {
        records: project_agent_records(cwd),
    })
}

pub async fn generate_project_agent_draft(
    settings: &Settings,
    description: &str,
) -> Result<subagent_types::GeneratedAgentDraft, CoreError> {
    let mut parse_error = None;
    for attempt in 0..2 {
        match crate::agent::generate_agent_draft_checked_from_settings(settings, description).await
        {
            Ok(draft) => return Ok(draft),
            Err(crate::agent::GenerateAgentDraftError::Parse(message)) if attempt == 0 => {
                parse_error = Some(message);
            }
            Err(error) => return Err(CoreError::new(error.to_string())),
        }
    }
    Err(CoreError::new(
        parse_error.unwrap_or_else(|| "生成 agent 失败".to_string()),
    ))
}

fn project_agent_records(cwd: &Path) -> Vec<subagent_types::AgentRecord> {
    crate::agent::list_agent_records(cwd)
}

fn resolve_editable_agent_path(cwd: &Path, agent_id: &str) -> Result<PathBuf, CoreError> {
    project_agent_records(cwd)
        .into_iter()
        .find(|record| record.editable && agent_record_id(record) == agent_id)
        .and_then(|record| record.path)
        .ok_or_else(|| CoreError::new(format!("agent '{agent_id}' 不存在或不可编辑")))
}

fn agent_record_id(record: &subagent_types::AgentRecord) -> String {
    record
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| record.name.clone())
}

pub(crate) fn user_invocable_skill_summaries(
    registry: &crate::skills::SkillRegistry,
) -> Vec<thread_types::SkillSummarySnapshot> {
    let mut skills = registry
        .skills()
        .filter(|skill| skill.user_invocable)
        .map(|skill| thread_types::SkillSummarySnapshot {
            name: skill.name.clone(),
            description: skill.description.clone(),
            short_description: skill.short_description.clone(),
            argument_hint: skill.argument_hint.clone(),
        })
        .collect::<Vec<_>>();
    skills.sort_by(|a, b| a.name.cmp(&b.name));
    skills
}

pub(crate) fn runtime_skill_snapshots(
    capabilities: &crate::runtime::CapabilityStore,
) -> Vec<thread_types::RuntimeSkillSnapshot> {
    let skill_registry = capabilities.skill_registry();
    let mut skills = skill_registry
        .skills()
        .map(|skill| thread_types::RuntimeSkillSnapshot {
            name: skill.name.clone(),
            description: skill.description.clone(),
            short_description: skill.short_description.clone(),
            source_kind: runtime_skill_source_kind(skill.source_kind()),
            directory: skill.directory.clone(),
            status: thread_types::RuntimeCapabilityStatus::Available,
            disable_model_invocation: skill.disable_model_invocation,
            user_invocable: skill.user_invocable,
        })
        .collect::<Vec<_>>();
    skills.sort_by(|left, right| {
        runtime_skill_source_sort(left.source_kind)
            .cmp(&runtime_skill_source_sort(right.source_kind))
            .then_with(|| left.name.cmp(&right.name))
    });
    skills
}

fn runtime_skill_source_kind(
    source_kind: crate::skills::SkillSourceKind,
) -> thread_types::RuntimeSkillSourceKind {
    match source_kind {
        crate::skills::SkillSourceKind::BuiltIn => thread_types::RuntimeSkillSourceKind::BuiltIn,
        crate::skills::SkillSourceKind::Project => thread_types::RuntimeSkillSourceKind::Project,
        crate::skills::SkillSourceKind::User => thread_types::RuntimeSkillSourceKind::User,
    }
}

fn runtime_skill_source_sort(source_kind: thread_types::RuntimeSkillSourceKind) -> u8 {
    match source_kind {
        thread_types::RuntimeSkillSourceKind::BuiltIn => 0,
        thread_types::RuntimeSkillSourceKind::Project => 1,
        thread_types::RuntimeSkillSourceKind::User => 2,
    }
}

fn models_snapshot_from_settings(settings: &Settings) -> thread_types::ModelsSnapshot {
    let mut providers = settings.resolved_config().catalog();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    let model = settings.active_model();
    thread_types::ModelsSnapshot {
        providers,
        current_provider: model.provider_id.clone(),
        current_model: model.model_id.clone(),
    }
}
