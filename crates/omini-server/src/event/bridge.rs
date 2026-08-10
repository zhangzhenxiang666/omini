use crate::store as store_model;
use crate::store;
use omini_config::Settings;
use omini_config::project::ThreadDir;
use omini_core::CoreError;
use omini_domain as domain;
use omini_protocol as client_proto;
use omini_runtime_contract as runtime_contract;
use std::path::Path;

#[cfg(test)]
pub fn submit_run_command_from_protocol_request(
    request: client_proto::SubmitRunRequest,
) -> runtime_contract::thread::SubmitRunCommand {
    runtime_contract::thread::SubmitRunCommand {
        draft: user_input_from_protocol(request.input, None)
            .expect("uploaded attachments require a thread directory"),
        client_echo_id: request.client_echo_id,
        mode: run_input_mode_from_protocol(request.mode),
    }
}

pub fn submit_run_command_from_protocol_request_for_thread(
    request: client_proto::SubmitRunRequest,
    thread_dir: &ThreadDir,
) -> Result<runtime_contract::thread::SubmitRunCommand, CoreError> {
    Ok(runtime_contract::thread::SubmitRunCommand {
        draft: user_input_from_protocol(request.input, Some(thread_dir))?,
        client_echo_id: request.client_echo_id,
        mode: run_input_mode_from_protocol(request.mode),
    })
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
            })
            .collect(),
    }
}

pub fn skill_response_from_runtime_skill_detail(
    skill: runtime_contract::thread::SkillDetailSnapshot,
) -> client_proto::SkillResponse {
    client_proto::SkillResponse {
        skill: client_proto::SkillDetail {
            name: skill.name,
            description: skill.description,
            short_description: skill.short_description,
            body: skill.body,
            directory: skill.directory.display().to_string(),
            user_invocable: skill.user_invocable,
        },
    }
}

