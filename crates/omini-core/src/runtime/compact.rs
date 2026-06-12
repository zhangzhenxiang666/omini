use crate::error::{CompactError, RuntimeError};
use crate::tools::ToolRuntimeContext;
use crate::types::events::EngineToRuntimeEvent;
use omini_config::project::SessionDir;
use omini_config::{CompactConfig, Settings};
use omini_domain::events::{
    CompactEvent, CompactShrinkFinishedEvent, CompactSummaryDeltaEvent, CompactSummaryFailedEvent,
    CompactSummaryFinishedEvent, CompactTrigger,
};
use omini_domain::message::{ContentBlock, Message, Role, TextBlock, ToolResultBlock};
use omini_domain::tool::ToolDefinition;
use omini_provider_api::{ApiEvent, ApiRequest, LlmClient};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;

const DEFAULT_CONTEXT_WINDOW: usize = 256_000;
const SOFT_COMPACT_USAGE_PERCENT: usize = 80;
const HARD_COMPACT_USAGE_PERCENT: usize = 85;
const TOKEN_ESTIMATION_PADDING_NUMERATOR: usize = 4;
const TOKEN_ESTIMATION_PADDING_DENOMINATOR: usize = 3;
const IMAGE_TOKEN_ESTIMATE: usize = 3_072;
const TIME_BASED_MC_CLEARED_MESSAGE: &str = "[Old tool result content cleared]";
const CONTEXT_COLLAPSE_TEXT_CHAR_LIMIT: usize = 2_400;
const CONTEXT_COLLAPSE_HEAD_CHARS: usize = 900;
const CONTEXT_COLLAPSE_TAIL_CHARS: usize = 500;
const MAX_COMPACT_STREAMING_RETRIES: usize = 2;
const MAX_PTL_RETRIES: usize = 3;
const PTL_RETRY_MARKER: &str = "[earlier conversation truncated for compaction retry]";

#[derive(Debug, Default)]
pub struct AutoCompactState {
    pub consecutive_failures: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct CompactOutcome {
    pub before_tokens: usize,
    pub after_tokens: usize,
    pub before_messages: usize,
    pub after_messages: usize,
}

struct CompactRequestContext<'a> {
    settings: &'a Settings,
    llm_client: &'a LlmClient,
    tool_definitions: &'a [ToolDefinition],
    runtime_context: Option<&'a ToolRuntimeContext>,
    event_tx: &'a mpsc::Sender<EngineToRuntimeEvent>,
    trigger: CompactTrigger,
    custom_instructions: Option<&'a str>,
}

