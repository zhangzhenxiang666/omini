use super::*;
use crate::types::events::{
    PermissionPreview, RuntimeToUiEvent, SubagentStartedEvent, ToolPauseKind, ToolPauseRequest,
};
use chrono::Utc;
use omini_domain::display::MentionKind;
use omini_domain::message::ToolResultBlock;
use omini_domain::subagents::{AgentRecord, AgentSourceKind, AgentSummary};
use omini_protocol as protocol;
use std::time::Duration;
use tokio::time::Instant;

fn state_with_mention(cursor_char: usize) -> UiState {
    let mut state = UiState::new();
    state.input = "see @src now".to_string();
    state.cursor_char = cursor_char;
    state.input_mentions.push(InputMention {
        start_char: 4,
        end_char: 9,
        kind: MentionKind::Directory,
        label: "src".to_string(),
        target: "src".to_string(),
        description: "directory".to_string(),
    });
    state
}

fn long_paste_text() -> String {
    "x".repeat(PASTE_MARKER_THRESHOLD_CHARS + 1)
}

fn temp_image_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("omini_image_input_test_{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, b"image").unwrap();
    path
}

fn start_subagent(state: &mut UiState) {
    state.apply_event(RuntimeToUiEvent::SubagentStarted(SubagentStartedEvent {
        session_id: "sub_1".to_string(),
        parent_session_id: "parent".to_string(),
        spawn_tool_use_id: "tool_1".to_string(),
        agent_label: "explorer".to_string(),
    }));
}

fn permission_pause(tool_use_id: &str) -> ToolPauseRequest {
    ToolPauseRequest {
        tool_use_id: tool_use_id.to_string(),
        preview_tool_use_id: None,
        tool_name: "bash".to_string(),
        permission_source: None,
        source_session_id: None,
        source_agent_label: None,
        kind: ToolPauseKind::Permission(PermissionPreview::Custom {
            tool_name: "bash".to_string(),
            payload: serde_json::Map::new(),
        }),
    }
}

fn query_runtime_status(
    state: protocol::SessionRuntimeState,
    elapsed_ms: u64,
    pending_pause_ids: &[&str],
) -> protocol::SessionRuntimeStatus {
    protocol::SessionRuntimeStatus {
        session_id: "session_1".to_string(),
        state,
        active_profile: protocol::ActiveProfile::Main,
        loaded: true,
        controller_id: Some("client_1".to_string()),
        connected_client_count: 1,
        activity: Some(protocol::SessionRuntimeActivity {
            kind: protocol::SessionRuntimeActivityKind::Query,
            started_at: Utc::now(),
            elapsed_ms,
        }),
        pending_pauses: pending_pause_ids
            .iter()
            .map(|tool_use_id| protocol::SessionRuntimePendingPause {
                tool_use_id: (*tool_use_id).to_string(),
                tool_name: "bash".to_string(),
                kind: protocol::ToolPauseEventKind::Permission,
                source_session_id: None,
                source_agent_label: None,
            })
            .collect(),
        pending_plan_approval: None,
        active_tools: Vec::new(),
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        subagent_sessions: Vec::new(),
        git_branch: None,
    }
}

fn compact_runtime_status(elapsed_ms: u64) -> protocol::SessionRuntimeStatus {
    protocol::SessionRuntimeStatus {
        session_id: "session_1".to_string(),
        state: protocol::SessionRuntimeState::Compacting,
        active_profile: protocol::ActiveProfile::Main,
        loaded: true,
        controller_id: Some("client_1".to_string()),
        connected_client_count: 1,
        activity: Some(protocol::SessionRuntimeActivity {
            kind: protocol::SessionRuntimeActivityKind::Compact,
            started_at: Utc::now(),
            elapsed_ms,
        }),
        pending_pauses: Vec::new(),
        pending_plan_approval: None,
        active_tools: Vec::new(),
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        subagent_sessions: Vec::new(),
        git_branch: None,
    }
}

