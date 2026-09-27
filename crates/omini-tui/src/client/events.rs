use crate::app::event::{
    CompactEvent, CompactSummaryDeltaEvent, CompactSummaryFailedEvent, CompactSummaryFinishedEvent,
    Notification, NotificationKind, RuntimeToUiEvent,
};
use omini_protocol as protocol;

pub fn runtime_event_from_protocol(event: protocol::RuntimeEvent) -> RuntimeToUiEvent {
    match event.event {
        protocol::TypedRuntimeEvent::AgentRunChanged(run) => {
            RuntimeToUiEvent::AgentRunChanged(run.into())
        }
        protocol::TypedRuntimeEvent::RunStarted => RuntimeToUiEvent::RunStarted,
        protocol::TypedRuntimeEvent::UserMessageInjected {
            item,
            client_echo_id,
        } => RuntimeToUiEvent::UserMessageInjected {
            item,
            client_echo_id,
        },
        protocol::TypedRuntimeEvent::AgentTaskUserMessageQueued {
            task_id,
            thread_id,
            item,
            client_echo_id,
            ..
        } => RuntimeToUiEvent::AgentTaskUserMessageQueued {
            task_id,
            thread_id,
            item,
            client_echo_id,
        },
        protocol::TypedRuntimeEvent::AgentTaskMessageQueued { task_id, item, .. } => {
            RuntimeToUiEvent::AgentTaskMessageQueued { task_id, item }
        }
        protocol::TypedRuntimeEvent::RunFinished => RuntimeToUiEvent::RunFinished,
        protocol::TypedRuntimeEvent::Notification(event) => {
            RuntimeToUiEvent::Notification(Notification {
                kind: notification_kind_from_protocol(event.level),
                message: event.message,
                details: event.details,
            })
        }
        protocol::TypedRuntimeEvent::ModelChanged(event) => RuntimeToUiEvent::ModelChanged {
            provider: event.provider,
            model: event.model,
            thinking_effort: event.thinking_effort.map(Into::into),
            context_window: event.context_window,
        },
        protocol::TypedRuntimeEvent::UsageChanged(usage) => RuntimeToUiEvent::UsageChanged(usage),
        protocol::TypedRuntimeEvent::UsageTotalsChanged(event) => {
            RuntimeToUiEvent::UsageTotalsChanged {
                total_tokens: event.total_tokens,
                total_cached_tokens: event.total_cached_tokens,
            }
        }
        protocol::TypedRuntimeEvent::ActiveProfileChanged(event) => {
            RuntimeToUiEvent::ActiveProfileChanged(event.profile.into())
        }
        protocol::TypedRuntimeEvent::ThreadTitleChanged(event) => {
            RuntimeToUiEvent::ThreadTitleChanged { title: event.title }
        }
        protocol::TypedRuntimeEvent::ToolPauseRequested(request) => {
            RuntimeToUiEvent::ToolPauseRequested(request.into())
        }
        protocol::TypedRuntimeEvent::PlanSubmitted(plan) => RuntimeToUiEvent::PlanSubmitted(plan),
        protocol::TypedRuntimeEvent::PlanApprovalResolved(event) => {
            RuntimeToUiEvent::PlanApprovalResolved {
                plan_id: event.plan_id,
                action: event.action.into(),
            }
        }
        protocol::TypedRuntimeEvent::AgentManagementUpdated { records } => {
            RuntimeToUiEvent::AgentManagementUpdated { records }
        }
        protocol::TypedRuntimeEvent::TurnStarted => RuntimeToUiEvent::TurnStarted,
        protocol::TypedRuntimeEvent::TurnEnded => RuntimeToUiEvent::TurnEnded,
        protocol::TypedRuntimeEvent::GitBranchChanged(event) => {
            RuntimeToUiEvent::GitBranchChanged {
                branch: event.branch,
            }
        }
        protocol::TypedRuntimeEvent::ThinkingDelta(event) => {
            RuntimeToUiEvent::ThinkingDelta(event.delta)
        }
        protocol::TypedRuntimeEvent::TextDelta(event) => RuntimeToUiEvent::TextDelta(event.delta),
        protocol::TypedRuntimeEvent::ProposedPlanDelta(event) => {
            RuntimeToUiEvent::ProposedPlanDelta(event.delta)
        }
        protocol::TypedRuntimeEvent::ToolUse(tool_use) => RuntimeToUiEvent::ToolUse(tool_use),
        protocol::TypedRuntimeEvent::ToolResult(tool_result) => {
            RuntimeToUiEvent::ToolResult(tool_result)
        }
        protocol::TypedRuntimeEvent::TaskChanged(task) => RuntimeToUiEvent::TaskChanged(task),
        protocol::TypedRuntimeEvent::TaskOutputDelta(output) => {
            RuntimeToUiEvent::TaskOutputDelta(output)
        }
        protocol::TypedRuntimeEvent::CompactSummaryStarted(event) => {
            RuntimeToUiEvent::CompactSummaryStarted(CompactEvent {
                trigger: event.trigger,
                thread_id: event.thread_id,
                agent_label: event.agent_label,
            })
        }
        protocol::TypedRuntimeEvent::CompactSummaryDelta(event) => {
            RuntimeToUiEvent::CompactSummaryDelta(CompactSummaryDeltaEvent {
                trigger: event.trigger,
                delta: event.delta,
                thread_id: event.thread_id,
                agent_label: event.agent_label,
            })
        }
        protocol::TypedRuntimeEvent::CompactSummaryFinished(event) => {
            RuntimeToUiEvent::CompactSummaryFinished(CompactSummaryFinishedEvent {
                trigger: event.trigger,
                summary: event.summary,
                after_tokens: event.after_tokens,
                thread_id: event.thread_id,
                agent_label: event.agent_label,
            })
        }
        protocol::TypedRuntimeEvent::CompactSummaryFailed(event) => {
            RuntimeToUiEvent::CompactSummaryFailed(CompactSummaryFailedEvent {
                trigger: event.trigger,
                message: event.message,
                thread_id: event.thread_id,
                agent_label: event.agent_label,
            })
        }
        protocol::TypedRuntimeEvent::ThreadSnapshot(event) => RuntimeToUiEvent::ThreadSnapshot {
            thread_id: Some(event.thread_id),
            messages: event.messages,
            agent_tasks: event.agent_tasks,
            usage: event.usage,
        },
        protocol::TypedRuntimeEvent::AgentTaskEvent(event) => {
            RuntimeToUiEvent::AgentTaskEvent(event)
        }
        // ThreadSwitched 在 `handle_server_text` 拦截,不会流到这里。
        protocol::TypedRuntimeEvent::ThreadSwitched(_) => unreachable!(
            "ThreadSwitched 事件应被 handle_server_text 提前拦截为 HandleOutcome::Switch"
        ),
    }
}

pub fn notification_kind_from_protocol(level: protocol::NotificationLevel) -> NotificationKind {
    match level {
        protocol::NotificationLevel::Info => NotificationKind::Info,
        protocol::NotificationLevel::Warn => NotificationKind::Warn,
        protocol::NotificationLevel::Error => NotificationKind::Error,
    }
}