struct CollectedSummary {
    raw: String,
    visible: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AutoCompactThresholds {
    soft: usize,
    hard: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoCompactDecision {
    Skip,
    LocalOnly,
    FullIfLocalInsufficient,
}

pub async fn auto_compact_if_needed(
    messages: &mut Vec<Message>,
    settings: &Settings,
    llm_client: &LlmClient,
    tool_definitions: &[ToolDefinition],
    runtime_context: Option<Arc<ToolRuntimeContext>>,
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    state: &mut AutoCompactState,
) -> bool {
    let config = normalized_config(&settings.compact);
    if !config.enabled || state.consecutive_failures >= config.max_consecutive_failures {
        tracing::debug!(
            compact_trigger = %CompactTrigger::Auto,
            enabled = config.enabled,
            consecutive_failures = state.consecutive_failures,
            max_consecutive_failures = config.max_consecutive_failures,
            "auto compact skipped by config or failure budget"
        );
        return false;
    }
    // TODO(compact): 等 parent/subagent 的 UI 展示和 history 语义确定后，
    // 再重新启用 subagent 自动压缩。
    if is_subagent_session_type(
        runtime_context
            .as_ref()
            .map(|runtime| runtime.session_type.as_str()),
    ) {
        tracing::debug!(
            compact_trigger = %CompactTrigger::Auto,
            session_id = ?compact_session_id(runtime_context.as_deref()),
            session_type = ?compact_session_type(runtime_context.as_deref()),
            agent_label = ?compact_agent_label(runtime_context.as_deref()),
            "auto compact skipped for subagent session"
        );
        return false;
    }

    let before_tokens = estimate_request_tokens(
        messages,
        settings.system_prompt.as_deref(),
        tool_definitions,
    );
    let thresholds = auto_compact_thresholds(settings);
    let decision = auto_compact_decision(before_tokens, thresholds);
    tracing::debug!(
        compact_trigger = %CompactTrigger::Auto,
        session_id = ?compact_session_id(runtime_context.as_deref()),
        session_type = ?compact_session_type(runtime_context.as_deref()),
        agent_label = ?compact_agent_label(runtime_context.as_deref()),
        before_tokens,
        before_messages = messages.len(),
        soft_threshold = thresholds.soft,
        hard_threshold = thresholds.hard,
        decision = ?decision,
        consecutive_failures = state.consecutive_failures,
        "auto compact evaluated"
    );
    if decision == AutoCompactDecision::Skip {
        return false;
    }

    let event = compact_event(CompactTrigger::Auto, runtime_context.as_deref());
    let _ = event_tx
        .send(EngineToRuntimeEvent::CompactShrinkStarted(event))
        .await;

    let before_messages = messages.len();
    let mut changed = false;
    let microcompacted = microcompact_messages(messages, 5);
    if microcompacted > 0 {
        changed = true;
    }

    let after_micro = estimate_request_tokens(
        messages,
        settings.system_prompt.as_deref(),
        tool_definitions,
    );
    tracing::debug!(
        compact_trigger = %CompactTrigger::Auto,
        microcompacted_tool_results = microcompacted,
        after_micro_tokens = after_micro,
        before_tokens,
        "auto compact micro pass finished"
    );
    if changed && after_micro < thresholds.soft {
        rewrite_runtime_history(runtime_context.as_deref(), messages);
        let outcome = CompactOutcome {
            before_tokens,
            after_tokens: after_micro,
            before_messages,
            after_messages: messages.len(),
        };
        emit_compact_shrink_finished(
            event_tx,
            CompactTrigger::Auto,
            runtime_context.as_deref(),
            outcome,
        )
        .await;
        state.consecutive_failures = 0;
        tracing::debug!(
            compact_trigger = %CompactTrigger::Auto,
            before_tokens = outcome.before_tokens,
            after_tokens = outcome.after_tokens,
            before_messages = outcome.before_messages,
            after_messages = outcome.after_messages,
            "auto compact completed with local micro pass"
        );
        return true;
    }

    if let Some(collapsed) = try_context_collapse(messages, config.preserve_recent) {
        *messages = collapsed;
        changed = true;
        tracing::debug!(
            compact_trigger = %CompactTrigger::Auto,
            preserve_recent = config.preserve_recent,
            "auto compact context collapse applied"
        );
    }

    let after_collapse = estimate_request_tokens(
        messages,
        settings.system_prompt.as_deref(),
        tool_definitions,
    );
    tracing::debug!(
        compact_trigger = %CompactTrigger::Auto,
        after_collapse_tokens = after_collapse,
        before_tokens,
        changed,
        "auto compact collapse pass finished"
    );
    if changed && after_collapse < thresholds.soft {
        rewrite_runtime_history(runtime_context.as_deref(), messages);
        let outcome = CompactOutcome {
            before_tokens,
            after_tokens: after_collapse,
            before_messages,
            after_messages: messages.len(),
        };
        emit_compact_shrink_finished(
            event_tx,
            CompactTrigger::Auto,
            runtime_context.as_deref(),
            outcome,
        )
        .await;
        state.consecutive_failures = 0;
        tracing::debug!(
            compact_trigger = %CompactTrigger::Auto,
            before_tokens = outcome.before_tokens,
            after_tokens = outcome.after_tokens,
            before_messages = outcome.before_messages,
            after_messages = outcome.after_messages,
            "auto compact completed with context collapse"
        );
        return true;
    }

    if decision == AutoCompactDecision::LocalOnly {
        if changed {
            rewrite_runtime_history(runtime_context.as_deref(), messages);
            let outcome = CompactOutcome {
                before_tokens,
                after_tokens: after_collapse,
                before_messages,
                after_messages: messages.len(),
            };
            emit_compact_shrink_finished(
                event_tx,
                CompactTrigger::Auto,
                runtime_context.as_deref(),
                outcome,
            )
            .await;
            state.consecutive_failures = 0;
            tracing::debug!(
                compact_trigger = %CompactTrigger::Auto,
                before_tokens = outcome.before_tokens,
                after_tokens = outcome.after_tokens,
                before_messages = outcome.before_messages,
                after_messages = outcome.after_messages,
                "auto compact completed with local-only shrink"
            );
        }
        return changed;
    }

    let compact_context = CompactRequestContext {
        settings,
        llm_client,
        tool_definitions,
        runtime_context: runtime_context.as_deref(),
        event_tx,
        trigger: CompactTrigger::Auto,
        custom_instructions: None,
    };
    match full_compact(messages, compact_context).await {
        Ok(outcome) => {
            rewrite_runtime_history(runtime_context.as_deref(), messages);
            emit_compact_shrink_finished(
                event_tx,
                CompactTrigger::Auto,
                runtime_context.as_deref(),
                outcome,
            )
            .await;
            state.consecutive_failures = 0;
            tracing::debug!(
                compact_trigger = %CompactTrigger::Auto,
                before_tokens = outcome.before_tokens,
                after_tokens = outcome.after_tokens,
                before_messages = outcome.before_messages,
                after_messages = outcome.after_messages,
                "auto compact completed with summary"
            );
            true
        }
        Err(error) => {
            state.consecutive_failures += 1;
            tracing::warn!(
                compact_trigger = %CompactTrigger::Auto,
                error = %error,
                consecutive_failures = state.consecutive_failures,
                "auto compact failed"
            );
            emit_compact_summary_failed(
                event_tx,
                CompactTrigger::Auto,
                runtime_context.as_deref(),
                error.to_string(),
            )
            .await;
            changed
        }
    }
}

pub async fn force_compact(
    messages: &mut Vec<Message>,
    settings: &Settings,
    llm_client: &LlmClient,
    tool_definitions: &[ToolDefinition],
    custom_instructions: Option<&str>,
    runtime_context: Option<Arc<ToolRuntimeContext>>,
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
) -> Result<CompactOutcome, RuntimeError> {
    tracing::debug!(
        compact_trigger = %CompactTrigger::Manual,
        session_id = ?compact_session_id(runtime_context.as_deref()),
        session_type = ?compact_session_type(runtime_context.as_deref()),
        agent_label = ?compact_agent_label(runtime_context.as_deref()),
        message_count = messages.len(),
        has_custom_instructions = custom_instructions.is_some(),
        "manual compact started"
    );
    let _ = event_tx
        .send(EngineToRuntimeEvent::CompactShrinkStarted(compact_event(
            CompactTrigger::Manual,
            runtime_context.as_deref(),
        )))
        .await;

    let compact_context = CompactRequestContext {
        settings,
        llm_client,
        tool_definitions,
        runtime_context: runtime_context.as_deref(),
        event_tx,
        trigger: CompactTrigger::Manual,
        custom_instructions,
    };
    match full_compact(messages, compact_context).await {
        Ok(outcome) => {
            rewrite_runtime_history(runtime_context.as_deref(), messages);
            emit_compact_shrink_finished(
                event_tx,
                CompactTrigger::Manual,
                runtime_context.as_deref(),
                outcome,
            )
            .await;
            tracing::debug!(
                compact_trigger = %CompactTrigger::Manual,
                before_tokens = outcome.before_tokens,
                after_tokens = outcome.after_tokens,
                before_messages = outcome.before_messages,
                after_messages = outcome.after_messages,
                "manual compact completed"
            );
            Ok(outcome)
        }
        Err(error) => {
            tracing::warn!(
                compact_trigger = %CompactTrigger::Manual,
                error = %error,
                "manual compact failed"
            );
            emit_compact_summary_failed(
                event_tx,
                CompactTrigger::Manual,
                runtime_context.as_deref(),
                error.to_string(),
            )
            .await;
            Err(error.into())
        }
    }
}

pub fn estimate_request_tokens(
    messages: &[Message],
    system_prompt: Option<&str>,
    tools: &[ToolDefinition],
) -> usize {
    let mut total = 0;
    if let Some(system_prompt) = system_prompt {
        total += estimate_text_tokens(system_prompt);
    }
    for tool in tools {
        total += estimate_text_tokens(&tool.name);
        total += estimate_text_tokens(&tool.description);
        total += estimate_value_tokens(&tool.input_schema);
    }
    total += estimate_message_tokens_unpadded(messages);
    padded(total)
}

fn estimate_message_tokens_unpadded(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| {
            let role_overhead = 4 + estimate_text_tokens(&message.role.to_string());
            role_overhead
                + message
                    .content
                    .iter()
                    .map(estimate_block_tokens_unpadded)
                    .sum::<usize>()
        })
        .sum()
}

fn estimate_block_tokens_unpadded(block: &ContentBlock) -> usize {
    match block {
        ContentBlock::Thinking(block) => estimate_text_tokens(&block.thinking),
        ContentBlock::Text(block) => estimate_text_tokens(&block.text),
        ContentBlock::Image(_) => IMAGE_TOKEN_ESTIMATE,
        ContentBlock::ToolUse(block) => {
            estimate_text_tokens(&block.name)
                + estimate_value_tokens(&serde_json::json!(block.input))
        }
        ContentBlock::ToolResult(block) => estimate_text_tokens(&block.content),
    }
}

fn estimate_text_tokens(text: &str) -> usize {
    let mut ascii_chars = 0usize;
    let mut non_ascii_chars = 0usize;
    for ch in text.chars() {
        if ch.is_ascii() {
            ascii_chars += 1;
        } else {
            non_ascii_chars += 1;
        }
    }
    ascii_chars.div_ceil(4) + non_ascii_chars.div_ceil(2) + 1
}

fn estimate_value_tokens(value: &Value) -> usize {
    estimate_text_tokens(&value.to_string())
}

fn padded(tokens: usize) -> usize {
    tokens
        .saturating_mul(TOKEN_ESTIMATION_PADDING_NUMERATOR)
        .div_ceil(TOKEN_ESTIMATION_PADDING_DENOMINATOR)
}

fn normalized_config(config: &CompactConfig) -> CompactConfig {
    CompactConfig {
        enabled: config.enabled,
        preserve_recent: config.preserve_recent.max(1),
        buffer_tokens: config.buffer_tokens,
        summary_output_tokens: config.summary_output_tokens.max(1),
        max_consecutive_failures: config.max_consecutive_failures.max(1),
    }
}

fn auto_compact_thresholds(settings: &Settings) -> AutoCompactThresholds {
    let config = normalized_config(&settings.compact);
    let context_window = context_window(settings);
    let reserve_threshold = context_window
        .saturating_sub(config.summary_output_tokens)
        .saturating_sub(config.buffer_tokens);
    let soft =
        usage_percent_threshold(context_window, SOFT_COMPACT_USAGE_PERCENT).min(reserve_threshold);
    let hard =
        usage_percent_threshold(context_window, HARD_COMPACT_USAGE_PERCENT).min(reserve_threshold);
    AutoCompactThresholds { soft, hard }
}

fn usage_percent_threshold(context_window: usize, percent: usize) -> usize {
    context_window.saturating_mul(percent) / 100
}

fn auto_compact_decision(tokens: usize, thresholds: AutoCompactThresholds) -> AutoCompactDecision {
    if tokens < thresholds.soft {
        AutoCompactDecision::Skip
    } else if tokens < thresholds.hard {
        AutoCompactDecision::LocalOnly
    } else {
        AutoCompactDecision::FullIfLocalInsufficient
    }
}

fn context_window(settings: &Settings) -> usize {
    settings
        .providers
        .get(&settings.active_provider)
        .and_then(|provider| {
            provider
                .models
                .iter()
                .find(|model| model.id == settings.model)
                .map(|model| model.limit as usize)
        })
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

fn is_subagent_session_type(session_type: Option<&str>) -> bool {
    session_type == Some("subagent")
}

fn microcompact_messages(messages: &mut [Message], keep_recent: usize) -> usize {
    let compactable_ids = collect_compactable_tool_ids(messages);
    if compactable_ids.len() <= keep_recent.max(1) {
        return 0;
    }

    let keep_set = compactable_ids
        .iter()
        .rev()
        .take(keep_recent.max(1))
        .cloned()
        .collect::<HashSet<_>>();
    let mut tokens_saved = 0;
    for message in messages {
        if message.role != Role::User {
            continue;
        }
        for block in &mut message.content {
            if let ContentBlock::ToolResult(result) = block
                && !keep_set.contains(&result.tool_use_id)
                && result.content != TIME_BASED_MC_CLEARED_MESSAGE
            {
                tokens_saved += estimate_text_tokens(&result.content);
                result.content = TIME_BASED_MC_CLEARED_MESSAGE.to_string();
            }
        }
    }
    tokens_saved
}

fn collect_compactable_tool_ids(messages: &[Message]) -> Vec<String> {
    let mut ordered_ids = Vec::new();
    for message in messages {
        for block in &message.content {
            if let ContentBlock::ToolUse(tool_use) = block
                && is_compactable_tool(&tool_use.name)
            {
                ordered_ids.push(tool_use.id.clone());
            }
        }
    }
    ordered_ids
}

fn is_compactable_tool(name: &str) -> bool {
    matches!(name, "read" | "bash" | "search" | "subagent" | "skill")
}

fn try_context_collapse(messages: &[Message], preserve_recent: usize) -> Option<Vec<Message>> {
    if messages.len() <= preserve_recent + 2 {
        return None;
    }
    let (older, newer) = split_preserving_tool_pairs(messages, preserve_recent);
    let mut changed = false;
    let mut collapsed = Vec::with_capacity(messages.len());
    for message in older {
        let mut content = Vec::with_capacity(message.content.len());
        for block in &message.content {
            match block {
                ContentBlock::Text(text) => {
                    let next = collapse_text(&text.text);
                    changed |= next != text.text;
                    content.push(ContentBlock::Text(TextBlock { text: next }));
                }
                ContentBlock::ToolResult(result) => {
                    let next = collapse_text(&result.content);
                    changed |= next != result.content;
                    content.push(ContentBlock::ToolResult(ToolResultBlock {
                        tool_use_id: result.tool_use_id.clone(),
                        is_error: result.is_error,
                        content: next,
                        metadata: result.metadata.clone(),
                    }));
                }
                other => content.push(other.clone()),
            }
        }
        collapsed.push(Message::new(message.role.clone(), content));
    }
    collapsed.extend(newer);
    changed.then_some(collapsed)
}

fn collapse_text(text: &str) -> String {
    if text.chars().count() <= CONTEXT_COLLAPSE_TEXT_CHAR_LIMIT {
        return text.to_string();
    }
    let head = text
        .chars()
        .take(CONTEXT_COLLAPSE_HEAD_CHARS)
        .collect::<String>()
        .trim_end()
        .to_string();
    let tail_chars = text
        .chars()
        .rev()
        .take(CONTEXT_COLLAPSE_TAIL_CHARS)
        .collect::<Vec<_>>();
    let tail = tail_chars
        .into_iter()
        .rev()
        .collect::<String>()
        .trim_start()
        .to_string();
    let omitted = text
        .chars()
        .count()
        .saturating_sub(CONTEXT_COLLAPSE_HEAD_CHARS + CONTEXT_COLLAPSE_TAIL_CHARS);
    format!("{head}\n...[collapsed {omitted} chars]...\n{tail}")
}

async fn full_compact(
    messages: &mut Vec<Message>,
    ctx: CompactRequestContext<'_>,
) -> Result<CompactOutcome, CompactError> {
    let config = normalized_config(&ctx.settings.compact);
    let before_messages = messages.len();
    let before_tokens = estimate_request_tokens(
        messages,
        ctx.settings.system_prompt.as_deref(),
        ctx.tool_definitions,
    );
    tracing::debug!(
        compact_trigger = %ctx.trigger,
        session_id = ?compact_session_id(ctx.runtime_context),
        session_type = ?compact_session_type(ctx.runtime_context),
        agent_label = ?compact_agent_label(ctx.runtime_context),
        before_tokens,
        before_messages,
        preserve_recent = config.preserve_recent,
        summary_output_tokens = config.summary_output_tokens,
        "full compact evaluated"
    );
    if messages.len() <= config.preserve_recent {
        tracing::debug!(
            compact_trigger = %ctx.trigger,
            before_messages,
            preserve_recent = config.preserve_recent,
            "full compact skipped because history is within preserve_recent"
        );
        return Ok(CompactOutcome {
            before_tokens,
            after_tokens: before_tokens,
            before_messages,
            after_messages: before_messages,
        });
    }

    let mut compact_input = messages.clone();
    microcompact_messages(&mut compact_input, 5);
    let (older, newer) = split_preserving_tool_pairs(&compact_input, config.preserve_recent);
    if older.is_empty() {
        tracing::debug!(
            compact_trigger = %ctx.trigger,
            newer_messages = newer.len(),
            preserve_recent = config.preserve_recent,
            "full compact skipped because no older messages are compactable"
        );
        return Ok(CompactOutcome {
            before_tokens,
            after_tokens: before_tokens,
            before_messages,
            after_messages: before_messages,
        });
    }

    tracing::debug!(
        compact_trigger = %ctx.trigger,
        older_messages = older.len(),
        newer_messages = newer.len(),
        "full compact summary request starting"
    );
    emit_compact_summary_started(ctx.event_tx, ctx.trigger, ctx.runtime_context).await;
    let summary = collect_summary(older, config.summary_output_tokens, &ctx).await?;
    tracing::debug!(
        compact_trigger = %ctx.trigger,
        raw_summary_chars = summary.raw.chars().count(),
        visible_summary_chars = summary.visible.chars().count(),
        "full compact summary collected"
    );
    let summary_message = Message::from_user_text(build_compact_summary_message(
        &summary.raw,
        !newer.is_empty(),
        ctx.trigger,
    ));
    let metadata_message =
        compact_boundary_message(ctx.trigger, before_messages, before_tokens, newer.len() + 2);
    let mut compacted = vec![metadata_message, summary_message];
    compacted.extend(newer);
    compacted = sanitize_messages(compacted);
    let after_tokens = estimate_request_tokens(
        &compacted,
        ctx.settings.system_prompt.as_deref(),
        ctx.tool_definitions,
    );
    let after_messages = compacted.len();
    *messages = compacted;
    emit_compact_summary_finished(
        ctx.event_tx,
        ctx.trigger,
        ctx.runtime_context,
        summary.visible,
        after_tokens,
    )
    .await;
    tracing::debug!(
        compact_trigger = %ctx.trigger,
        before_tokens,
        after_tokens,
        before_messages,
        after_messages,
        "full compact finished"
    );

    Ok(CompactOutcome {
        before_tokens,
        after_tokens,
        before_messages,
        after_messages,
    })
}

async fn collect_summary(
    older: Vec<Message>,
    max_tokens: usize,
    ctx: &CompactRequestContext<'_>,
) -> Result<CollectedSummary, CompactError> {
    let compact_prompt = get_compact_prompt(ctx.custom_instructions);
    let mut retry_messages = replace_images_with_placeholders(&older);
    retry_messages.push(Message::from_user_text(compact_prompt));
    let mut ptl_retries = 0;
    let mut last_error = None;

    for attempt in 0..=MAX_COMPACT_STREAMING_RETRIES {
        tracing::debug!(
            compact_trigger = %ctx.trigger,
            attempt,
            ptl_retries,
            retry_message_count = retry_messages.len(),
            max_tokens,
            "compact summary attempt started"
        );
        match invoke_summary(&retry_messages, max_tokens, ctx).await {
            Ok(summary) if !summary.trim().is_empty() => {
                let visible = visible_compact_summary(&summary);
                tracing::debug!(
                    compact_trigger = %ctx.trigger,
                    attempt,
                    raw_summary_chars = summary.chars().count(),
                    visible_summary_chars = visible.chars().count(),
                    "compact summary attempt succeeded"
                );
                return Ok(CollectedSummary {
                    raw: summary,
                    visible,
                });
            }
            Ok(_) => {
                tracing::warn!(
                    compact_trigger = %ctx.trigger,
                    attempt,
                    "compact summary attempt returned empty response"
                );
                last_error = Some(CompactError::IncompleteResponse)
            }
            Err(error)
                if is_prompt_too_long_error(&error.to_string())
                    && ptl_retries < MAX_PTL_RETRIES =>
            {
                tracing::warn!(
                    compact_trigger = %ctx.trigger,
                    attempt,
                    ptl_retries,
                    error = %error,
                    "compact summary prompt too long; retrying with truncated history"
                );
                let Some(truncated) =
                    truncate_head_for_ptl_retry(&retry_messages[..retry_messages.len() - 1])
                else {
                    last_error = Some(error);
                    continue;
                };
                ptl_retries += 1;
                retry_messages = truncated;
                retry_messages.push(Message::from_user_text(get_compact_prompt(
                    ctx.custom_instructions,
                )));
                continue;
            }
            Err(error) => {
                tracing::warn!(
                    compact_trigger = %ctx.trigger,
                    attempt,
                    error = %error,
                    "compact summary attempt failed"
                );
                last_error = Some(error)
            }
        }
        if attempt == MAX_COMPACT_STREAMING_RETRIES {
            break;
        }
    }

    Err(last_error.unwrap_or(CompactError::IncompleteResponse))
}

async fn invoke_summary(
    messages: &[Message],
    max_tokens: usize,
    ctx: &CompactRequestContext<'_>,
) -> Result<String, CompactError> {
    let request = ApiRequest {
        messages,
        model: &ctx.settings.model,
        system_prompt: Some("You are a conversation summarizer."),
        tools: Some(&[]),
        max_tokens: Some(max_tokens as u64),
        temperature: None,
        thinking_effort: None,
    };
    let mut stream = ctx
        .llm_client
        .invoke(request)
        .await
        .map_err(CompactError::Request)?;
    tracing::debug!(
        compact_trigger = %ctx.trigger,
        request_messages = messages.len(),
        max_tokens,
        model = %ctx.settings.model,
        provider = %ctx.settings.active_provider,
        "compact summary stream opened"
    );
    let mut summary = String::new();
    let mut forwarder = CompactSummaryForwarder::new();
    while let Some(event) = stream.next().await {
        match event.map_err(CompactError::Stream)? {
            ApiEvent::Text(delta) => {
                summary.push_str(&delta);
                forward_compact_summary_deltas(
                    ctx.event_tx,
                    ctx.trigger,
                    ctx.runtime_context,
                    forwarder.push_str(&delta),
                )
                .await;
            }
            ApiEvent::Done(completion) => {
                tracing::debug!(
                    compact_trigger = %ctx.trigger,
                    finish_reason = ?completion.finish_reason,
                    prompt_tokens = completion.usage.prompt_tokens,
                    completion_tokens = completion.usage.completion_tokens,
                    cached_tokens = completion.usage.cached_tokens,
                    "compact summary stream completed"
                );
                let _ = ctx
                    .event_tx
                    .send(EngineToRuntimeEvent::CompactSummaryUsageRecorded(
                        completion.usage,
                    ))
                    .await;
                let completed = message_text(&completion.message);
                if !completed.trim().is_empty() {
                    summary = completed;
                }
            }
            ApiEvent::Thinking(_) | ApiEvent::ToolUse(_) => {}
        }
    }
    forward_compact_summary_deltas(
        ctx.event_tx,
        ctx.trigger,
        ctx.runtime_context,
        forwarder.finish(),
    )
    .await;
    if summary.trim().is_empty() {
        tracing::warn!(
            compact_trigger = %ctx.trigger,
            "compact summary stream ended without summary text"
        );
        Err(CompactError::IncompleteResponse)
    } else {
        tracing::debug!(
            compact_trigger = %ctx.trigger,
            summary_chars = summary.chars().count(),
            "compact summary stream finished"
        );
        Ok(summary)
    }
}

fn get_compact_prompt(custom_instructions: Option<&str>) -> String {
    let prompt = r#"CRITICAL: Respond with TEXT ONLY. Do NOT call any tools.

- Do NOT use read, bash, search, edit, write, subagent, skill, or ANY other tool.
- You already have all the context you need in the conversation above.
- Tool calls will be rejected and will waste your only turn.
- Your entire response must be plain text: an <analysis> block followed by a <summary> block.

Your task is to create a detailed summary of the conversation so far. This summary will replace the earlier messages, so it must capture all important information.

First, draft your analysis inside <analysis> tags. Walk through the conversation chronologically and extract:
- Every user request and intent.
- The approach taken and technical decisions made.
- Specific code, files, commands, configurations, and line numbers where available.
- Errors encountered and how they were fixed.
- User feedback, corrections, constraints, permissions, and preferences.

Then, produce a structured summary inside <summary> tags with these sections:

1. **Primary Request and Intent**
2. **Key Technical Concepts**
3. **Files and Code Sections**
4. **Errors and Fixes**
5. **Problem Solving**
6. **All User Messages**
7. **Pending Tasks**
8. **Current Work**
9. **Optional Next Step**

REMINDER: Do NOT call any tools. Respond with plain text only."#;
    let mut prompt = prompt.to_string();
    if let Some(custom_instructions) = custom_instructions.map(str::trim)
        && !custom_instructions.is_empty()
    {
        prompt.push_str("\n\nAdditional user focus for this compaction:\n");
        prompt.push_str(custom_instructions);
        prompt.push_str(
            "\nPreserve details matching this focus even if they seem less important globally.",
        );
    }
    prompt
}

fn build_compact_summary_message(
    raw_summary: &str,
    recent_preserved: bool,
    trigger: CompactTrigger,
) -> String {
    let mut text = format!(
        "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\n{}",
        format_compact_summary(raw_summary)
    );
    if recent_preserved {
        text.push_str("\n\nRecent messages are preserved verbatim.");
    }
    if trigger == CompactTrigger::Auto {
        text.push_str("\nContinue the conversation from where it left off without asking the user any further questions. Resume directly without acknowledging the summary.");
    }
    text
}

struct CompactSummaryForwarder {
    buffer: String,
    in_summary: bool,
    done: bool,
}

impl CompactSummaryForwarder {
    fn new() -> Self {
        Self {
            buffer: String::new(),
            in_summary: false,
            done: false,
        }
    }

    fn push_str(&mut self, delta: &str) -> Vec<String> {
        if self.done {
            return Vec::new();
        }
        self.buffer.push_str(delta);
        let mut out = Vec::new();

        if !self.in_summary {
            let Some(start) = self.buffer.find("<summary>") else {
                return out;
            };
            let content_start = start + "<summary>".len();
            self.buffer.drain(..content_start);
            self.in_summary = true;
        }

        if let Some(end) = self.buffer.find("</summary>") {
            let delta = self.buffer[..end].to_string();
            if !delta.is_empty() {
                out.push(delta);
            }
            self.buffer.clear();
            self.done = true;
            return out;
        }

        let keep = partial_end_tag_suffix_len(&self.buffer);
        let emit_len = self.buffer.len().saturating_sub(keep);
        if emit_len > 0 {
            out.push(self.buffer[..emit_len].to_string());
            self.buffer.drain(..emit_len);
        }
        out
    }

    fn finish(&mut self) -> Vec<String> {
        if self.done || !self.in_summary || self.buffer.is_empty() {
            return Vec::new();
        }
        self.done = true;
        vec![std::mem::take(&mut self.buffer)]
    }
}

fn partial_end_tag_suffix_len(text: &str) -> usize {
    let tag = "</summary>";
    let max_len = tag.len().min(text.len());
    for len in (1..=max_len).rev() {
        if text.ends_with(&tag[..len]) {
            return len;
        }
    }
    0
}

async fn forward_compact_summary_deltas(
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    trigger: CompactTrigger,
    runtime_context: Option<&ToolRuntimeContext>,
    deltas: Vec<String>,
) {
    for delta in deltas {
        if delta.is_empty() {
            continue;
        }
        let _ = event_tx
            .send(EngineToRuntimeEvent::CompactSummaryDelta(
                CompactSummaryDeltaEvent {
                    trigger,
                    delta,
                    session_id: runtime_context.map(|runtime| runtime.session_id.clone()),
                    agent_label: runtime_context.and_then(|runtime| runtime.agent_label.clone()),
                },
            ))
            .await;
    }
}

fn format_compact_summary(raw_summary: &str) -> String {
    let without_analysis = strip_tag(raw_summary, "analysis");
    extract_tag(&without_analysis, "summary")
        .map(|summary| format!("Summary:\n{}", summary.trim()))
        .unwrap_or_else(|| without_analysis.trim().to_string())
}

fn visible_compact_summary(raw_summary: &str) -> String {
    let without_analysis = strip_tag(raw_summary, "analysis");
    extract_tag(&without_analysis, "summary")
        .unwrap_or(without_analysis)
        .trim()
        .to_string()
}

fn strip_tag(text: &str, tag: &str) -> String {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let mut out = text.to_string();
    while let Some(start) = out.find(&start_tag) {
        let Some(end_rel) = out[start + start_tag.len()..].find(&end_tag) else {
            break;
        };
        let end = start + start_tag.len() + end_rel + end_tag.len();
        out.replace_range(start..end, "");
    }
    out
}

fn extract_tag(text: &str, tag: &str) -> Option<String> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let start = text.find(&start_tag)? + start_tag.len();
    let end = text[start..].find(&end_tag)? + start;
    Some(text[start..end].to_string())
}

fn compact_boundary_message(
    trigger: CompactTrigger,
    pre_messages: usize,
    pre_tokens: usize,
    projected_post_messages: usize,
) -> Message {
    Message::from_user_text(format!(
        "[Compact boundary marker]\nEarlier conversation was compacted. Use the summary and preserved recent messages as the continuity boundary.\nTrigger: {trigger}\nPre-compact footprint: messages={pre_messages}, tokens={pre_tokens}\nProjected post-compact messages={projected_post_messages}"
    ))
}

fn split_preserving_tool_pairs(
    messages: &[Message],
    preserve_recent: usize,
) -> (Vec<Message>, Vec<Message>) {
    if messages.len() <= preserve_recent {
        return (Vec::new(), sanitize_messages(messages.to_vec()));
    }

    let mut split_index = messages.len().saturating_sub(preserve_recent);
    while split_index > 0
        && boundary_crosses_tool_pair(&messages[split_index - 1], &messages[split_index])
    {
        split_index -= 1;
    }

    let older = messages[..split_index].to_vec();
    let newer = sanitize_messages(messages[split_index..].to_vec());
    (older, newer)
}

fn boundary_crosses_tool_pair(previous: &Message, current: &Message) -> bool {
    if previous.role != Role::Assistant || current.role != Role::User {
        return false;
    }
    let pending = previous
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse(tool_use) => Some(tool_use.id.as_str()),
            _ => None,
        })
        .collect::<HashSet<_>>();
    if pending.is_empty() {
        return false;
    }
    current.content.iter().any(|block| match block {
        ContentBlock::ToolResult(result) => pending.contains(result.tool_use_id.as_str()),
        _ => false,
    })
}

