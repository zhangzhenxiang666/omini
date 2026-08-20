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

    pub async fn insert_display_message(
        &self,
        thread_id: &str,
        display: &DisplayMessage,
        model_ref: Option<&str>,
        created_at: DateTime<Utc>,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            &self.pool,
            NewUiJson {
                thread_id,
                role: &display.role.to_string(),
                model_ref,
                content: &serde_json::to_string(display)?,
                kind: "display",
                created_at,
            },
            thread_dir,
        )
        .await
    }

    pub async fn insert_plan_message(
        &self,
        thread_id: &str,
        plan: &DisplayPlan,
        model_ref: &str,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            &self.pool,
            NewUiJson {
                thread_id,
                role: "assistant",
                model_ref: Some(model_ref),
                content: &serde_json::to_string(plan)?,
                kind: "plan",
                created_at: plan.created_at,
            },
            thread_dir,
        )
        .await
    }

    pub async fn insert_compact_summary_message(
        &self,
        thread_id: &str,
        summary: &DisplaySummary,
        model_ref: &str,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        insert_ui_json(
            &self.pool,
            NewUiJson {
                thread_id,
                role: "assistant",
                model_ref: Some(model_ref),
                content: &serde_json::to_string(summary)?,
                kind: "compact_summary",
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
    if let Ok(display) = serde_json::from_str::<DisplayMessage>(content_json) {
        return display.text.replace('\n', " ").replace('\r', "");
    }
    if let Ok(summary) = serde_json::from_str::<DisplaySummary>(content_json) {
        return summary.markdown.replace('\n', " ").replace('\r', "");
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