fn pending_plan_runtime_status(plan_id: &str) -> protocol::SessionRuntimeStatus {
    protocol::SessionRuntimeStatus {
        session_id: "session_1".to_string(),
        state: protocol::SessionRuntimeState::Idle,
        active_profile: protocol::ActiveProfile::Main,
        loaded: true,
        controller_id: Some("client_1".to_string()),
        connected_client_count: 1,
        activity: None,
        pending_pauses: Vec::new(),
        pending_plan_approval: Some(protocol::PlanSubmittedEvent {
            plan_id: plan_id.to_string(),
            title: "Plan".to_string(),
            markdown: "# Plan".to_string(),
        }),
        active_tools: Vec::new(),
        skills: Vec::new(),
        mcp_servers: Vec::new(),
        subagent_sessions: Vec::new(),
        git_branch: None,
    }
}

fn submitted_plan(plan_id: &str) -> SubmittedPlan {
    SubmittedPlan {
        id: plan_id.to_string(),
        title: "Plan".to_string(),
        markdown: "# Plan".to_string(),
        path: PathBuf::new(),
        created_at: Utc::now(),
    }
}

#[test]
fn tool_pause_queue_uses_arrival_order() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
        "tool_z",
    )));
    state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
        "tool_a",
    )));

    assert_eq!(state.active_tool_pause().unwrap().tool_use_id, "tool_z");
    assert_eq!(state.pending_tool_pauses.len(), 2);
}

#[test]
fn queued_tool_pause_does_not_reset_active_drawer_state() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
        "tool_1",
    )));
    state.permission_selected = 1;
    state.user_input_notes[0] = "not now".to_string();
    state.user_input_note_cursors[0] = state.user_input_notes[0].chars().count();

    state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
        "tool_2",
    )));

    assert_eq!(state.active_tool_pause().unwrap().tool_use_id, "tool_1");
    assert_eq!(state.permission_selected, 1);
    assert_eq!(state.current_user_input_note(), "not now");
}

#[test]
fn removing_active_tool_pause_prepares_next_request() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
        "tool_1",
    )));
    state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
        "tool_2",
    )));
    state.permission_selected = 1;
    state.user_input_notes[0] = "deny first".to_string();
    let removed_active = state.remove_tool_pause("tool_1");
    state.finish_tool_pause_removal(removed_active);

    assert_eq!(state.active_tool_pause().unwrap().tool_use_id, "tool_2");
    assert_eq!(state.permission_selected, 0);
    assert_eq!(state.current_user_input_note(), "");
    assert_eq!(state.agent_status, AgentStatus::AwaitingInput);
}

#[test]
fn formats_run_duration() {
    assert_eq!(format_run_duration(Duration::from_secs(0)), "0s");
    assert_eq!(format_run_duration(Duration::from_secs(7)), "7s");
    assert_eq!(format_run_duration(Duration::from_secs(67)), "1m07s");
    assert_eq!(format_run_duration(Duration::from_secs(3723)), "1h02m03s");
}

#[test]
fn run_timer_excludes_paused_duration() {
    let started_at = Instant::now();
    let mut timer = RunTimer::started_at(started_at);

    timer.pause_at(started_at + Duration::from_secs(10));
    assert_eq!(
        timer.elapsed_at(started_at + Duration::from_secs(30)),
        Duration::from_secs(10)
    );

    timer.resume_at(started_at + Duration::from_secs(30));
    assert_eq!(
        timer.elapsed_at(started_at + Duration::from_secs(35)),
        Duration::from_secs(15)
    );

    timer.pause_at(started_at + Duration::from_secs(40));
    assert_eq!(
        timer.finish_at(started_at + Duration::from_secs(50)),
        Duration::from_secs(20)
    );
}