pub fn sanitize_messages(messages: Vec<Message>) -> Vec<Message> {
    let mut sanitized = Vec::new();
    let mut pending_tool_use_ids: HashSet<String> = HashSet::new();
    let mut pending_tool_use_index: Option<usize> = None;

    for mut message in messages {
        if message.role == Role::Assistant && is_effectively_empty(&message) {
            continue;
        }

        let tool_uses = if message.role == Role::Assistant {
            tool_use_ids(&message)
        } else {
            Vec::new()
        };
        let tool_results = if message.role == Role::User {
            tool_result_ids(&message)
        } else {
            Vec::new()
        };

        let mut matched_pending_tool_results = false;
        if !pending_tool_use_ids.is_empty() {
            let result_ids = tool_results.iter().cloned().collect::<HashSet<_>>();
            if message.role != Role::User || !pending_tool_use_ids.is_subset(&result_ids) {
                if let Some(index) = pending_tool_use_index
                    && index < sanitized.len()
                {
                    sanitized.remove(index);
                }
                pending_tool_use_ids.clear();
                pending_tool_use_index = None;
            } else {
                matched_pending_tool_results = true;
                pending_tool_use_ids.clear();
                pending_tool_use_index = None;
            }
        }

        if message.role == Role::User && !tool_results.is_empty() && !matched_pending_tool_results {
            message
                .content
                .retain(|block| !matches!(block, ContentBlock::ToolResult(_)));
            if message.content.is_empty() {
                continue;
            }
        }

        sanitized.push(message);
        if !tool_uses.is_empty() {
            pending_tool_use_ids = tool_uses.into_iter().collect();
            pending_tool_use_index = Some(sanitized.len() - 1);
        }
    }

    if !pending_tool_use_ids.is_empty()
        && let Some(index) = pending_tool_use_index
        && index < sanitized.len()
    {
        sanitized.remove(index);
    }

    sanitized
}

