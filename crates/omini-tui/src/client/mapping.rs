use omini_protocol as protocol;
use std::path::PathBuf;

pub fn thinking_effort_from_protocol(
    effort: protocol::ThinkingEffort,
) -> crate::client::catalog::ThinkingEffort {
    match effort {
        protocol::ThinkingEffort::None => crate::client::catalog::ThinkingEffort::None,
        protocol::ThinkingEffort::Low => crate::client::catalog::ThinkingEffort::Low,
        protocol::ThinkingEffort::Medium => crate::client::catalog::ThinkingEffort::Medium,
        protocol::ThinkingEffort::High => crate::client::catalog::ThinkingEffort::High,
        protocol::ThinkingEffort::XHigh => crate::client::catalog::ThinkingEffort::XHigh,
        protocol::ThinkingEffort::Max => crate::client::catalog::ThinkingEffort::Max,
    }
}

pub fn thread_summary_from_protocol(
    thread: protocol::ThreadSummary,
) -> crate::app::event::ThreadSummary {
    crate::app::event::ThreadSummary {
        id: thread.id,
        title: thread.title,
        model: thread.model,
        provider: thread.provider,
        created_at: thread.created_at,
        updated_at: thread.updated_at,
        runtime_state: thread.runtime_state.map(Into::into),
    }
}

pub fn agent_summary_from_protocol(
    agent: protocol::AgentSummary,
) -> omini_domain::subagents::AgentSummary {
    omini_domain::subagents::AgentSummary {
        name: agent.name,
        description: agent.description,
        short_description: agent.short_description,
        location: agent.location,
    }
}

pub fn skill_command_summary(skill: protocol::SkillSummary) -> crate::app::event::CommandSummary {
    let description = skill.short_description.clone().unwrap_or(skill.description);
    let args_description = skill.argument_hint.or_else(|| Some("[prompt]".to_string()));
    crate::app::event::CommandSummary {
        name: skill.name,
        aliases: Vec::new(),
        description,
        sort_weight: 500,
        kind: crate::app::event::CommandKind::Skill,
        has_args: true,
        args_description,
    }
}

pub fn event_types_thread_selection(
    threads: protocol::ThreadsResponse,
) -> crate::app::event::InteractionRequest {
    crate::app::event::InteractionRequest::ThreadSelection {
        threads: threads
            .threads
            .into_iter()
            .map(thread_summary_from_protocol)
            .collect(),
    }
}

pub fn event_types_model_selection(
    models: protocol::ModelsResponse,
) -> crate::app::event::InteractionRequest {
    crate::app::event::InteractionRequest::ModelSelection {
        providers: providers_from_protocol(models.providers),
        current_provider: models.current_provider,
        current_model: models.current_model,
    }
}

pub fn event_types_agent_management(
    agents: protocol::AgentsResponse,
) -> crate::app::event::InteractionRequest {
    crate::app::event::InteractionRequest::AgentManagement {
        records: agent_records_from_protocol(agents.records),
        providers: providers_from_protocol(agents.providers),
        current_provider: agents.current_provider,
        current_model: agents.current_model,
    }
}

pub fn agent_records_from_protocol(
    records: Vec<protocol::AgentRecord>,
) -> Vec<omini_domain::subagents::AgentRecord> {
    records
        .into_iter()
        .map(agent_record_from_protocol)
        .collect()
}

pub fn providers_from_protocol(
    providers: Vec<protocol::ProviderInfo>,
) -> std::collections::HashMap<String, crate::client::catalog::ProviderProfile> {
    providers
        .into_iter()
        .map(|provider| {
            (
                provider.id,
                crate::client::catalog::ProviderProfile {
                    name: provider.name,
                    endpoint: provider_endpoint_from_protocol(provider.endpoint),
                    base_url: provider.base_url,
                    models: provider
                        .models
                        .into_iter()
                        .map(model_config_from_protocol)
                        .collect(),
                },
            )
        })
        .collect()
}

pub fn provider_endpoint_from_protocol(
    endpoint: protocol::ProviderEndpointKind,
) -> crate::client::catalog::ProviderType {
    match endpoint {
        protocol::ProviderEndpointKind::OpenAI => crate::client::catalog::ProviderType::OpenAI,
        protocol::ProviderEndpointKind::Anthropic => {
            crate::client::catalog::ProviderType::Anthropic
        }
    }
}

pub fn model_config_from_protocol(
    model: protocol::ModelInfo,
) -> crate::client::catalog::ModelConfig {
    crate::client::catalog::ModelConfig {
        id: model.id,
        name: model.name,
        limit: model.limit,
        thinking: model.thinking,
        input_modalities: model.input_modalities.map(|modalities| {
            modalities
                .into_iter()
                .map(input_modality_from_protocol)
                .collect()
        }),
        extra_headers: model.extra_headers,
        extra_body: model.extra_body.map(|body| body.into_iter().collect()),
    }
}

pub fn input_modality_from_protocol(
    modality: protocol::InputModality,
) -> crate::client::catalog::InputModality {
    match modality {
        protocol::InputModality::Text => crate::client::catalog::InputModality::Text,
        protocol::InputModality::Image => crate::client::catalog::InputModality::Image,
    }
}

pub fn agent_record_from_protocol(
    record: protocol::AgentRecord,
) -> omini_domain::subagents::AgentRecord {
    omini_domain::subagents::AgentRecord {
        name: record.name,
        description: record.description,
        short_description: record.short_description,
        instructions: record.instructions,
        tools: record.tools,
        disallow_tools: record.disallow_tools,
        model: record.model,
        source_kind: agent_source_kind_from_protocol(record.source_kind),
        path: record.editable.then(|| PathBuf::from(record.id)),
        editable: record.editable,
    }
}

pub fn agent_source_kind_from_protocol(
    source_kind: protocol::AgentSourceKind,
) -> omini_domain::subagents::AgentSourceKind {
    match source_kind {
        protocol::AgentSourceKind::BuiltIn => omini_domain::subagents::AgentSourceKind::BuiltIn,
        protocol::AgentSourceKind::Project => omini_domain::subagents::AgentSourceKind::Project,
        protocol::AgentSourceKind::User => omini_domain::subagents::AgentSourceKind::User,
    }
}

pub fn generated_agent_draft_from_protocol(
    draft: protocol::GeneratedAgentDraft,
    tools: Vec<String>,
    disallow_tools: Vec<String>,
    model: Option<String>,
) -> omini_domain::subagents::AgentDraft {
    omini_domain::subagents::AgentDraft {
        name: draft.name,
        description: draft.description,
        short_description: draft.short_description,
        instructions: draft.instructions,
        tools,
        disallow_tools,
        model,
    }
}
