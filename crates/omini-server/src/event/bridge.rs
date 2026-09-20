use crate::store as store_model;
use crate::thread::ThreadRuntime;
use omini_config::Settings;
use omini_core::CoreError;
use omini_domain as domain;
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;
use std::collections::HashSet;
use std::path::{Component, Path};

pub async fn submit_run_command_from_protocol_request_for_thread(
    request: client_proto::SubmitRunRequest,
    thread: &ThreadRuntime,
) -> Result<runtime_contract::thread::SubmitRunCommand, CoreError> {
    let (mut input, client_echo_id, intent) = match request {
        client_proto::SubmitRunRequest::SubmitMessage {
            input,
            client_echo_id,
        } => (
            input,
            client_echo_id,
            runtime_contract::thread::RunIntent::SubmitMessage,
        ),
        client_proto::SubmitRunRequest::InterveneMessage {
            input,
            client_echo_id,
        } => (
            input,
            client_echo_id,
            runtime_contract::thread::RunIntent::InterveneMessage,
        ),
        client_proto::SubmitRunRequest::ExecuteCommand {
            command,
            input,
            client_echo_id,
        } => (
            input,
            client_echo_id,
            runtime_contract::thread::RunIntent::ExecuteCommand(command),
        ),
    };
    validate_attachment_ids(&input.attachment_ids)?;
    normalize_input_parts(&mut input.parts, thread.cwd())?;
    let attachments = thread.resolve_attachments(&input.attachment_ids).await?;
    Ok(runtime_contract::thread::SubmitRunCommand {
        input: domain::input::RuntimeUserInput {
            parts: input.parts,
            attachments,
        },
        client_echo_id,
        intent,
    })
}

fn validate_attachment_ids(attachment_ids: &[String]) -> Result<(), CoreError> {
    let mut unique = HashSet::with_capacity(attachment_ids.len());
    if attachment_ids
        .iter()
        .any(|attachment_id| attachment_id.is_empty() || !unique.insert(attachment_id))
    {
        return Err(CoreError::invalid_input(
            "invalid_input_part",
            "attachment IDs must be non-empty and unique",
        ));
    }
    Ok(())
}

fn normalize_input_parts(
    parts: &mut [client_proto::InputPart],
    cwd: &Path,
) -> Result<(), CoreError> {
    let canonical_root = cwd.canonicalize().map_err(|error| {
        CoreError::invalid_input(
            "invalid_input_part",
            format!("project root cannot be resolved: {error}"),
        )
    })?;
    for part in parts {
        let (path, expected_directory) = match part {
            client_proto::InputPart::File { path, .. } => (path, false),
            client_proto::InputPart::Directory { path, .. } => (path, true),
            client_proto::InputPart::Text { .. }
            | client_proto::InputPart::Skill { .. }
            | client_proto::InputPart::Subagent { .. } => continue,
        };
        let relative = Path::new(path);
        if relative.is_absolute()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(CoreError::invalid_input(
                "invalid_input_part",
                format!("project reference '{path}' is not a safe relative path"),
            ));
        }
        let canonical = canonical_root.join(relative).canonicalize().map_err(|_| {
            CoreError::invalid_input(
                "invalid_input_part",
                format!("project reference '{path}' does not exist"),
            )
        })?;
        if !canonical.starts_with(&canonical_root)
            || (expected_directory && !canonical.is_dir())
            || (!expected_directory && !canonical.is_file())
        {
            return Err(CoreError::invalid_input(
                "invalid_input_part",
                format!("project reference '{path}' has the wrong type or escapes the project"),
            ));
        }
        *path = canonical
            .strip_prefix(&canonical_root)
            .expect("contained path has a relative suffix")
            .to_string_lossy()
            .replace('\\', "/");
    }
    Ok(())
}

pub fn run_submitted_response_from_runtime_result(
    result: runtime_contract::thread::RunSubmitted,
) -> client_proto::RunSubmittedResponse {
    client_proto::RunSubmittedResponse {
        run_id: result.run_id,
    }
}

pub fn models_response_from_runtime_snapshot(
    snapshot: runtime_contract::thread::ModelsSnapshot,
) -> client_proto::ModelsResponse {
    client_proto::ModelsResponse {
        providers: snapshot.providers,
        current_provider: snapshot.current_provider,
        current_model: snapshot.current_model,
    }
}