fn is_effectively_empty(message: &Message) -> bool {
    message.content.iter().all(|block| match block {
        ContentBlock::Text(text) => text.text.trim().is_empty(),
        ContentBlock::Thinking(thinking) => thinking.thinking.trim().is_empty(),
        ContentBlock::Image(_) | ContentBlock::ToolUse(_) | ContentBlock::ToolResult(_) => false,
    })
}

fn tool_use_ids(message: &Message) -> Vec<String> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolUse(tool_use) => Some(tool_use.id.clone()),
            _ => None,
        })
        .collect()
}

fn tool_result_ids(message: &Message) -> Vec<String> {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::ToolResult(result) => Some(result.tool_use_id.clone()),
            _ => None,
        })
        .collect()
}

fn replace_images_with_placeholders(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .map(|message| {
            let content = message
                .content
                .iter()
                .map(|block| match block {
                    ContentBlock::Image(_) => ContentBlock::from_text(
                        "[Image omitted from compaction summarization.]".to_string(),
                    ),
                    other => other.clone(),
                })
                .collect();
            Message::new(message.role.clone(), content)
        })
        .collect()
}

fn truncate_head_for_ptl_retry(messages: &[Message]) -> Option<Vec<Message>> {
    let groups = group_messages_by_prompt_round(messages);
    if groups.len() < 2 {
        return None;
    }
    let drop_count = (groups.len() / 5).max(1).min(groups.len() - 1);
    let mut retained = groups
        .into_iter()
        .skip(drop_count)
        .flatten()
        .collect::<Vec<_>>();
    if retained.is_empty() {
        return None;
    }
    if retained[0].role == Role::Assistant {
        retained.insert(0, Message::from_user_text(PTL_RETRY_MARKER.to_string()));
    }
    Some(retained)
}