#[test]
fn run_timer_starts_from_synced_elapsed_and_preserves_pause() {
    let now = Instant::now();
    let elapsed = Duration::from_secs(5);
    let running = RunTimer::started_with_elapsed_at(now, elapsed, false);

    assert_eq!(running.elapsed_at(now), elapsed);
    assert_eq!(
        running.elapsed_at(now + Duration::from_secs(2)),
        Duration::from_secs(7)
    );

    let paused = RunTimer::started_with_elapsed_at(now, elapsed, true);

    assert_eq!(paused.elapsed_at(now), elapsed);
    assert_eq!(paused.elapsed_at(now + Duration::from_secs(2)), elapsed);
    assert!(paused.is_paused());
}

#[test]
fn run_finished_appends_elapsed_divider_and_clears_timer() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::RunStarted);
    assert!(state.run_timer.is_some());

    state.apply_event(RuntimeToUiEvent::RunFinished);

    assert!(state.run_timer.is_none());
    assert!(matches!(
        state.messages.last(),
        Some(UiMessage::RunDivider { .. })
    ));
}

#[test]
fn runtime_status_sync_calibrates_elapsed_and_pause_state() {
    let mut state = UiState::new();
    let status = query_runtime_status(protocol::SessionRuntimeState::Waiting, 2_500, &["tool_1"]);

    state.apply_event(RuntimeToUiEvent::RunStarted);
    state.apply_event(RuntimeToUiEvent::RuntimeStatusSynced { status });

    assert_eq!(state.agent_status, AgentStatus::AwaitingInput);
    assert!(state.is_run_timer_paused());

    let timer = state.run_timer.as_ref().expect("timer should be synced");
    let now = Instant::now();
    let elapsed = timer.elapsed_at(now);
    assert!(elapsed >= Duration::from_millis(2_500));
    assert!(elapsed < Duration::from_millis(2_600));
    assert_eq!(timer.elapsed_at(now + Duration::from_secs(5)), elapsed);
}

#[test]
fn runtime_status_sync_applies_thinking_state() {
    let mut state = UiState::new();
    let status = query_runtime_status(protocol::SessionRuntimeState::Thinking, 2_500, &[]);

    state.apply_event(RuntimeToUiEvent::RuntimeStatusSynced { status });

    assert_eq!(state.agent_status, AgentStatus::Thinking);
    assert!(!state.is_run_timer_paused());
}

#[test]
fn replayed_run_started_keeps_synced_elapsed_timer() {
    let mut state = UiState::new();
    let status = query_runtime_status(protocol::SessionRuntimeState::Working, 2_500, &[]);

    state.apply_event(RuntimeToUiEvent::RuntimeStatusSynced { status });
    state.apply_event(RuntimeToUiEvent::RunStarted);

    let timer = state.run_timer.as_ref().expect("timer should stay synced");
    let elapsed = timer.elapsed_at(Instant::now());
    assert!(elapsed >= Duration::from_millis(2_500));
    assert!(elapsed < Duration::from_millis(2_600));
}

#[test]
fn runtime_status_sync_calibrates_compact_activity() {
    let mut state = UiState::new();
    let status = compact_runtime_status(1_200);

    state.apply_event(RuntimeToUiEvent::RuntimeStatusSynced { status });

    assert_eq!(state.agent_status, AgentStatus::Working);
    assert!(!state.manual_compact_running);
    assert!(!state.is_run_timer_paused());

    let timer = state.run_timer.as_ref().expect("timer should be synced");
    let elapsed = timer.elapsed_at(Instant::now());
    assert!(elapsed >= Duration::from_millis(1_200));
    assert!(elapsed < Duration::from_millis(1_300));
}

#[test]
fn runtime_status_sync_restores_pending_plan_approval_without_activity() {
    let mut state = UiState::new();
    let status = pending_plan_runtime_status("plan_1");

    state.apply_event(RuntimeToUiEvent::RuntimeStatusSynced { status });

    assert_eq!(
        state.plan_approval.as_ref().map(|plan| plan.id.as_str()),
        Some("plan_1")
    );
    assert!(state.run_timer.is_none());
}