pub fn agents_response_from_runtime_snapshot(
    snapshot: runtime_contract::thread::AgentsSnapshot,
) -> client_proto::AgentsResponse {
    client_proto::AgentsResponse {
        records: snapshot
            .records
            .into_iter()
            .map(agent_record_snapshot_to_protocol)
            .collect(),
        providers: snapshot.providers,
        current_provider: snapshot.current_provider,
        current_model: snapshot.current_model,
    }
}

pub fn skills_response_from_runtime_skill_summaries(
    skills: Vec<runtime_contract::thread::SkillSummarySnapshot>,
) -> client_proto::SkillsResponse {
    client_proto::SkillsResponse {
        skills: skills
            .into_iter()
            .map(|skill| client_proto::SkillSummary {
                name: skill.name,
                description: skill.description,
                short_description: skill.short_description,
                argument_hint: skill.argument_hint,
            })
            .collect(),
    }
}

pub fn thread_runtime_skills_from_runtime_snapshot(
    skills: Vec<runtime_contract::thread::RuntimeSkillSnapshot>,
) -> Vec<client_proto::ThreadRuntimeSkill> {
    skills
        .into_iter()
        .map(|skill| client_proto::ThreadRuntimeSkill {
            name: skill.name,
            description: skill.description,
            short_description: skill.short_description,
            source_kind: runtime_skill_source_kind_to_protocol(skill.source_kind),
            directory: skill.directory.display().to_string(),
            status: runtime_capability_status_to_protocol(skill.status),
            disable_model_invocation: skill.disable_model_invocation,
            user_invocable: skill.user_invocable,
        })
        .collect()
}

pub fn set_model_command_from_protocol_request(
    request: client_proto::SetModelRequest,
) -> runtime_contract::thread::SetModelCommand {
    runtime_contract::thread::SetModelCommand {
        provider: request.provider,
        model: request.model,
        thinking_effort: request.thinking_effort,
    }
}

pub fn set_thinking_effort_command_from_protocol_request(
    request: client_proto::SetThinkingEffortRequest,
) -> runtime_contract::thread::SetThinkingEffortCommand {
    runtime_contract::thread::SetThinkingEffortCommand {
        effort: request.effort,
    }
}

pub fn set_active_profile_command_from_protocol_request(
    request: client_proto::SetActiveProfileRequest,
) -> runtime_contract::thread::SetActiveProfileCommand {
    runtime_contract::thread::SetActiveProfileCommand {
        profile: request.profile,
    }
}

pub fn resolve_tool_pause_command_from_protocol_request(
    tool_use_id: String,
    request: client_proto::ResolveToolPauseRequest,
) -> runtime_contract::thread::ResolveToolPauseCommand {
    runtime_contract::thread::ResolveToolPauseCommand {
        tool_use_id,
        response: request.response,
    }
}

pub fn resolve_plan_command_from_protocol_request(
    plan_id: String,
    request: client_proto::ResolvePlanRequest,
) -> runtime_contract::thread::ResolvePlanCommand {
    runtime_contract::thread::ResolvePlanCommand {
        plan_id,
        action: request.action,
    }
}

fn agent_record_snapshot_to_protocol(
    record: domain::subagents::AgentRecord,
) -> client_proto::AgentRecord {
    let id = record
        .path
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| record.name.clone());
    client_proto::AgentRecord {
        id,
        name: record.name,
        description: record.description,
        short_description: record.short_description,
        instructions: record.instructions,
        tools: record.tools,
        disallow_tools: record.disallow_tools,
        model: record.model,
        source_kind: record.source_kind,
        editable: record.editable,
    }
}

fn runtime_skill_source_kind_to_protocol(
    source_kind: runtime_contract::thread::RuntimeSkillSourceKind,
) -> client_proto::SkillSourceKind {
    match source_kind {
        runtime_contract::thread::RuntimeSkillSourceKind::BuiltIn => {
            client_proto::SkillSourceKind::BuiltIn
        }
        runtime_contract::thread::RuntimeSkillSourceKind::Project => {
            client_proto::SkillSourceKind::Project
        }
        runtime_contract::thread::RuntimeSkillSourceKind::User => {
            client_proto::SkillSourceKind::User
        }
    }
}

