mod support;

use omini_core::prompts::{
    build_system_prompt_for_profile, build_system_prompt_with_subagents,
    language_preference_section,
};
use omini_domain::events::ActiveProfile;
use omini_domain::subagents::AgentSummary;

#[test]
fn language_preference_blank_or_padded_values_have_stable_projection() {
    let temp = support::TestTempDir::new("prompts-language");
    let mut settings = support::settings(temp.path(), false);
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
    let temp = support::TestTempDir::new("prompts-main");
    let settings = support::settings(temp.path(), false);
    let agents = vec![AgentSummary {
        name: "reviewer".into(),
        description: "Review focused changes.".into(),
        short_description: Some("审查".into()),
        location: "project".into(),
    }];

    let prompt = build_system_prompt_with_subagents(&settings, &agents);
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
    let temp = support::TestTempDir::new("prompts-profiles");
    let settings = support::settings(temp.path(), false);
    let main = build_system_prompt_for_profile(&settings, ActiveProfile::Main);
    let auto = build_system_prompt_for_profile(&settings, ActiveProfile::Auto);
    let plan = build_system_prompt_for_profile(&settings, ActiveProfile::Plan);

    assert_eq!(main, auto);
    assert!(main.starts_with("<active_mode>\nCollaboration Mode: Main"));
    assert!(main.contains("## Code Editing"));
    assert!(plan.starts_with("<active_mode>\nCollaboration Mode: Plan"));
    assert!(plan.contains("<HARD-GATE>"));
    assert!(plan.contains("exactly one `<proposed_plan>` block"));
    assert!(!plan.contains("## Code Editing"));
    assert!(!plan.contains("## Git Safety"));
}