#[test]
fn runtime_status_sync_updates_subagent_mention_candidates() {
    let mut state = UiState::new();
    state.input = "@wo".to_string();
    state.cursor_char = 3;
    let mut status = query_runtime_status(protocol::SessionRuntimeState::Working, 2_500, &[]);
    status.subagent_sessions = vec![
        AgentSummary {
            name: "explorer".to_string(),
            description: "Read-only codebase exploration agent.".to_string(),
        },
        AgentSummary {
            name: "worker".to_string(),
            description: "Implementation agent for focused coding tasks.".to_string(),
        },
    ];

    state.apply_event(RuntimeToUiEvent::RuntimeStatusSynced { status });

    assert!(state.mention_autocomplete.visible);
    let candidates: Vec<_> = state
        .mention_autocomplete
        .filtered
        .iter()
        .map(|candidate| {
            (
                candidate.kind,
                candidate.label.as_str(),
                candidate.target.as_str(),
                candidate.description.as_str(),
            )
        })
        .collect();
    assert_eq!(
        candidates,
        vec![(
            MentionKind::Subagent,
            "worker",
            "worker",
            "Implementation agent for focused coding tasks."
        )]
    );
}

#[test]
fn agent_management_update_refreshes_subagent_mention_candidates() {
    let mut state = UiState::new();
    state.input = "@wo".to_string();
    state.cursor_char = 3;

    state.apply_event(RuntimeToUiEvent::AgentManagementUpdated {
        records: vec![
            AgentRecord {
                name: "explorer".to_string(),
                description: "Read-only codebase exploration agent.".to_string(),
                instructions: "Explore.".to_string(),
                tools: Vec::new(),
                disallow_tools: Vec::new(),
                model: None,
                source_kind: AgentSourceKind::BuiltIn,
                path: None,
                editable: false,
            },
            AgentRecord {
                name: "worker".to_string(),
                description: "Implementation agent for focused coding tasks.".to_string(),
                instructions: "Work.".to_string(),
                tools: Vec::new(),
                disallow_tools: Vec::new(),
                model: None,
                source_kind: AgentSourceKind::Project,
                path: None,
                editable: true,
            },
        ],
    });

    assert!(state.mention_autocomplete.visible);
    let candidates: Vec<_> = state
        .mention_autocomplete
        .filtered
        .iter()
        .map(|candidate| candidate.label.as_str())
        .collect();
    assert_eq!(candidates, vec!["worker"]);
}

#[test]
fn plan_approval_resolved_closes_only_matching_plan() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::PlanSubmitted(submitted_plan("plan_1")));
    state.plan_approval_selected = 1;
    state.plan_approval_auto = true;
    state.apply_event(RuntimeToUiEvent::PlanApprovalResolved {
        plan_id: "other_plan".to_string(),
        action: protocol::PlanApprovalAction::ContinueDiscussing,
    });
    assert!(state.plan_approval.is_some());

    state.apply_event(RuntimeToUiEvent::PlanApprovalResolved {
        plan_id: "plan_1".to_string(),
        action: protocol::PlanApprovalAction::ContinueDiscussing,
    });

    assert!(state.plan_approval.is_none());
    assert_eq!(state.plan_approval_selected, 0);
    assert!(!state.plan_approval_auto);
}

#[test]
fn run_finished_divider_uses_synced_elapsed() {
    let mut state = UiState::new();
    let status = query_runtime_status(protocol::SessionRuntimeState::Working, 3_000, &[]);

    state.apply_event(RuntimeToUiEvent::RunStarted);
    state.apply_event(RuntimeToUiEvent::RuntimeStatusSynced { status });
    state.apply_event(RuntimeToUiEvent::RunFinished);

    let Some(UiMessage::RunDivider { elapsed }) = state.messages.last() else {
        panic!("expected run divider");
    };
    assert!(*elapsed >= Duration::from_secs(3));
    assert!(*elapsed < Duration::from_secs(4));
}

#[test]
fn run_finished_refreshes_input_placeholder() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::RunStarted);
    state.apply_event(RuntimeToUiEvent::RunFinished);

    assert!(INPUT_PLACEHOLDERS.contains(&state.input_placeholder.as_str()));
}