fn runtime_capability_status_to_protocol(
    status: runtime_contract::thread::RuntimeCapabilityStatus,
) -> client_proto::ThreadRuntimeCapabilityStatus {
    match status {
        runtime_contract::thread::RuntimeCapabilityStatus::Available => {
            client_proto::ThreadRuntimeCapabilityStatus::Available
        }
    }
}

pub fn models_response_from_settings(settings: &Settings) -> client_proto::ModelsResponse {
    let mut providers = settings.resolved_config().catalog();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    let model = settings.active_model();
    client_proto::ModelsResponse {
        providers,
        current_provider: model.provider_id.clone(),
        current_model: model.model_id.clone(),
    }
}

/// 将数据库线程记录压缩成协议层线程摘要。
pub fn thread_summary_from_store_record(
    thread: store_model::Thread,
) -> client_proto::ThreadSummary {
    client_proto::ThreadSummary {
        id: thread.id,
        title: thread.title.unwrap_or_default(),
        model: thread.model,
        provider: thread.provider,
        created_at: thread.created_at,
        updated_at: thread.updated_at,
        runtime_state: None,
    }
}

/// 从首条用户输入生成默认线程标题。
pub fn fallback_thread_title_from_user_input(input: &client_proto::UserInput) -> Option<String> {
    let text = input
        .parts
        .iter()
        .filter_map(|part| match part {
            client_proto::InputPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    let title = text.trim();
    (!title.is_empty()).then(|| title.chars().take(300).collect())
}

/// 将持久化 snapshot 转成一组 runtime 事件供 TUI 恢复 UI。
pub fn protocol_events_from_loaded_thread_snapshot(
    snapshot: domain::events::LoadedThread,
    context_window: Option<u32>,
    active_profile: domain::events::ActiveProfile,
) -> Result<Vec<client_proto::RuntimeEvent>, omini_core::CoreError> {
    let mut usage = snapshot.usage;
    usage.context_window = context_window;
    // snapshot 投影由 server 自己生成,不走 core 内部事件通道 —— title 也用
    // server-side helper 直接构造,绕开 `runtime_contract::RuntimeToServerEvent::ThreadTitleChanged`。
    // `ThreadSnapshot` 同样在 server 端直接构造 `client_proto::TypedRuntimeEvent`,
    // core 不再产生这条事件。
    let events = [
        thread_title_changed_protocol_event(snapshot.title),
        runtime_event_from_runtime_contract_event(
            runtime_contract::RuntimeToServerEvent::ModelChanged {
                provider: snapshot.provider,
                model: snapshot.model,
                thinking_effort: snapshot.thinking_effort,
                context_window,
            },
        )?,
        runtime_event_from_runtime_contract_event(
            runtime_contract::RuntimeToServerEvent::ActiveProfileChanged(active_profile),
        )?,
        client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::ThreadSnapshot(
            client_proto::ThreadSnapshotEvent {
                thread_id: snapshot.thread_id,
                messages: snapshot.messages,
                agent_tasks: snapshot.agent_tasks,
                usage,
            },
        )),
    ];

    Ok(events.to_vec())
}

/// 把 core 内部 runtime 事件编码成协议 `RuntimeEvent`。
pub fn runtime_event_from_runtime_contract_event(
    event: runtime_contract::RuntimeToServerEvent,
) -> Result<client_proto::RuntimeEvent, CoreError> {
    Ok(client_proto::RuntimeEvent::new(
        typed_runtime_event_from_runtime_contract_event(event),
    ))
}

/// Server 端直接构造的 thread title 变更事件。新架构下 title 由 server 编排层
/// 负责,绕开 core 内部事件通道。
pub fn thread_title_changed_protocol_event(title: Option<String>) -> client_proto::RuntimeEvent {
    client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::ThreadTitleChanged(
        client_proto::ThreadTitleChangedEvent { title },
    ))
}

