use crate::types::config as config_types;
use crate::types::events as event_types;
use omini_domain::display as display_types;
use omini_domain::subagents as subagent_types;

pub(crate) fn user_input_from_draft(draft: display_types::UserDraft) -> omini_protocol::UserInput {
    let context_refs = draft
        .mentions
        .into_iter()
        .filter_map(context_ref_from_mention)
        .collect::<Vec<_>>();
    let attachments = draft
        .images
        .into_iter()
        .map(attachment_from_image)
        .collect::<Vec<_>>();

    omini_protocol::UserInput {
        text: draft.text,
        context_refs: (!context_refs.is_empty()).then_some(context_refs),
        attachments: (!attachments.is_empty()).then_some(attachments),
    }
}

fn context_ref_from_mention(
    mention: display_types::DisplayMention,
) -> Option<omini_protocol::ContextRef> {
    match mention.kind {
        display_types::MentionKind::File => Some(omini_protocol::ContextRef::File {
            path: mention.target,
            label: Some(mention.label),
        }),
        display_types::MentionKind::Directory => Some(omini_protocol::ContextRef::Directory {
            path: mention.target,
            label: Some(mention.label),
        }),
        display_types::MentionKind::Subagent => Some(omini_protocol::ContextRef::Subagent {
            name: mention.target,
            label: Some(mention.label),
        }),
        display_types::MentionKind::Command => None,
    }
}

fn attachment_from_image(
    image: display_types::DisplayImageAttachment,
) -> omini_protocol::AttachmentRef {
    omini_protocol::AttachmentRef::LocalPath {
        path: image.source_path,
        mime_type: None,
        name: (!image.file_name.is_empty()).then_some(image.file_name),
    }
}

pub(crate) fn thinking_effort_from_internal(
    effort: config_types::ThinkingEffort,
) -> omini_protocol::ThinkingEffort {
    match effort {
        config_types::ThinkingEffort::None => omini_protocol::ThinkingEffort::None,
        config_types::ThinkingEffort::Low => omini_protocol::ThinkingEffort::Low,
        config_types::ThinkingEffort::Medium => omini_protocol::ThinkingEffort::Medium,
        config_types::ThinkingEffort::High => omini_protocol::ThinkingEffort::High,
        config_types::ThinkingEffort::XHigh => omini_protocol::ThinkingEffort::XHigh,
        config_types::ThinkingEffort::Max => omini_protocol::ThinkingEffort::Max,
    }
}

pub(crate) fn active_profile_from_internal(
    profile: event_types::ActiveProfile,
) -> omini_protocol::ActiveProfile {
    match profile {
        event_types::ActiveProfile::Main => omini_protocol::ActiveProfile::Main,
        event_types::ActiveProfile::Auto => omini_protocol::ActiveProfile::Auto,
        event_types::ActiveProfile::Plan => omini_protocol::ActiveProfile::Plan,
    }
}

pub(crate) fn tool_pause_response_from_internal(
    response: event_types::ToolPauseResponse,
) -> omini_protocol::ToolPauseResponse {
    match response {
        event_types::ToolPauseResponse::Permission { approved, note } => {
            omini_protocol::ToolPauseResponse::Permission { approved, note }
        }
        event_types::ToolPauseResponse::UserInput { value } => {
            omini_protocol::ToolPauseResponse::UserInput { value }
        }
        event_types::ToolPauseResponse::Cancelled => omini_protocol::ToolPauseResponse::Cancelled,
    }
}

pub(crate) fn plan_approval_action_from_internal(
    action: event_types::PlanApprovalAction,
) -> omini_protocol::PlanApprovalAction {
    match action {
        event_types::PlanApprovalAction::Approve { profile } => {
            omini_protocol::PlanApprovalAction::Approve {
                profile: plan_execution_profile_from_internal(profile),
            }
        }
        event_types::PlanApprovalAction::ApproveInNewSession { profile } => {
            omini_protocol::PlanApprovalAction::ApproveInNewSession {
                profile: plan_execution_profile_from_internal(profile),
            }
        }
        event_types::PlanApprovalAction::ContinueDiscussing => {
            omini_protocol::PlanApprovalAction::ContinueDiscussing
        }
    }
}

fn plan_execution_profile_from_internal(
    profile: event_types::PlanExecutionProfile,
) -> omini_protocol::PlanExecutionProfile {
    match profile {
        event_types::PlanExecutionProfile::Main => omini_protocol::PlanExecutionProfile::Main,
        event_types::PlanExecutionProfile::Auto => omini_protocol::PlanExecutionProfile::Auto,
    }
}

pub(crate) fn agent_source_kind_from_internal(
    source_kind: subagent_types::AgentSourceKind,
) -> omini_protocol::AgentSourceKind {
    match source_kind {
        subagent_types::AgentSourceKind::BuiltIn => omini_protocol::AgentSourceKind::BuiltIn,
        subagent_types::AgentSourceKind::Project => omini_protocol::AgentSourceKind::Project,
        subagent_types::AgentSourceKind::User => omini_protocol::AgentSourceKind::User,
    }
}

pub(crate) fn agent_draft_from_internal(
    draft: subagent_types::AgentDraft,
) -> omini_protocol::AgentDraft {
    omini_protocol::AgentDraft {
        name: draft.name,
        description: draft.description,
        short_description: draft.short_description,
        instructions: draft.instructions,
        tools: draft.tools,
        disallow_tools: draft.disallow_tools,
        model: draft.model,
    }
}