#[test]
fn run_started_removes_previous_elapsed_divider() {
    let mut state = UiState::new();
    state.messages.push(UiMessage::RunDivider {
        elapsed: Duration::from_secs(67),
    });

    state.apply_event(RuntimeToUiEvent::RunStarted);

    assert!(
        !state
            .messages
            .iter()
            .any(|message| matches!(message, UiMessage::RunDivider { .. }))
    );
}

#[test]
fn tool_pause_pauses_timer_until_result_removes_last_preview() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::RunStarted);
    state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
        "tool_1",
    )));

    assert!(state.is_run_timer_paused());

    state.apply_event(RuntimeToUiEvent::ToolResult(ToolResultBlock {
        tool_use_id: "tool_1".to_string(),
        is_error: false,
        content: String::new(),
        metadata: None,
    }));

    assert!(!state.is_run_timer_paused());
}

#[test]
fn permission_pause_prepares_single_note_slot() {
    let mut state = UiState::new();

    state.apply_event(RuntimeToUiEvent::ToolPauseRequested(permission_pause(
        "tool_1",
    )));

    assert_eq!(state.user_input_notes, vec![String::new()]);
    assert_eq!(state.user_input_note_cursors, vec![0]);
    assert!(!state.user_input_note_mode);
    assert_eq!(state.permission_selected, 0);
}

#[test]
fn subagent_spawn_tool_error_finishes_running_state() {
    let mut state = UiState::new();
    start_subagent(&mut state);

    state.apply_event(RuntimeToUiEvent::ToolResult(ToolResultBlock {
        tool_use_id: "tool_1".to_string(),
        is_error: true,
        content: "Stream error: Stream ended unexpectedly".to_string(),
        metadata: None,
    }));

    let node = state.subagents.get("sub_1").unwrap();
    assert_eq!(node.status, SubagentStatus::Failed);
}

#[test]
fn runtime_error_does_not_fail_running_subagent_state() {
    let mut state = UiState::new();
    start_subagent(&mut state);

    state.apply_event(RuntimeToUiEvent::error(
        "Cannot handle this request while a run is active".to_string(),
    ));

    let node = state.subagents.get("sub_1").unwrap();
    assert_eq!(node.status, SubagentStatus::Running);
}

#[test]
fn backspace_deletes_whole_mention_at_end() {
    let mut state = state_with_mention(9);
    state.delete_before();
    assert_eq!(state.input, "see now");
    assert_eq!(state.cursor_char, 4);
    assert!(state.input_mentions.is_empty());
}

#[test]
fn backspace_deletes_whole_mention_from_inside() {
    let mut state = state_with_mention(6);
    state.delete_before();
    assert_eq!(state.input, "see now");
    assert_eq!(state.cursor_char, 4);
    assert!(state.input_mentions.is_empty());
}

#[test]
fn delete_deletes_whole_mention_at_start() {
    let mut state = state_with_mention(4);
    state.delete_after();
    assert_eq!(state.input, "see now");
    assert_eq!(state.cursor_char, 4);
    assert!(state.input_mentions.is_empty());
}

#[test]
fn delete_deletes_whole_mention_from_inside() {
    let mut state = state_with_mention(6);
    state.delete_after();
    assert_eq!(state.input, "see now");
    assert_eq!(state.cursor_char, 4);
    assert!(state.input_mentions.is_empty());
}

#[test]
fn cursor_left_skips_whole_mention_at_end() {
    let mut state = state_with_mention(9);
    state.cursor_left();
    assert_eq!(state.cursor_char, 4);
}

#[test]
fn cursor_left_skips_whole_mention_from_inside() {
    let mut state = state_with_mention(6);
    state.cursor_left();
    assert_eq!(state.cursor_char, 4);
}

#[test]
fn cursor_right_skips_whole_mention_at_start() {
    let mut state = state_with_mention(4);
    state.cursor_right();
    assert_eq!(state.cursor_char, 9);
}

#[test]
fn cursor_right_skips_whole_mention_from_inside() {
    let mut state = state_with_mention(6);
    state.cursor_right();
    assert_eq!(state.cursor_char, 9);
}

