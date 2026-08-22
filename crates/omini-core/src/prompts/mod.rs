pub mod environment;
pub mod instructions;
pub mod sections;

use crate::skills::SkillSummary;
use environment::{EnvironmentContext, environment_context_section};
use instructions::{
    load_global_instructions, load_project_instructions, project_instructions_section,
};
use omini_config::Settings;
use omini_domain::events::ActiveProfile;
use omini_domain::subagents::AgentSummary;
use sections::{
    main_mode_body, max_steps_prompt, mode_header_section, plan_mode_body, subagent_section,
};
use std::path::Path;

pub use sections::language_preference_section;
pub use sections::skill_section;

/// 获取最大停止步数的预算耗尽提示词
pub fn get_max_steps_prompt() -> &'static str {
    max_steps_prompt()
}

/// 使用调用方传入的能力快照,按给定的 active profile 构建 system prompt。
/// 这是 runtime 唯一使用的入口(服务启动期初始化与 `rebuild_system_prompt`)。
pub fn build_system_prompt_with_capabilities(
    settings: &Settings,
    subagents: &[AgentSummary],
    skills: &[SkillSummary],
    active_profile: ActiveProfile,
) -> String {
    match active_profile {
        ActiveProfile::Main | ActiveProfile::Auto => build_main_body(settings, subagents, skills),
        ActiveProfile::Plan => build_plan_body(settings, subagents, skills),
    }
}

fn build_main_body(
    settings: &Settings,
    subagents: &[AgentSummary],
    skills: &[SkillSummary],
) -> String {
    let mut prompt = String::new();
    prompt.push_str(&mode_header_section(ActiveProfile::Main));
    prompt.push('\n');
    prompt.push_str(main_mode_body());
    prompt.push('\n');
    if let Some(section) = language_preference_section(settings) {
        prompt.push_str(&section);
        prompt.push('\n');
    }
    prompt.push_str(&subagent_section(subagents, ActiveProfile::Main));
    prompt.push('\n');
    if let Some(section) = skill_section(skills) {
        prompt.push_str(&section);
        prompt.push('\n');
    }
    prompt.push_str(&project_context_prompt(&settings.cwd));
    prompt
}

fn build_plan_body(
    settings: &Settings,
    subagents: &[AgentSummary],
    skills: &[SkillSummary],
) -> String {
    let mut prompt = String::new();
    prompt.push_str(&mode_header_section(ActiveProfile::Plan));
    prompt.push('\n');
    prompt.push_str(plan_mode_body());
    prompt.push('\n');
    if let Some(section) = language_preference_section(settings) {
        prompt.push_str(&section);
        prompt.push('\n');
    }
    prompt.push_str(&subagent_section(subagents, ActiveProfile::Plan));
    prompt.push('\n');
    if let Some(section) = skill_section(skills) {
        prompt.push_str(&section);
        prompt.push('\n');
    }
    prompt.push_str(&project_context_prompt(&settings.cwd));
    prompt
}

pub fn project_context_prompt(cwd: &Path) -> String {
    let env = EnvironmentContext::detect(cwd);
    let global_instructions = load_global_instructions();
    let project_instructions = load_project_instructions(cwd);

    let mut prompt = String::new();
    prompt.push_str(&project_instructions_section(
        global_instructions.as_ref(),
        project_instructions.as_ref(),
    ));
    prompt.push('\n');
    prompt.push_str(&environment_context_section(&env));
    prompt
}