fn typed_runtime_event_from_runtime_contract_event(
    event: runtime_contract::RuntimeToServerEvent,
) -> client_proto::TypedRuntimeEvent {
    match event {
        runtime_contract::RuntimeToServerEvent::RunStarted => {
            client_proto::TypedRuntimeEvent::RunStarted
        }
        runtime_contract::RuntimeToServerEvent::UserMessageInjected {
            item,
            client_echo_id,
        } => client_proto::TypedRuntimeEvent::UserMessageInjected {
            item,
            client_echo_id,
        },
        runtime_contract::RuntimeToServerEvent::RunFinished => {
            client_proto::TypedRuntimeEvent::RunFinished
        }
        runtime_contract::RuntimeToServerEvent::Notification(notification) => {
            client_proto::TypedRuntimeEvent::Notification(client_proto::NotificationEvent {
                level: notification_level_to_protocol(notification.kind),
                message: notification.message,
                details: notification.details,
            })
        }
        runtime_contract::RuntimeToServerEvent::ModelChanged {
            provider,
            model,
            thinking_effort,
            context_window,
        } => client_proto::TypedRuntimeEvent::ModelChanged(client_proto::ModelChangedEvent {
            provider,
            model,
            thinking_effort,
            context_window,
        }),
        runtime_contract::RuntimeToServerEvent::UsageChanged(usage) => {
            client_proto::TypedRuntimeEvent::UsageChanged(usage)
        }
        runtime_contract::RuntimeToServerEvent::UsageTotalsChanged {
            total_tokens,
            total_cached_tokens,
        } => client_proto::TypedRuntimeEvent::UsageTotalsChanged(
            client_proto::UsageTotalsChangedEvent {
                total_tokens,
                total_cached_tokens,
            },
        ),
        runtime_contract::RuntimeToServerEvent::ActiveProfileChanged(profile) => {
            client_proto::TypedRuntimeEvent::ActiveProfileChanged(
                client_proto::ActiveProfileChangedEvent { profile },
            )
        }
        runtime_contract::RuntimeToServerEvent::AgentManagementUpdated { records } => {
            client_proto::TypedRuntimeEvent::AgentManagementUpdated { records }
        }
        runtime_contract::RuntimeToServerEvent::TurnStarted => {
            client_proto::TypedRuntimeEvent::TurnStarted
        }
        runtime_contract::RuntimeToServerEvent::TurnEnded => {
            client_proto::TypedRuntimeEvent::TurnEnded
        }
        runtime_contract::RuntimeToServerEvent::ThinkingDelta(delta) => {
            client_proto::TypedRuntimeEvent::ThinkingDelta(client_proto::RuntimeDeltaEvent {
                delta,
            })
        }
        runtime_contract::RuntimeToServerEvent::TextDelta(delta) => {
            client_proto::TypedRuntimeEvent::TextDelta(client_proto::RuntimeDeltaEvent { delta })
        }
        runtime_contract::RuntimeToServerEvent::ProposedPlanDelta(delta) => {
            client_proto::TypedRuntimeEvent::ProposedPlanDelta(client_proto::RuntimeDeltaEvent {
                delta,
            })
        }
        runtime_contract::RuntimeToServerEvent::ToolUse(tool_use) => {
            client_proto::TypedRuntimeEvent::ToolUse(tool_use)
        }
        runtime_contract::RuntimeToServerEvent::ToolResult(tool_result) => {
            client_proto::TypedRuntimeEvent::ToolResult(tool_result)
        }
        runtime_contract::RuntimeToServerEvent::ToolPauseRequested(request) => {
            client_proto::TypedRuntimeEvent::ToolPauseRequested(request)
        }
        runtime_contract::RuntimeToServerEvent::PlanSubmitted(plan) => {
            client_proto::TypedRuntimeEvent::PlanSubmitted(plan)
        }
        runtime_contract::RuntimeToServerEvent::PlanApprovalResolved { plan_id, action } => {
            client_proto::TypedRuntimeEvent::PlanApprovalResolved(
                client_proto::PlanApprovalResolvedEvent { plan_id, action },
            )
        }
        runtime_contract::RuntimeToServerEvent::CompactSummaryStarted(event) => {
            client_proto::TypedRuntimeEvent::CompactSummaryStarted(
                client_proto::CompactSummaryStartedEvent {
                    trigger: event.trigger,
                    thread_id: event.thread_id,
                    agent_label: event.agent_label,
                },
            )
        }
        runtime_contract::RuntimeToServerEvent::CompactSummaryDelta(event) => {
            client_proto::TypedRuntimeEvent::CompactSummaryDelta(
                client_proto::CompactSummaryDeltaEvent {
                    trigger: event.trigger,
                    delta: event.delta,
                    thread_id: event.thread_id,
                    agent_label: event.agent_label,
                },
            )
        }
        runtime_contract::RuntimeToServerEvent::CompactSummaryFinished(event) => {
            client_proto::TypedRuntimeEvent::CompactSummaryFinished(
                client_proto::CompactSummaryFinishedEvent {
                    trigger: event.trigger,
                    summary: event.summary,
                    after_tokens: event.after_tokens,
                    thread_id: event.thread_id,
                    agent_label: event.agent_label,
                },
            )
        }
        runtime_contract::RuntimeToServerEvent::CompactSummaryFailed(event) => {
            client_proto::TypedRuntimeEvent::CompactSummaryFailed(
                client_proto::CompactSummaryFailedEvent {
                    trigger: event.trigger,
                    message: event.message,
                    thread_id: event.thread_id,
                    agent_label: event.agent_label,
                },
            )
        }
        runtime_contract::RuntimeToServerEvent::AgentTaskEvent(event) => {
            client_proto::TypedRuntimeEvent::AgentTaskEvent(event)
        }
        runtime_contract::RuntimeToServerEvent::ThreadSwitched { from, to } => {
            client_proto::TypedRuntimeEvent::ThreadSwitched(client_proto::ThreadSwitchedEvent {
                from,
                to,
            })
        }
    }
}