#[test]
fn cursor_movement_in_plain_text_stays_character_based() {
    let mut state = state_with_mention(3);
    state.cursor_left();
    assert_eq!(state.cursor_char, 2);

    state.cursor_char = 9;
    state.cursor_right();
    assert_eq!(state.cursor_char, 10);
}

#[test]
fn inserted_mention_range_includes_trailing_space() {
    let mut state = UiState::new();
    state.input = "@sr".to_string();
    state.cursor_char = 3;
    state.mention_autocomplete.visible = true;
    state.mention_autocomplete.active_start = 0;
    state.mention_autocomplete.active_end = 3;
    state.mention_autocomplete.filtered.push(MentionCandidate {
        kind: MentionKind::Directory,
        label: "src".to_string(),
        target: "src".to_string(),
        description: "directory".to_string(),
    });

    assert!(state.insert_selected_mention());
    assert_eq!(state.input, "@src ");
    assert_eq!(state.cursor_char, 5);
    assert_eq!(state.input_mentions[0].start_char, 0);
    assert_eq!(state.input_mentions[0].end_char, 5);
}

#[test]
fn selected_image_mention_inserts_image_marker() {
    let image = temp_image_path("image.png");
    let cwd = image.parent().unwrap().to_path_buf();
    let mut state = UiState::new();
    state.status_bar.cwd = cwd;
    state.input = "@ima".to_string();
    state.cursor_char = 4;
    state.mention_autocomplete.visible = true;
    state.mention_autocomplete.active_start = 0;
    state.mention_autocomplete.active_end = 4;
    state.mention_autocomplete.filtered.push(MentionCandidate {
        kind: MentionKind::File,
        label: "image.png".to_string(),
        target: "image.png".to_string(),
        description: "file".to_string(),
    });

    assert!(state.insert_selected_mention());
    assert_eq!(state.input, "[Image#1] ");
    assert!(state.input_mentions.is_empty());
    assert_eq!(state.input_images.len(), 1);
    assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
}

#[test]
fn quoted_existing_image_path_paste_inserts_image_marker() {
    let image = temp_image_path("dragged.jpg");
    let mut state = UiState::new();

    state.insert_paste(format!("'{}'", image.display()));

    assert_eq!(state.input, "[Image#1] ");
    assert_eq!(state.input_images.len(), 1);
    assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
}

#[test]
fn nonexistent_image_path_paste_remains_text() {
    let mut state = UiState::new();
    let path = "/tmp/omini_missing_image.png";

    state.insert_paste(format!("'{path}'"));

    assert_eq!(state.input, format!("'{path}'"));
    assert!(state.input_images.is_empty());
}

#[test]
fn typed_quoted_existing_absolute_image_path_inserts_image_marker() {
    let image = temp_image_path("typed.png");
    let mut state = UiState::new();

    for ch in format!("'{}'", image.display()).chars() {
        state.insert_char(ch);
    }

    assert_eq!(state.input, "[Image#1] ");
    assert_eq!(state.input_images.len(), 1);
    assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
}

#[test]
fn typed_quoted_image_path_with_spaces_inserts_image_marker() {
    let image = temp_image_path("typed image.png");
    let mut state = UiState::new();

    for ch in format!("\"{}\"", image.display()).chars() {
        state.insert_char(ch);
    }

    assert_eq!(state.input, "[Image#1] ");
    assert_eq!(state.input_images.len(), 1);
    assert_eq!(state.input_images[0].source_path, image.to_string_lossy());
}

#[test]
fn typed_quoted_nonexistent_image_path_remains_text() {
    let mut state = UiState::new();
    let text = "'/tmp/omini_missing_typed_image.png'";

    for ch in text.chars() {
        state.insert_char(ch);
    }

    assert_eq!(state.input, text);
    assert!(state.input_images.is_empty());
}