fn group_messages_by_prompt_round(messages: &[Message]) -> Vec<Vec<Message>> {
    let mut groups = Vec::new();
    let mut current = Vec::new();
    for message in messages {
        let starts_new_round = message.role == Role::User
            && !message
                .content
                .iter()
                .any(|block| matches!(block, ContentBlock::ToolResult(_)))
            && !message_text(message).trim().is_empty();
        if starts_new_round && !current.is_empty() {
            groups.push(current);
            current = Vec::new();
        }
        current.push(message.clone());
    }
    if !current.is_empty() {
        groups.push(current);
    }
    groups
}

fn message_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

fn is_prompt_too_long_error(error: &str) -> bool {
    let error = error.to_ascii_lowercase();
    [
        "prompt too long",
        "context_length_exceeded",
        "context length",
        "maximum context",
        "context window",
        "input tokens exceed",
        "too many tokens",
        "too large for the model",
        "maximum context length",
        "exceed_context",
        "exceeds the available context size",
        "available context size",
    ]
    .iter()
    .any(|needle| error.contains(needle))
}

fn rewrite_runtime_history(runtime_context: Option<&ToolRuntimeContext>, messages: &[Message]) {
    let Some(runtime_context) = runtime_context else {
        return;
    };
    if let Err(error) = rewrite_history(&runtime_context.session_dir, messages) {
        tracing::warn!(msg = "failed to rewrite compacted history", error = %error);
    }
}