pub fn session_runtime_skills_from_runtime_snapshot(
    skills: Vec<runtime_contract::thread::RuntimeSkillSnapshot>,
) -> Vec<client_proto::SessionRuntimeSkill> {
    skills
        .into_iter()
        .map(|skill| client_proto::SessionRuntimeSkill {
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

fn run_input_mode_from_protocol(
    mode: client_proto::RunInputMode,
) -> runtime_contract::thread::RunInputMode {
    match mode {
        client_proto::RunInputMode::Submit => runtime_contract::thread::RunInputMode::Submit,
        client_proto::RunInputMode::Intervene => runtime_contract::thread::RunInputMode::Intervene,
    }
}

fn user_input_from_protocol(
    input: client_proto::UserInput,
    thread_dir: Option<&ThreadDir>,
) -> Result<domain::display::UserDraft, CoreError> {
    Ok(domain::display::UserDraft {
        text: input.text.clone(),
        mentions: input
            .context_refs
            .unwrap_or_default()
            .into_iter()
            .filter_map(|context_ref| mention_from_protocol(&input.text, context_ref))
            .collect(),
        images: input
            .attachments
            .unwrap_or_default()
            .into_iter()
            .map(|attachment| image_from_protocol(attachment, thread_dir))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect(),
    })
}

fn mention_from_protocol(
    text: &str,
    context_ref: client_proto::ContextRef,
) -> Option<domain::display::DisplayMention> {
    let label = context_ref.label();
    let target = context_ref.target().to_string();
    let (start_char, end_char) = find_reference_span(text, &label).unwrap_or((0, 0));
    let (kind, description) = match context_ref {
        client_proto::ContextRef::File { .. } => (domain::display::MentionKind::File, "file"),
        client_proto::ContextRef::Directory { .. } => {
            (domain::display::MentionKind::Directory, "directory")
        }
        client_proto::ContextRef::Subagent { .. } => {
            (domain::display::MentionKind::Subagent, "subagent")
        }
        client_proto::ContextRef::Url { .. } => return None,
    };

    Some(domain::display::DisplayMention {
        start_char,
        end_char,
        kind,
        label,
        target,
        description: description.to_string(),
    })
}

fn find_reference_span(text: &str, label: &str) -> Option<(usize, usize)> {
    let needle = format!("@{label}");
    let byte_start = text.find(&needle)?;
    let start = text[..byte_start].chars().count();
    let end = start + needle.chars().count();
    Some((start, end))
}

fn image_from_protocol(
    attachment: client_proto::AttachmentRef,
    thread_dir: Option<&ThreadDir>,
) -> Result<Option<domain::display::DisplayImageAttachment>, CoreError> {
    match attachment {
        client_proto::AttachmentRef::LocalPath { path, name, .. } => {
            let file_name = name.unwrap_or_else(|| {
                Path::new(&path)
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .unwrap_or_default()
                    .to_string()
            });
            Ok(Some(domain::display::DisplayImageAttachment {
                start_char: 0,
                end_char: 0,
                marker: String::new(),
                source_path: path,
                file_name,
            }))
        }
        client_proto::AttachmentRef::Uploaded {
            attachment_id,
            mime_type,
            name,
        } => {
            let thread_dir = thread_dir.ok_or_else(|| {
                CoreError::new("uploaded attachment cannot be resolved without a thread")
            })?;
            let path = store::asset_path(thread_dir, &attachment_id, &mime_type)
                .map_err(|error| CoreError::persistence("invalid attachment", error.to_string()))?;
            if !path.is_file() {
                return Err(CoreError::new(format!(
                    "uploaded attachment '{}' does not exist",
                    attachment_id
                )));
            }
            Ok(Some(domain::display::DisplayImageAttachment {
                start_char: 0,
                end_char: 0,
                marker: String::new(),
                source_path: path.display().to_string(),
                file_name: name.unwrap_or(attachment_id),
            }))
        }
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
) -> client_proto::SessionRuntimeCapabilityStatus {
    match status {
        runtime_contract::thread::RuntimeCapabilityStatus::Available => {
            client_proto::SessionRuntimeCapabilityStatus::Available
        }
    }
}

pub fn models_response_from_settings(settings: &Settings) -> client_proto::ModelsResponse {
    let mut providers = settings
        .providers
        .iter()
        .map(|(id, provider)| client_proto::ProviderInfo {
            id: id.clone(),
            name: provider.name.clone(),
            endpoint: provider.endpoint,
            base_url: provider.base_url.clone(),
            models: provider
                .models
                .iter()
                .map(|model| client_proto::ModelInfo {
                    id: model.id.clone(),
                    name: model.name.clone(),
                    limit: model.limit,
                    thinking: model.thinking,
                    input_modalities: model.input_modalities.clone(),
                    extra_headers: model.extra_headers.clone(),
                    extra_body: model.extra_body.clone(),
                })
                .collect(),
        })
        .collect::<Vec<_>>();
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    client_proto::ModelsResponse {
        providers,
        current_provider: settings.active_provider.clone(),
        current_model: settings.model.clone(),
    }
}

/// 将数据库会话记录压缩成协议层会话摘要。
pub fn session_summary_from_store_record(
    session: store_model::Thread,
) -> client_proto::SessionSummary {
    client_proto::SessionSummary {
        id: session.id,
        title: session.title.unwrap_or_default(),
        model: session.model,
        provider: session.provider,
        created_at: session.created_at,
        updated_at: session.updated_at,
        runtime_state: None,
    }
}

/// 从首条用户输入生成默认会话标题。
pub fn fallback_session_title_from_user_input(input: &client_proto::UserInput) -> Option<String> {
    let title = input.text.trim();
    (!title.is_empty()).then(|| title.chars().take(300).collect())
}

/// 将持久化 snapshot 转成一组 runtime 事件供 TUI 恢复 UI。
pub fn protocol_events_from_loaded_session_snapshot(
    snapshot: domain::events::LoadedSession,
    context_window: Option<u32>,
    active_profile: domain::events::ActiveProfile,
) -> Result<Vec<client_proto::RuntimeEvent>, omini_core::CoreError> {
    let mut usage = snapshot.usage;
    usage.context_window = context_window;
    // snapshot 投影由 server 自己生成,不走 core 内部事件通道 —— title 也用
    // server-side helper 直接构造,绕开 `runtime_contract::RuntimeToServerEvent::SessionTitleChanged`。
    // `SessionSnapshot` 同样在 server 端直接构造 `client_proto::TypedRuntimeEvent`,
    // core 不再产生这条事件。
    let events = [
        session_title_changed_protocol_event(snapshot.title),
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
        client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::SessionSnapshot(
            client_proto::SessionSnapshotEvent {
                session_id: Some(snapshot.session_id),
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

pub fn thinking_display_changed_protocol_event(show: bool) -> client_proto::RuntimeEvent {
    client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::ThinkingDisplayChanged(
        client_proto::ThinkingDisplayChangedEvent { show },
    ))
}

/// Server 端直接构造的 session title 变更事件。新架构下 title 由 server 编排层
/// 负责,绕开 core 内部事件通道 —— 这里和 `thinking_display_changed_event` 对称。
pub fn session_title_changed_protocol_event(title: Option<String>) -> client_proto::RuntimeEvent {
    client_proto::RuntimeEvent::new(client_proto::TypedRuntimeEvent::SessionTitleChanged(
        client_proto::SessionTitleChangedEvent { title },
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
                    session_id: event.session_id,
                    agent_label: event.agent_label,
                },
            )
        }
        runtime_contract::RuntimeToServerEvent::CompactSummaryDelta(event) => {
            client_proto::TypedRuntimeEvent::CompactSummaryDelta(
                client_proto::CompactSummaryDeltaEvent {
                    trigger: event.trigger,
                    delta: event.delta,
                    session_id: event.session_id,
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
                    session_id: event.session_id,
                    agent_label: event.agent_label,
                },
            )
        }
        runtime_contract::RuntimeToServerEvent::CompactSummaryFailed(event) => {
            client_proto::TypedRuntimeEvent::CompactSummaryFailed(
                client_proto::CompactSummaryFailedEvent {
                    trigger: event.trigger,
                    message: event.message,
                    session_id: event.session_id,
                    agent_label: event.agent_label,
                },
            )
        }
        runtime_contract::RuntimeToServerEvent::AgentTaskEvent(event) => {
            client_proto::TypedRuntimeEvent::AgentTaskEvent(event)
        }
        runtime_contract::RuntimeToServerEvent::SessionSwitched { from, to } => {
            client_proto::TypedRuntimeEvent::SessionSwitched(client_proto::SessionSwitchedEvent {
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
    fn initial_session_title_from_input_trims_and_limits_text() {
        let input = client_proto::UserInput::plain(format!("  {}  ", "a".repeat(400)));

        let title =
            fallback_session_title_from_user_input(&input).expect("title should be present");

        assert_eq!(title.len(), 300);
        assert!(title.chars().all(|ch| ch == 'a'));
    }

    #[test]
    fn initial_session_title_from_input_skips_blank_text() {
        let input = client_proto::UserInput::plain("   ");

        assert_eq!(fallback_session_title_from_user_input(&input), None);
    }

    #[test]
    fn submit_run_command_from_protocol_maps_input_and_mode() {
        let command = submit_run_command_from_protocol_request(client_proto::SubmitRunRequest {
            input: client_proto::UserInput {
                text: "open @src/lib.rs".to_string(),
                context_refs: Some(vec![
                    client_proto::ContextRef::File {
                        path: "src/lib.rs".to_string(),
                        label: None,
                    },
                    client_proto::ContextRef::Url {
                        url: "https://example.com".to_string(),
                        label: None,
                    },
                ]),
                attachments: Some(vec![client_proto::AttachmentRef::LocalPath {
                    path: "/tmp/diagram.png".to_string(),
                    mime_type: Some("image/png".to_string()),
                    name: None,
                }]),
            },
            client_echo_id: Some("echo-1".to_string()),
            mode: client_proto::RunInputMode::Intervene,
        });

        assert_eq!(
            command.mode,
            runtime_contract::thread::RunInputMode::Intervene
        );
        assert_eq!(command.client_echo_id.as_deref(), Some("echo-1"));
        assert_eq!(command.draft.text, "open @src/lib.rs");
        assert_eq!(command.draft.mentions.len(), 1);
        assert_eq!(
            command.draft.mentions[0].kind,
            domain::display::MentionKind::File
        );
        assert_eq!(command.draft.mentions[0].start_char, 5);
        assert_eq!(command.draft.mentions[0].end_char, 16);
        assert_eq!(command.draft.images.len(), 1);
        assert_eq!(command.draft.images[0].file_name, "diagram.png");
    }

    #[test]
    fn session_snapshot_events_replay_current_session_state() {
        let events = protocol_events_from_loaded_session_snapshot(
            domain::events::LoadedSession {
                session_id: "s1".to_string(),
                provider: "main".to_string(),
                model: "test-model".to_string(),
                thinking_effort: None,
                active_profile: domain::events::ActiveProfile::Main,
                title: Some("hello".to_string()),
                messages: vec![domain::display::HistoryItem::Message(
                    domain::message::Message::from_user_text("hello".to_string()),
                )],
                agent_tasks: Vec::new(),
                usage: domain::events::SessionUsageSnapshot {
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
                "session_title_changed",
                "model_changed",
                "active_profile_changed",
                "session_snapshot"
            ]
        );
        assert!(matches!(
            &events[0].event,
            client_proto::TypedRuntimeEvent::SessionTitleChanged(event)
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
            client_proto::TypedRuntimeEvent::SessionSnapshot(event)
                if event.session_id.as_deref() == Some("s1")
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
                    session_id: Some("session_1".to_string()),
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
                    session_id: Some("session_1".to_string()),
                    agent_label: None,
                },
            )
        );
    }

    #[test]
    fn plan_approval_resolved_is_typed() {
        let event = runtime_event_from_runtime_contract_event(
            runtime_contract::RuntimeToServerEvent::PlanApprovalResolved {
                plan_id: "plan_1".to_string(),
                action: domain::events::PlanApprovalAction::ContinueDiscussing,
            },
        )
        .expect("event should encode");

        assert_eq!(event.kind(), "plan_approval_resolved");
        assert_eq!(
            event.event,
            client_proto::TypedRuntimeEvent::PlanApprovalResolved(
                client_proto::PlanApprovalResolvedEvent {
                    plan_id: "plan_1".to_string(),
                    action: client_proto::PlanApprovalAction::ContinueDiscussing,
                },
            )
        );
    }

    #[test]
    fn thinking_display_changed_is_typed() {
        let event = thinking_display_changed_protocol_event(false);

        assert_eq!(event.kind(), "thinking_display_changed");
        assert!(matches!(
            event.event,
            client_proto::TypedRuntimeEvent::ThinkingDisplayChanged(
                client_proto::ThinkingDisplayChangedEvent { show: false }
            )
        ));
    }
}
