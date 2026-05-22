mod environment;
mod instructions;
mod sections;

use crate::skills::SkillSummary;
use crate::subagents::AgentSummary;
use crate::types::config::Settings;
use crate::types::events::ActiveProfile;
use environment::{EnvironmentContext, environment_context_section};
use instructions::{
    load_global_instructions, load_project_instructions, project_instructions_section,
};
use sections::{
    agent_identity_section, core_behavior_section, plan_mode_instructions_section,
    subagent_section, tool_instructions_section,
};
use std::path::Path;

pub use sections::language_preference_section;
pub(crate) use sections::skill_section;

/// 构建当前请求的完整系统提示词。
pub fn build_system_prompt(settings: &Settings) -> String {
    build_system_prompt_for_profile(settings, ActiveProfile::Main)
}

/// 按当前请求和激活模式构建完整系统提示词。
pub fn build_system_prompt_for_profile(
    settings: &Settings,
    active_profile: ActiveProfile,
) -> String {
    let subagents = crate::subagents::load_agent_summaries(&settings.cwd);
    let skills = crate::skills::load_skill_summaries(&settings.cwd);
    build_system_prompt_with_capabilities(settings, &subagents, &skills, active_profile)
}

/// 使用运行时提供的能力快照构建完整系统提示词。
pub fn build_system_prompt_with_subagents(
    settings: &Settings,
    subagents: &[AgentSummary],
) -> String {
    build_system_prompt_with_capabilities(settings, subagents, &[], ActiveProfile::Main)
}

/// 使用运行时提供的多类能力快照构建完整系统提示词。
pub(crate) fn build_system_prompt_with_capabilities(
    settings: &Settings,
    subagents: &[AgentSummary],
    skills: &[SkillSummary],
    active_profile: ActiveProfile,
) -> String {
    let mut prompt = String::new();
    prompt.push_str("You are Omini, a coding agent running in the user's local terminal.\n\n");
    prompt.push_str(&agent_identity_section());
    prompt.push('\n');
    prompt.push_str(&core_behavior_section());
    prompt.push('\n');
    prompt.push_str(&tool_instructions_section());
    prompt.push('\n');
    if let Some(section) = language_preference_section(settings) {
        prompt.push_str(&section);
        prompt.push('\n');
    }
    prompt.push_str(&subagent_section(subagents, active_profile));
    prompt.push('\n');
    if let Some(section) = skill_section(skills) {
        prompt.push_str(&section);
        prompt.push('\n');
    }
    prompt.push_str(&project_context_prompt_for_profile(
        &settings.cwd,
        active_profile,
    ));
    prompt
}

pub fn project_context_prompt(cwd: &Path) -> String {
    project_context_prompt_for_profile(cwd, ActiveProfile::Main)
}