fn notification_level_to_protocol(
    kind: domain::events::NotificationKind,
) -> client_proto::NotificationLevel {
    match kind {
        domain::events::NotificationKind::Info => client_proto::NotificationLevel::Info,
        domain::events::NotificationKind::Warn => client_proto::NotificationLevel::Warn,
        domain::events::NotificationKind::Error => client_proto::NotificationLevel::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_domain as domain;

    #[test]
    fn initial_thread_title_from_input_trims_and_limits_text() {
        let input = client_proto::UserInput::plain(format!("  {}  ", "a".repeat(400)));

        let title = fallback_thread_title_from_user_input(&input).expect("title should be present");

        assert_eq!(title.len(), 300);
        assert!(title.chars().all(|ch| ch == 'a'));
    }

    #[test]
    fn initial_thread_title_from_input_skips_blank_text() {
        let input = client_proto::UserInput::plain("   ");

        assert_eq!(fallback_thread_title_from_user_input(&input), None);
    }

    #[test]
    fn project_references_are_normalized_without_changing_part_order() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut parts = vec![
            client_proto::InputPart::Text {
                text: "before ".to_string(),
            },
            client_proto::InputPart::File {
                path: "src/event/bridge.rs".to_string(),
                label: Some("bridge".to_string()),
            },
            client_proto::InputPart::Directory {
                path: "src/event".to_string(),
                label: None,
            },
            client_proto::InputPart::Text {
                text: " after".to_string(),
            },
        ];

        normalize_input_parts(&mut parts, root).expect("project references should validate");

        assert!(matches!(parts[0], client_proto::InputPart::Text { .. }));
        assert!(matches!(parts[1], client_proto::InputPart::File { .. }));
        assert!(matches!(
            parts[2],
            client_proto::InputPart::Directory { .. }
        ));
        assert!(matches!(parts[3], client_proto::InputPart::Text { .. }));
    }

    #[test]
    fn duplicate_attachment_ids_are_rejected() {
        let error = validate_attachment_ids(&["same".to_string(), "same".to_string()])
            .expect_err("duplicates must be rejected");
        assert_eq!(error.code(), "invalid_input_part");
    }

    #[cfg(unix)]
    #[test]
    fn project_references_reject_parent_absolute_and_symlink_escape() {
        use std::os::unix::fs::symlink;

        let base =
            std::env::temp_dir().join(format!("omini-input-reference-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        let outside = base.join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), b"secret").unwrap();
        symlink(&outside, project.join("escape")).unwrap();

        for path in [
            "../outside/secret.txt",
            "/tmp/absolute",
            "escape/secret.txt",
        ] {
            let mut parts = vec![client_proto::InputPart::File {
                path: path.to_string(),
                label: None,
            }];
            let error = normalize_input_parts(&mut parts, &project)
                .expect_err("unsafe project reference must fail");
            assert_eq!(error.code(), "invalid_input_part");
        }

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn thread_snapshot_events_replay_current_thread_state() {
        let events = protocol_events_from_loaded_thread_snapshot(
            domain::events::LoadedThread {
                thread_id: "s1".to_string(),
                provider: "main".to_string(),
                model: "test-model".to_string(),
                thinking_effort: None,
                active_profile: domain::events::ActiveProfile::Main,
                title: Some("hello".to_string()),
                messages: vec![domain::display::HistoryItem::Message(
                    domain::message::Message::from_user_text("hello".to_string()),
                )],
                agent_tasks: Vec::new(),
                usage: domain::events::ThreadUsageSnapshot {
                    current_context_tokens: 3,
                    total_tokens: 5,
                    total_cached_tokens: 1,
                    context_window: None,
                },
            },
            Some(1000),
            domain::events::ActiveProfile::Plan,
        )
        .expect("snapshot events should encode");

        assert_eq!(
            events.iter().map(|event| event.kind()).collect::<Vec<_>>(),
            vec![
                "thread_title_changed",
                "model_changed",
                "active_profile_changed",
                "thread_snapshot"
            ]
        );
        assert!(matches!(
            &events[0].event,
            client_proto::TypedRuntimeEvent::ThreadTitleChanged(event)
                if event.title.as_deref() == Some("hello")
        ));
        assert!(matches!(
            &events[1].event,
            client_proto::TypedRuntimeEvent::ModelChanged(event)
                if event.context_window == Some(1000)
        ));
        assert!(matches!(
            &events[2].event,
            client_proto::TypedRuntimeEvent::ActiveProfileChanged(event)
                if event.profile == client_proto::ActiveProfile::Plan
        ));
        assert!(matches!(
            &events[3].event,
            client_proto::TypedRuntimeEvent::ThreadSnapshot(event)
                if event.thread_id == "s1"
                    && event.usage.context_window == Some(1000)
                    && event.messages.len() == 1
        ));
    }

    #[test]
    fn active_profile_changed_is_typed() {
        let event = runtime_event_from_runtime_contract_event(
            runtime_contract::RuntimeToServerEvent::ActiveProfileChanged(
                domain::events::ActiveProfile::Plan,
            ),
        )
        .expect("event should encode");

        assert_eq!(event.kind(), "active_profile_changed");
        assert!(matches!(
            event.event,
            client_proto::TypedRuntimeEvent::ActiveProfileChanged(
                client_proto::ActiveProfileChangedEvent {
                    profile: client_proto::ActiveProfile::Plan,
                },
            )
        ));
    }

    #[test]
    fn compact_summary_finished_is_typed() {
        let event = runtime_event_from_runtime_contract_event(
            runtime_contract::RuntimeToServerEvent::CompactSummaryFinished(
                domain::events::CompactSummaryFinishedEvent {
                    trigger: domain::events::CompactTrigger::Manual,
                    summary: "summary".to_string(),
                    after_tokens: 42,
                    thread_id: Some("thread_1".to_string()),
                    agent_label: None,
                },
            ),
        )
        .expect("event should encode");

        assert_eq!(event.kind(), "compact_summary_finished");
        assert_eq!(
            event.event,
            client_proto::TypedRuntimeEvent::CompactSummaryFinished(
                client_proto::CompactSummaryFinishedEvent {
                    trigger: client_proto::CompactTrigger::Manual,
                    summary: "summary".to_string(),
                    after_tokens: 42,
                    thread_id: Some("thread_1".to_string()),
                    agent_label: None,
                },
            )
        );
    }

    #[test]
    fn plan_approval_resolved_is_typed() {
        let event = runtime_event_from_runtime_contract_event(
            runtime_contract::RuntimeToServerEvent::PlanApprovalResolved {
                plan_id: "plan".to_string(),
                action: domain::events::PlanApprovalAction::ContinueDiscussing,
            },
        )
        .expect("event should encode");

        assert_eq!(event.kind(), "plan_approval_resolved");
        assert_eq!(
            event.event,
            client_proto::TypedRuntimeEvent::PlanApprovalResolved(
                client_proto::PlanApprovalResolvedEvent {
                    plan_id: "plan".to_string(),
                    action: client_proto::PlanApprovalAction::ContinueDiscussing,
                },
            )
        );
    }
}