fn rewrite_history(session_dir: &SessionDir, messages: &[Message]) -> Result<(), String> {
    session_dir
        .rewrite_history(messages)
        .map_err(|error| error.to_string())
}

fn compact_event(
    trigger: CompactTrigger,
    runtime_context: Option<&ToolRuntimeContext>,
) -> CompactEvent {
    CompactEvent {
        trigger,
        session_id: runtime_context.map(|runtime| runtime.session_id.clone()),
        agent_label: runtime_context.and_then(|runtime| runtime.agent_label.clone()),
    }
}

fn compact_session_id(runtime_context: Option<&ToolRuntimeContext>) -> Option<&str> {
    runtime_context.map(|runtime| runtime.session_id.as_str())
}

fn compact_session_type(runtime_context: Option<&ToolRuntimeContext>) -> Option<&str> {
    runtime_context.map(|runtime| runtime.session_type.as_str())
}

fn compact_agent_label(runtime_context: Option<&ToolRuntimeContext>) -> Option<&str> {
    runtime_context.and_then(|runtime| runtime.agent_label.as_deref())
}

async fn emit_compact_shrink_finished(
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    trigger: CompactTrigger,
    runtime_context: Option<&ToolRuntimeContext>,
    outcome: CompactOutcome,
) {
    let _ = event_tx
        .send(EngineToRuntimeEvent::CompactShrinkFinished(
            CompactShrinkFinishedEvent {
                trigger,
                before_tokens: outcome.before_tokens,
                after_tokens: outcome.after_tokens,
                before_messages: outcome.before_messages,
                after_messages: outcome.after_messages,
                session_id: runtime_context.map(|runtime| runtime.session_id.clone()),
                agent_label: runtime_context.and_then(|runtime| runtime.agent_label.clone()),
            },
        ))
        .await;
}

