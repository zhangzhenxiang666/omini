use chrono::Utc;
use omini_config::project::ProjectDir;
use omini_domain::events::{ActiveProfile, SubmittedPlan};
use omini_domain::message::{ContentBlock, Message, Role};
use omini_domain::proposed_plan::{
    ProposedPlanParser, ProposedPlanSegment, extract_proposed_plan_text,
};
use omini_runtime_contract::RuntimeToServerEvent;
use tokio::sync::mpsc;

const CURRENT_PLAN_ID: &str = "plan";
const CURRENT_PLAN_FILE: &str = "plan.md";

pub(super) struct ProposedPlanForwarder {
    parser: Option<ProposedPlanParser>,
}

impl ProposedPlanForwarder {
    pub(super) fn new(active_profile: ActiveProfile) -> Self {
        Self {
            parser: (active_profile == ActiveProfile::Plan).then(ProposedPlanParser::new),
        }
    }

    pub(super) async fn forward_text_delta(
        &mut self,
        event_tx: &mpsc::Sender<RuntimeToServerEvent>,
        delta: String,
    ) {
        let Some(parser) = self.parser.as_mut() else {
            let _ = event_tx.send(RuntimeToServerEvent::TextDelta(delta)).await;
            return;
        };

        forward_segments(event_tx, parser.push_str(&delta)).await;
    }

    pub(super) async fn flush(&mut self, event_tx: &mpsc::Sender<RuntimeToServerEvent>) {
        let Some(parser) = self.parser.as_mut() else {
            return;
        };
        forward_segments(event_tx, parser.finish()).await;
    }
}

async fn forward_segments(
    event_tx: &mpsc::Sender<RuntimeToServerEvent>,
    segments: Vec<ProposedPlanSegment>,
) {
    for segment in segments {
        match segment {
            ProposedPlanSegment::Normal(text) if !text.is_empty() => {
                let _ = event_tx.send(RuntimeToServerEvent::TextDelta(text)).await;
            }
            ProposedPlanSegment::ProposedPlanDelta(delta) if !delta.is_empty() => {
                let _ = event_tx
                    .send(RuntimeToServerEvent::ProposedPlanDelta(delta))
                    .await;
            }
            ProposedPlanSegment::Normal(_)
            | ProposedPlanSegment::ProposedPlanStart
            | ProposedPlanSegment::ProposedPlanDelta(_)
            | ProposedPlanSegment::ProposedPlanEnd => {}
        }
    }
}

pub(super) fn path(project: &ProjectDir, plan_id: &str) -> std::path::PathBuf {
    let plans_dir = project.path().join("plans");
    if plan_id == CURRENT_PLAN_ID {
        plans_dir.join(CURRENT_PLAN_FILE)
    } else {
        // TODO(plan-path): 这里保留旧的时间戳计划文件名兼容。确认不再需要读取旧计划后，
        // 可以改成忽略 plan_id 并始终返回 plans/plan.md。
        plans_dir.join(format!("{plan_id}.md"))
    }
}

/// 把已批准 plan 包装成新会话的首条 user message,server 端 fork 时使用。
pub(crate) fn compacted_context(plan_content: &str) -> String {
    format!(
        "A previous planning pass produced the approved plan below to accomplish the user's task. Implement the plan in a fresh context. Treat the plan as the source of user intent, re-read files as needed, and carry the work through implementation and verification.\n\nApproved plan:\n{plan_content}\n\nIntermediate planning discussion and discarded alternatives were intentionally omitted."
    )
}

pub(super) fn approval_message() -> String {
    "Approved. Implement the proposed plan now.".to_string()
}

pub(super) async fn persist_latest(
    project: &ProjectDir,
    active_profile: ActiveProfile,
    messages: &[Message],
) -> Result<Option<SubmittedPlan>, String> {
    if active_profile != ActiveProfile::Plan {
        return Ok(None);
    }

    let Some(message) = messages.last() else {
        return Ok(None);
    };
    if message.role != Role::Assistant {
        return Ok(None);
    }

    let text = assistant_text(message);
    let Some(markdown) = extract_proposed_plan_text(&text) else {
        return Ok(None);
    };
    let markdown = markdown.trim().to_string();
    if markdown.is_empty() {
        return Ok(None);
    }

    let created_at = Utc::now();
    let title = title_from_markdown(&markdown);
    let plans_dir = project.path().join("plans");
    let path = path(project, CURRENT_PLAN_ID);

    tokio::fs::create_dir_all(&plans_dir).await.map_err(|e| {
        format!(
            "Failed to create plans directory {}: {e}",
            plans_dir.display()
        )
    })?;
    // TODO(plan-load): 目前只先把最新计划写入 plans/plan.md。计划加载到
    // session 的具体方式还未确定，后续明确后再实现。
    tokio::fs::write(&path, &markdown)
        .await
        .map_err(|e| format!("Failed to write plan {}: {e}", path.display()))?;

    Ok(Some(SubmittedPlan {
        id: CURRENT_PLAN_ID.to_string(),
        title,
        markdown,
        path,
        created_at,
    }))
}

fn assistant_text(message: &Message) -> String {
    let mut text = String::new();
    for block in &message.content {
        if let ContentBlock::Text(tb) = block {
            text.push_str(&tb.text);
        }
    }
    text
}

fn title_from_markdown(markdown: &str) -> String {
    for line in markdown.lines() {
        let title = line.trim().trim_start_matches('#').trim();
        if !title.is_empty() {
            return title.chars().take(80).collect();
        }
    }
    "Plan".to_string()
}