#[test]
fn typed_quoted_non_image_path_remains_text() {
    let file = temp_image_path("not-image.txt");
    let mut state = UiState::new();
    let text = format!("'{}'", file.display());

    for ch in text.chars() {
        state.insert_char(ch);
    }

    assert_eq!(state.input, text);
    assert!(state.input_images.is_empty());
}

#[test]
fn typed_at_text_without_selection_remains_plain_text() {
    let mut state = UiState::new();
    for c in "@src ".chars() {
        state.insert_char(c);
        state.update_input_autocomplete();
    }

    assert_eq!(state.input, "@src ");
    assert!(state.input_mentions.is_empty());

    state.cursor_left();
    assert_eq!(state.cursor_char, 4);
    state.delete_before();
    assert_eq!(state.input, "@sr ");
}

#[test]
fn short_paste_inserts_literal_newlines() {
    let mut state = UiState::new();
    state.insert_paste("one\ntwo".to_string());

    assert_eq!(state.input, "one\ntwo");
    assert!(state.input_paste_markers.is_empty());
    assert_eq!(state.input_line_count(), 2);
}

#[test]
fn paste_over_two_lines_inserts_marker_even_when_short() {
    let mut state = UiState::new();
    let pasted = "a\nb\nc".to_string();
    state.insert_paste(pasted.clone());

    assert_eq!(state.input, format!("[Pasted Content {} chars]", 5));
    assert_eq!(state.input_paste_markers.len(), 1);

    let draft = state.take_input_draft().unwrap();
    assert_eq!(draft.text, pasted);
}

#[test]
fn long_paste_inserts_marker_and_submit_expands_original_text() {
    let mut state = UiState::new();
    let pasted = long_paste_text();
    state.insert_paste(pasted.clone());

    assert_eq!(state.input_paste_markers.len(), 1);
    assert_eq!(
        state.input,
        format!(
            "[Pasted Content {} chars]",
            PASTE_MARKER_THRESHOLD_CHARS + 1
        )
    );

    let draft = state.take_input_draft().unwrap();
    assert_eq!(draft.text, pasted);
    assert!(draft.mentions.is_empty());
    assert!(state.input.is_empty());
    assert!(state.input_paste_markers.is_empty());
}

#[test]
fn cursor_skips_whole_paste_marker() {
    let mut state = UiState::new();
    state.insert_paste(long_paste_text());
    let marker_len = state.input.chars().count();

    state.cursor_left();
    assert_eq!(state.cursor_char, 0);

    state.cursor_right();
    assert_eq!(state.cursor_char, marker_len);
}

#[test]
fn delete_removes_whole_paste_marker() {
    let mut state = UiState::new();
    state.insert_paste(long_paste_text());
    state.cursor_home();
    state.delete_after();

    assert!(state.input.is_empty());
    assert!(state.input_paste_markers.is_empty());
}

#[test]
fn backspace_removes_whole_paste_marker() {
    let mut state = UiState::new();
    state.insert_paste(long_paste_text());
    state.delete_before();

    assert!(state.input.is_empty());
    assert!(state.input_paste_markers.is_empty());
    assert_eq!(state.cursor_char, 0);
}

#[test]
fn clear_input_resets_text_and_attachment_state() {
    let image = temp_image_path("clear.png");
    let mut state = UiState::new();
    state.status_bar.cwd = image.parent().unwrap().to_path_buf();
    state.insert_paste(long_paste_text());
    state.insert_char(' ');
    let mention_start = state.cursor_char;
    state.insert_text("@src ");
    state.input_mentions.push(InputMention {
        start_char: mention_start,
        end_char: mention_start + 5,
        kind: MentionKind::Directory,
        label: "src".to_string(),
        target: "src".to_string(),
        description: "directory".to_string(),
    });
    state.insert_image_attachment(image);
    state.autocomplete.visible = true;
    state.mention_autocomplete.visible = true;
    state.input_scroll_line = 1;

    assert!(state.clear_input());

    assert!(state.input.is_empty());
    assert!(state.input_mentions.is_empty());
    assert!(state.input_images.is_empty());
    assert!(state.input_paste_markers.is_empty());
    assert_eq!(state.cursor_char, 0);
    assert_eq!(state.input_scroll_line, 0);
    assert!(!state.autocomplete.visible);
    assert!(!state.mention_autocomplete.visible);
}