async fn emit_compact_summary_started(
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    trigger: CompactTrigger,
    runtime_context: Option<&ToolRuntimeContext>,
) {
    let _ = event_tx
        .send(EngineToRuntimeEvent::CompactSummaryStarted(compact_event(
            trigger,
            runtime_context,
        )))
        .await;
}

async fn emit_compact_summary_finished(
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    trigger: CompactTrigger,
    runtime_context: Option<&ToolRuntimeContext>,
    summary: String,
    after_tokens: usize,
) {
    let _ = event_tx
        .send(EngineToRuntimeEvent::CompactSummaryFinished(
            CompactSummaryFinishedEvent {
                trigger,
                summary,
                after_tokens,
                session_id: runtime_context.map(|runtime| runtime.session_id.clone()),
                agent_label: runtime_context.and_then(|runtime| runtime.agent_label.clone()),
            },
        ))
        .await;
}

async fn emit_compact_summary_failed(
    event_tx: &mpsc::Sender<EngineToRuntimeEvent>,
    trigger: CompactTrigger,
    runtime_context: Option<&ToolRuntimeContext>,
    message: String,
) {
    let _ = event_tx
        .send(EngineToRuntimeEvent::CompactSummaryFailed(
            CompactSummaryFailedEvent {
                trigger,
                message,
                session_id: runtime_context.map(|runtime| runtime.session_id.clone()),
                agent_label: runtime_context.and_then(|runtime| runtime.agent_label.clone()),
            },
        ))
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_config::ProviderProfile;
    use omini_domain::config::{ModelInfo, ProviderEndpointKind};
    use omini_domain::message::{ContentBlock, ToolUseBlock};
    use std::collections::HashMap;
    use std::path::PathBuf;

    fn tool_use(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolUse(ToolUseBlock {
            id: id.to_string(),
            name: name.to_string(),
            input: HashMap::new(),
        })
    }

    fn tool_result(id: &str, content: &str) -> ContentBlock {
        ContentBlock::ToolResult(ToolResultBlock {
            tool_use_id: id.to_string(),
            is_error: false,
            content: content.to_string(),
            metadata: None,
        })
    }

    fn settings_with_limit(limit: u32) -> Settings {
        let model = "test-model".to_string();
        let provider = "test".to_string();
        let mut providers = HashMap::new();
        providers.insert(
            provider.clone(),
            ProviderProfile {
                name: "Test".to_string(),
                endpoint: ProviderEndpointKind::OpenAI,
                api_key: String::new(),
                base_url: String::new(),
                models: vec![ModelInfo {
                    id: model.clone(),
                    name: None,
                    limit,
                    thinking: false,
                    input_modalities: None,
                }],
            },
        );

        Settings {
            api_key: String::new(),
            base_url: String::new(),
            model,
            endpoint: ProviderEndpointKind::OpenAI,
            providers,
            active_provider: provider,
            system_prompt: None,
            language: None,
            max_turns: None,
            cwd: PathBuf::from("."),
            thinking_effort: None,
            permissions: None,
            compact: CompactConfig::default(),
            mcp_servers: HashMap::new(),
        }
    }

    #[test]
    fn auto_compact_thresholds_cap_default_window_at_hard_percent() {
        let settings = settings_with_limit(256_000);

        let thresholds = auto_compact_thresholds(&settings);

        assert_eq!(thresholds.soft, 204_800);
        assert_eq!(thresholds.hard, 217_600);
    }

    #[test]
    fn auto_compact_thresholds_keep_reserve_limit_when_lower_than_percent() {
        let settings = settings_with_limit(100_000);

        let thresholds = auto_compact_thresholds(&settings);

        assert_eq!(thresholds.soft, 67_000);
        assert_eq!(thresholds.hard, 67_000);
    }

    #[test]
    fn auto_compact_thresholds_keep_soft_at_or_below_hard() {
        let settings = settings_with_limit(200_000);

        let thresholds = auto_compact_thresholds(&settings);

        assert_eq!(thresholds.soft, 160_000);
        assert_eq!(thresholds.hard, 167_000);
        assert!(thresholds.soft <= thresholds.hard);
    }

    #[test]
    fn auto_compact_decision_splits_skip_local_and_full_ranges() {
        let thresholds = AutoCompactThresholds { soft: 80, hard: 85 };

        assert_eq!(
            auto_compact_decision(79, thresholds),
            AutoCompactDecision::Skip
        );
        assert_eq!(
            auto_compact_decision(80, thresholds),
            AutoCompactDecision::LocalOnly
        );
        assert_eq!(
            auto_compact_decision(84, thresholds),
            AutoCompactDecision::LocalOnly
        );
        assert_eq!(
            auto_compact_decision(85, thresholds),
            AutoCompactDecision::FullIfLocalInsufficient
        );
    }

    #[test]
    fn split_preserving_tool_pairs_expands_recent_boundary() {
        let messages = vec![
            Message::from_user_text("old".to_string()),
            Message::new(Role::Assistant, vec![tool_use("tool_1", "read")]),
            Message::new(Role::User, vec![tool_result("tool_1", "result")]),
            Message::from_user_text("new".to_string()),
        ];

        let (older, newer) = split_preserving_tool_pairs(&messages, 2);

        assert_eq!(older.len(), 1);
        assert_eq!(newer.len(), 3);
    }

    #[test]
    fn sanitize_removes_orphan_tool_result() {
        let messages = vec![
            Message::new(Role::User, vec![tool_result("missing", "orphan")]),
            Message::from_user_text("next".to_string()),
        ];

        let sanitized = sanitize_messages(messages);

        assert_eq!(sanitized.len(), 1);
        assert_eq!(message_text(&sanitized[0]), "next");
    }

    #[test]
    fn sanitize_removes_unclosed_tool_use_tail() {
        let messages = vec![
            Message::from_user_text("start".to_string()),
            Message::new(Role::Assistant, vec![tool_use("tool_1", "read")]),
        ];

        let sanitized = sanitize_messages(messages);

        assert_eq!(sanitized.len(), 1);
        assert_eq!(message_text(&sanitized[0]), "start");
    }

    #[test]
    fn microcompact_clears_old_tool_results() {
        let mut messages = vec![
            Message::new(Role::Assistant, vec![tool_use("tool_1", "read")]),
            Message::new(Role::User, vec![tool_result("tool_1", "large output")]),
            Message::new(Role::Assistant, vec![tool_use("tool_2", "bash")]),
            Message::new(Role::User, vec![tool_result("tool_2", "latest output")]),
        ];

        let saved = microcompact_messages(&mut messages, 1);

        assert!(saved > 0);
        let ContentBlock::ToolResult(first) = &messages[1].content[0] else {
            panic!("expected tool result");
        };
        let ContentBlock::ToolResult(second) = &messages[3].content[0] else {
            panic!("expected tool result");
        };
        assert_eq!(first.content, TIME_BASED_MC_CLEARED_MESSAGE);
        assert_eq!(second.content, "latest output");
    }

    #[test]
    fn context_collapse_preserves_head_and_tail() {
        let long = format!(
            "{}{}{}",
            "a".repeat(1000),
            "b".repeat(2000),
            "c".repeat(1000)
        );
        let messages = vec![
            Message::from_user_text(long),
            Message::from_user_text("middle".to_string()),
            Message::from_user_text("recent".to_string()),
            Message::from_user_text("latest".to_string()),
        ];

        let collapsed = try_context_collapse(&messages, 1).expect("should collapse");
        let text = message_text(&collapsed[0]);

        assert!(text.contains("[collapsed"));
        assert!(text.starts_with('a'));
        assert!(text.ends_with('c'));
    }

    #[test]
    fn token_estimate_counts_tools_and_system_prompt() {
        let tools = vec![ToolDefinition {
            name: "read".to_string(),
            description: "Read a file".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let messages = vec![Message::from_user_text("hello".to_string())];

        let with_tools = estimate_request_tokens(&messages, Some("system"), &tools);
        let without_tools = estimate_request_tokens(&messages, None, &[]);

        assert!(with_tools > without_tools);
    }

    #[test]
    fn compact_summary_forwarder_streams_only_summary_tag_content() {
        let mut forwarder = CompactSummaryForwarder::new();
        let mut out = Vec::new();

        out.extend(forwarder.push_str("<analysis>hidden</analysis><summ"));
        out.extend(forwarder.push_str("ary>visible"));
        out.extend(forwarder.push_str(" text</sum"));
        out.extend(forwarder.push_str("mary>ignored"));
        out.extend(forwarder.finish());

        assert_eq!(out.join(""), "visible text");
    }

    #[test]
    fn compact_prompt_includes_custom_focus() {
        let prompt = get_compact_prompt(Some("保留关于compact实现部分"));

        assert!(prompt.contains("Additional user focus"));
        assert!(prompt.contains("保留关于compact实现部分"));
    }
}
