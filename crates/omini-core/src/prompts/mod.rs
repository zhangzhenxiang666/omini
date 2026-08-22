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

#[cfg(test)]
mod tests {
    use super::*;
    use omini_domain::subagents::AgentSummary;

    #[test]
    fn language_preference_blank_or_padded_values_have_stable_projection() {
        let temp = crate::test_support::TestTempDir::new("prompts-language");
        let mut settings = crate::test_support::settings(temp.path(), false);
        settings.language = Some("  简体中文  ".into());
        assert_eq!(
            language_preference_section(&settings),
            Some(
                "<language_preference>\n- Use `简体中文` for user-facing responses unless the latest user request asks for another language.\n</language_preference>"
                    .into()
            )
        );
        settings.language = Some(" \t ".into());
        assert_eq!(language_preference_section(&settings), None);
    }

    #[test]
    fn main_prompt_projects_subagents_after_mode_header() {
        let temp = crate::test_support::TestTempDir::new("prompts-main");
        let settings = crate::test_support::settings(temp.path(), false);
        let agents = vec![AgentSummary {
            name: "reviewer".into(),
            description: "Review focused changes.".into(),
            short_description: Some("测试助手".into()),
            location: "project".into(),
        }];

        let prompt =
            build_system_prompt_with_capabilities(&settings, &agents, &[], ActiveProfile::Main);
        assert!(prompt.starts_with("<active_mode>\nCollaboration Mode: Main"));
        assert!(prompt.contains("<available_agents>\n  <agent>\n    <name>reviewer</name>"));
        assert!(!prompt.contains("<location>project</location>"));
        assert!(
            prompt.find("</active_mode>").expect("mode block")
                < prompt.find("<available_agents>").expect("agent block")
        );
        assert!(!prompt.contains("<plan_mode_instructions>"));
    }

    #[test]
    fn profile_prompts_keep_main_auto_equal_and_plan_execution_free() {
        let temp = crate::test_support::TestTempDir::new("prompts-profiles");
        let settings = crate::test_support::settings(temp.path(), false);
        let main = build_system_prompt_with_capabilities(&settings, &[], &[], ActiveProfile::Main);
        let auto = build_system_prompt_with_capabilities(&settings, &[], &[], ActiveProfile::Auto);
        let plan = build_system_prompt_with_capabilities(&settings, &[], &[], ActiveProfile::Plan);

        assert_eq!(main, auto);
        assert!(main.starts_with("<active_mode>\nCollaboration Mode: Main"));
        assert!(main.contains("## Code Editing"));
        assert!(plan.starts_with("<active_mode>\nCollaboration Mode: Plan"));
        assert!(plan.contains("<HARD-GATE>"));
        assert!(plan.contains("exactly one `<proposed_plan>` block"));
        assert!(!plan.contains("## Code Editing"));
        assert!(!plan.contains("## Git Safety"));
    }
}
