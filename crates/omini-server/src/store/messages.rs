use super::*;

impl Database {
    pub async fn insert_message(
        &self,
        msg: &NewMessage,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let prepared = prepare_blocks(&msg.blocks, thread_dir)?;
        let blocks_json = serde_json::to_string(&prepared.values)?;
        let result = sqlx::query(
            "INSERT INTO messages(
                    thread_id,
                    role,
                    model_ref,
                    content,
                    kind,
                    created_at
                )
                VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&msg.thread_id)
        .bind(&msg.role)
        .bind(&msg.model_ref)
        .bind(blocks_json)
        .bind(&msg.kind)
        .bind(msg.created_at)
        .execute(&self.pool)
        .await;
        finish_prepared_write(result.map(|_| ()), &prepared.created_files)
    }

    pub async fn insert_user_input(
        &self,
        thread_id: &str,
        input: &omini_domain::conversation::UserInput,
        created_at: DateTime<Utc>,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        self.insert_conversation_entry(
            thread_id,
            &omini_domain::conversation::ConversationEntry::UserInput(input.clone()),
            "user",
            None,
            created_at,
            thread_dir,
        )
        .await
    }

    pub async fn insert_conversation_entry(
        &self,
        thread_id: &str,
        entry: &omini_domain::conversation::ConversationEntry,
        role: &str,
        model_ref: Option<&str>,
        created_at: DateTime<Utc>,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            &self.pool,
            NewUiJson {
                thread_id,
                role,
                model_ref,
                content: &serde_json::to_string(entry)?,
                kind: "conversation_entry",
                created_at,
            },
            thread_dir,
        )
        .await
    }

    pub async fn insert_plan_message(
        &self,
        thread_id: &str,
        plan: &omini_domain::conversation::ProposedPlan,
        model_ref: &str,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            &self.pool,
            NewUiJson {
                thread_id,
                role: "assistant",
                model_ref: Some(model_ref),
                content: &serde_json::to_string(
                    &omini_domain::conversation::ConversationEntry::SystemEvent(
                        omini_domain::conversation::SystemEvent::Plan(plan.clone()),
                    ),
                )?,
                kind: "conversation_entry",
                created_at: plan.created_at,
            },
            thread_dir,
        )
        .await
    }

    pub async fn insert_compact_summary_message(
        &self,
        thread_id: &str,
        summary: &omini_domain::conversation::CompactionSummary,
        model_ref: &str,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            &self.pool,
            NewUiJson {
                thread_id,
                role: "assistant",
                model_ref: Some(model_ref),
                content: &serde_json::to_string(
                    &omini_domain::conversation::ConversationEntry::SystemEvent(
                        omini_domain::conversation::SystemEvent::Summary(summary.clone()),
                    ),
                )?,
                kind: "conversation_entry",
                created_at: summary.created_at,
            },
            thread_dir,
        )
        .await
    }

    pub async fn get_messages(&self, thread_id: &str) -> Result<Vec<StoredMessage>, StoreError> {
        let rows = sqlx::query_as::<_, StoredMessageRow>(
            "SELECT * FROM messages WHERE thread_id = ? ORDER BY id",
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(Into::into).collect())
    }

    pub async fn get_first_message_text(&self, thread_id: &str) -> Result<String, StoreError> {
        let row = sqlx::query_as::<_, StoredMessageRow>(
            "SELECT * FROM messages WHERE thread_id = ? ORDER BY id ASC LIMIT 1",
        )
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row
            .map(|message| extract_message_text(&message.content))
            .unwrap_or_default())
    }
}

struct NewUiJson<'a> {
    thread_id: &'a str,
    role: &'a str,
    model_ref: Option<&'a str>,
    content: &'a str,
    kind: &'a str,
    created_at: DateTime<Utc>,
}

async fn insert_ui_json(
    pool: &SqlitePool,
    row: NewUiJson<'_>,
    thread_dir: &ThreadDir,
) -> Result<(), StoreError> {
    let PreparedUiContent {
        value,
        created_files,
    } = prepare_ui_content(row.content, thread_dir)?;
    let result = sqlx::query(
        "INSERT INTO messages(
                thread_id,
                role,
                model_ref,
                content,
                kind,
                created_at)
            VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(row.thread_id)
    .bind(row.role)
    .bind(row.model_ref)
    .bind(value)
    .bind(row.kind)
    .bind(row.created_at)
    .execute(pool)
    .await;
    finish_prepared_write(result.map(|_| ()), &created_files)
}
fn extract_message_text(content_json: &str) -> String {
    if let Ok(entry) =
        serde_json::from_str::<omini_domain::conversation::ConversationEntry>(content_json)
    {
        let text = match entry {
            omini_domain::conversation::ConversationEntry::UserInput(input) => input
                .parts
                .iter()
                .filter_map(|part| match part {
                    omini_domain::input::InputPart::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<String>(),
            omini_domain::conversation::ConversationEntry::AssistantMessage(output) => output
                .blocks
                .iter()
                .filter_map(|block| match block {
                    omini_domain::conversation::AssistantMessageBlock::Text { text } => {
                        Some(text.as_str())
                    }
                    _ => None,
                })
                .collect::<String>(),
            omini_domain::conversation::ConversationEntry::SystemEvent(
                omini_domain::conversation::SystemEvent::Plan(plan),
            ) => plan.markdown,
            omini_domain::conversation::ConversationEntry::SystemEvent(
                omini_domain::conversation::SystemEvent::Summary(summary),
            ) => summary.markdown,
            omini_domain::conversation::ConversationEntry::SystemEvent(
                omini_domain::conversation::SystemEvent::TaskNotification(_),
            ) => String::new(),
            omini_domain::conversation::ConversationEntry::SystemEvent(
                omini_domain::conversation::SystemEvent::ToolResults { results },
            ) => results
                .iter()
                .map(|result| result.content.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        };
        return text.replace('\n', " ").replace('\r', "");
    }
    serde_json::from_str::<Vec<serde_json::Value>>(content_json)
        .unwrap_or_default()
        .iter()
        .filter(|value| value.get("type").and_then(serde_json::Value::as_str) == Some("text"))
        .filter_map(|value| value.get("text").and_then(serde_json::Value::as_str))
        .map(|text| text.replace('\n', " ").replace('\r', ""))
        .collect::<Vec<_>>()
        .join(" ")
}