fn project_context_prompt_for_profile(cwd: &Path, active_profile: ActiveProfile) -> String {
    let env = EnvironmentContext::detect(cwd);
    let global_instructions = load_global_instructions();
    let project_instructions = load_project_instructions(cwd);

    let mut prompt = String::new();
    prompt.push_str(&project_instructions_section(
        global_instructions.as_ref(),
        project_instructions.as_ref(),
    ));
    prompt.push('\n');
    if active_profile == ActiveProfile::Plan {
        prompt.push_str(&plan_mode_instructions_section());
        prompt.push('\n');
    }
    prompt.push_str(&environment_context_section(&env));
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::config::{ProviderType, Settings};
    use std::collections::HashMap;
    use std::fs;
    use std::path::PathBuf;
    use uuid::Uuid;

    fn test_settings(language: Option<&str>) -> Settings {
        Settings {
            api_key: "test-key".to_string(),
            base_url: "https://openai.example".to_string(),
            model: "gpt-test".to_string(),
            endpoint: ProviderType::OpenAI,
            providers: HashMap::new(),
            active_provider: "openai".to_string(),
            system_prompt: None,
            language: language.map(str::to_string),
            max_turns: None,
            cwd: std::env::temp_dir(),
            thinking_effort: None,
            permissions: None,
        }
    }

    fn temp_prompt_dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("omini-prompt-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create temp prompt dir");
        path
    }

    #[test]
    fn language_preference_section_trims_for_prompt_only() {
        let settings = test_settings(Some("  en  "));

        let section = language_preference_section(&settings).unwrap();

        assert!(section.contains("`en`"));
        assert!(!section.contains("`  en  `"));
    }

    #[test]
    fn language_preference_section_omits_blank_values() {
        let settings = test_settings(Some("   "));

        assert!(language_preference_section(&settings).is_none());
    }

    #[test]
    fn main_system_prompt_includes_language_preference_when_configured() {
        let settings = test_settings(Some("简体中文"));

        let prompt = build_system_prompt_with_subagents(&settings, &[]);

        assert!(prompt.contains("<language_preference>"));
        assert!(prompt.contains("`简体中文`"));
    }

    #[test]
    fn main_system_prompt_omits_language_preference_when_unset() {
        let settings = test_settings(None);

        let prompt = build_system_prompt_with_subagents(&settings, &[]);

        assert!(!prompt.contains("<language_preference>"));
    }

    #[test]
    fn skill_section_includes_descriptions_without_paths() {
        let settings = test_settings(None);
        let skill_dir = PathBuf::from("/tmp/omini-skill-test/writer");
        let skills = vec![SkillSummary {
            name: "writer".to_string(),
            description: "Write carefully".to_string(),
            directory: skill_dir.clone(),
        }];

        let prompt =
            build_system_prompt_with_capabilities(&settings, &[], &skills, ActiveProfile::Main);

        assert!(prompt.contains("<skill_instructions>"));
        assert!(prompt.contains("- `writer`: Write carefully"));
        assert!(prompt.contains("<source>slash_command</source>"));
        assert!(prompt.contains("do not call the `skill` tool for the same skill again"));
        assert!(!prompt.contains(skill_dir.to_str().unwrap()));
    }

    #[test]
    fn tool_instructions_do_not_hardcode_python_runner_policy() {
        let section = tool_instructions_section();

        assert!(!section.contains("uv run"));
        assert!(!section.contains("python3"));
    }

    #[test]
    fn main_system_prompt_does_not_add_mode_overlay() {
        let settings = test_settings(None);

        let prompt = build_system_prompt_for_profile(&settings, ActiveProfile::Main);

        assert!(!prompt.contains("<active_mode"));
        assert!(!prompt.contains("<plan_mode_instructions>"));
        assert!(!prompt.contains("You are in main mode"));
    }

    #[test]
    fn plan_mode_instructions_follow_project_instructions() {
        let settings = test_settings(None);

        let prompt = build_system_prompt_for_profile(&settings, ActiveProfile::Plan);
        let project_idx = prompt
            .find("<project_instructions>")
            .expect("prompt should include project instructions");
        let plan_idx = prompt
            .find("<plan_mode_instructions>")
            .expect("prompt should include plan mode instructions");
        let environment_idx = prompt
            .find("<environment_context>")
            .expect("prompt should include environment context");

        assert!(!prompt.starts_with("<plan_mode_instructions>"));
        assert!(project_idx < plan_idx);
        assert!(plan_idx < environment_idx);
        assert!(prompt.contains("# Plan Mode (Conversational)"));
        assert!(prompt.contains("You are in **Plan Mode**"));
        assert!(prompt.contains("final response must be exactly one `<proposed_plan>` block"));
        assert!(prompt.contains("must be exactly one valid `<proposed_plan>` block"));
        assert!(prompt.contains("Never present the final plan as plain prose"));
        assert!(prompt.contains("<proposed_plan>"));
        assert!(prompt.contains("Using subagents for read-only exploration"));
        assert!(!prompt.contains("<active_mode"));
        assert!(!prompt.contains("submit_plan"));
    }

    #[test]
    fn plan_mode_instructions_include_lightweight_brainstorming_flow() {
        let section = plan_mode_instructions_section();

        assert!(section.contains("Ask one question at a time"));
        assert!(section.contains("Offer 2-3 viable approaches"));
        assert!(section.contains("explain the tradeoffs"));
        assert!(section.contains("recommend one"));
        assert!(section.contains("design checkpoint"));
        assert!(section.contains("Pre-Final Self-Review"));
        assert!(section.contains("final response must be exactly one `<proposed_plan>` block"));
        assert!(section.contains("must be exactly one valid `<proposed_plan>` block"));
        assert!(section.contains("medium-executable structure"));
        assert!(section.contains("key files, interfaces, data flow, and tests"));
        assert!(section.contains("do not turn the plan into a step-by-step implementation manual"));
    }

    #[test]
    fn plan_mode_instructions_reference_todo_write_tool() {
        let section = plan_mode_instructions_section();

        assert!(section.contains("`todo_write` tool"));
        assert!(!section.contains("`todo` tool"));
    }

    #[test]
    fn plan_mode_delegation_instructions_avoid_worker_execution_guidance() {
        let settings = test_settings(None);
        let agents = vec![AgentSummary {
            name: "worker".to_string(),
            description: "Implementation agent for focused coding tasks.".to_string(),
        }];

        let plan_prompt =
            build_system_prompt_with_capabilities(&settings, &agents, &[], ActiveProfile::Plan);
        let main_prompt =
            build_system_prompt_with_capabilities(&settings, &agents, &[], ActiveProfile::Main);

        assert!(main_prompt.contains("focused implementation work"));
        assert!(!plan_prompt.contains("focused implementation work"));
        assert!(plan_prompt.contains("use subagents only for non-mutating exploration"));
    }

    #[test]
    fn environment_context_includes_git_branch_when_available() {
        let dir = temp_prompt_dir();
        let git_dir = dir.join(".git");
        fs::create_dir_all(&git_dir).expect("create .git dir");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/feature/search\n").expect("write HEAD");

        let env = EnvironmentContext::detect(&dir);
        let section = environment_context_section(&env);

        assert!(section.contains("- Git repository: `true`"));
        assert!(section.contains("- Git branch: `feature/search`"));
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn environment_context_reads_worktree_gitdir_file() {
        let dir = temp_prompt_dir();
        let git_dir = dir.join(".git-worktree");
        fs::create_dir_all(&git_dir).expect("create worktree git dir");
        fs::write(dir.join(".git"), "gitdir: .git-worktree\n").expect("write .git file");
        fs::write(git_dir.join("HEAD"), "ref: refs/heads/worktree-branch\n").expect("write HEAD");

        let env = EnvironmentContext::detect(&dir);

        assert_eq!(env.git_branch.as_deref(), Some("worktree-branch"));
        let _ = fs::remove_dir_all(dir);
    }
}