#[test]
fn clear_input_returns_false_when_input_is_empty() {
    let mut state = UiState::new();

    assert!(!state.clear_input());
}

#[test]
fn mention_offsets_shift_after_paste_marker_expansion() {
    let mut state = UiState::new();
    let pasted = long_paste_text();
    state.insert_paste(pasted.clone());
    state.insert_char(' ');
    let mention_start = state.cursor_char;
    state.insert_text("@src ");
    state.input_mentions.push(InputMention {
        start_char: mention_start,
        end_char: mention_start + 5,
        kind: MentionKind::Directory,
        label: "src".to_string(),
        target: "src".to_string(),
        description: "directory".to_string(),
    });

    let draft = state.take_input_draft().unwrap();
    assert_eq!(draft.text, format!("{pasted} @src "));
    assert_eq!(draft.mentions[0].start_char, pasted.chars().count() + 1);
    assert_eq!(draft.mentions[0].end_char, pasted.chars().count() + 6);
}

#[test]
fn input_visible_lines_caps_at_three_and_cursor_scrolls() {
    let mut state = UiState::new();
    state.insert_text("a\nb\nc\nd");

    assert_eq!(state.input_line_count(), 4);
    assert_eq!(state.input_visible_line_count(), 3);
    assert_eq!(state.input_scroll_line, 1);

    assert!(state.cursor_up_in_input());
    assert_eq!(state.input_scroll_line, 1);
    assert!(state.cursor_up_in_input());
    assert_eq!(state.input_scroll_line, 1);
    assert!(state.cursor_up_in_input());
    assert_eq!(state.input_scroll_line, 0);
}

#[test]
fn input_soft_wraps_by_width_without_mutating_text() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("abcdefghi");

    assert_eq!(state.input, "abcdefghi");
    assert_eq!(state.input_line_bounds(), vec![(0, 4), (4, 8), (8, 9)]);
    assert_eq!(state.input_line_count(), 3);
    assert_eq!(state.input_visible_line_count(), 3);
}

#[test]
fn input_soft_wraps_wide_characters_by_display_width() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("你好吗x");

    assert_eq!(state.input_line_bounds(), vec![(0, 2), (2, 4)]);
    assert_eq!(state.input_display_width(0, 2), 4);
    assert_eq!(state.input_display_width(2, 4), 3);
}

#[test]
fn input_soft_wrap_scrolls_after_three_visible_lines() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("abcdefghijklmnopqrst");

    assert_eq!(
        state.input_line_bounds(),
        vec![(0, 4), (4, 8), (8, 12), (12, 16), (16, 20)]
    );
    assert_eq!(state.input_visible_line_count(), 3);
    assert_eq!(state.input_scroll_line, 2);
}

#[test]
fn cursor_moves_vertically_across_soft_wrapped_lines() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("abcdefghijklmnopqrst");

    assert_eq!(state.input_cursor_line_col(), Some((4, 4)));
    assert!(state.cursor_up_in_input());
    assert_eq!(state.input_cursor_line_col(), Some((3, 4)));
    assert_eq!(state.cursor_char, 16);

    assert!(state.cursor_down_in_input());
    assert_eq!(state.input_cursor_line_col(), Some((4, 4)));
    assert_eq!(state.cursor_char, 20);
}

#[test]
fn manual_newlines_remain_real_line_breaks_with_soft_wrap() {
    let mut state = UiState::new();
    state.set_input_wrap_width(6);
    state.insert_text("ab\ncdefghi");

    assert_eq!(state.input, "ab\ncdefghi");
    assert_eq!(state.input_line_bounds(), vec![(0, 2), (3, 7), (7, 10)]);

    let draft = state.take_input_draft().unwrap();
    assert_eq!(draft.text, "ab\ncdefghi");
}
