use super::*;

impl Database {
    pub async fn append_llm_message(
        &self,
        thread_id: &str,
        message: &Message,
        created_at: DateTime<Utc>,
        thread_dir: &ThreadDir,
    ) -> Result<(), StoreError> {
        let prepared = prepare_blocks(&message.content, thread_dir)?;
        let content = serde_json::to_string(&prepared.values)?;
        let mut tx = self.pool.begin().await?;
        let version: i64 =
            sqlx::query_scalar("SELECT llm_context_version FROM thread WHERE id = ?")
                .bind(thread_id)
                .fetch_one(&mut *tx)
                .await?;
        let ordinal: i64 = sqlx::query_scalar(
            "SELECT COALESCE(MAX(ordinal) + 1, 0)
                FROM llm_messages
                WHERE thread_id = ? AND context_version = ?",
        )
        .bind(thread_id)
        .bind(version)
        .fetch_one(&mut *tx)
        .await?;
        let result = sqlx::query(
            "INSERT INTO llm_messages(
                    thread_id,
                    context_version,
                    ordinal,
                    role,
                    content,
                    created_at
                )
                VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(thread_id)
        .bind(version)
        .bind(ordinal)
        .bind(message.role.to_string())
        .bind(content)
        .bind(created_at)
        .execute(&mut *tx)
        .await;
        if let Err(error) = result {
            cleanup_created_files(&prepared.created_files);
            return Err(error.into());
        }
        if let Err(error) = tx.commit().await {
            cleanup_created_files(&prepared.created_files);
            return Err(error.into());
        }
        Ok(())
    }

    pub async fn replace_llm_context(
        &self,
        thread_id: &str,
        expected_version: i64,
        messages: &[Message],
        created_at: DateTime<Utc>,
        thread_dir: &ThreadDir,
    ) -> Result<i64, StoreError> {
        let mut prepared_messages = Vec::with_capacity(messages.len());
        let mut created_files = Vec::new();
        for message in messages {
            match prepare_blocks(&message.content, thread_dir) {
                Ok(prepared) => {
                    created_files.extend(prepared.created_files);
                    prepared_messages.push((message.role.to_string(), prepared.values));
                }
                Err(error) => {
                    cleanup_created_files(&created_files);
                    return Err(error);
                }
            }
        }

        let result = async {
            let mut tx = self.pool.begin().await?;
            let actual: i64 =
                sqlx::query_scalar("SELECT llm_context_version FROM thread WHERE id = ?")
                    .bind(thread_id)
                    .fetch_one(&mut *tx)
                    .await?;
            if actual != expected_version {
                return Err(StoreError::ContextVersionConflict {
                    expected: expected_version,
                    actual,
                });
            }
            let next_version = expected_version + 1;
            for (ordinal, (role, blocks)) in prepared_messages.iter().enumerate() {
                sqlx::query(
                    "INSERT INTO llm_messages(
                            thread_id,
                            context_version,
                            ordinal,
                            role,
                            content,
                            created_at
                        )
                        VALUES (?, ?, ?, ?, ?, ?)",
                )
                .bind(thread_id)
                .bind(next_version)
                .bind(i64::try_from(ordinal).unwrap_or(i64::MAX))
                .bind(role)
                .bind(serde_json::to_string(blocks)?)
                .bind(created_at)
                .execute(&mut *tx)
                .await?;
            }
            let updated = sqlx::query(
                "UPDATE thread SET
                        llm_context_version = ?,
                        updated_at = ?
                    WHERE id = ? AND llm_context_version = ?",
            )
            .bind(next_version)
            .bind(Utc::now())
            .bind(thread_id)
            .bind(expected_version)
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() != 1 {
                return Err(StoreError::ContextVersionConflict {
                    expected: expected_version,
                    actual,
                });
            }
            tx.commit().await?;
            Ok(next_version)
        }
        .await;

        if result.is_err() {
            cleanup_created_files(&created_files);
        }
        result
    }

    pub async fn load_current_llm_messages(
        &self,
        thread_id: &str,
        thread_dir: &ThreadDir,
    ) -> Result<Vec<Message>, StoreError> {
        let rows = sqlx::query_as::<_, StoredLlmMessageRow>(
            "SELECT lm.role, lm.content
                FROM llm_messages lm
                JOIN thread t ON t.id = lm.thread_id
                WHERE lm.thread_id = ? AND lm.context_version = t.llm_context_version
                ORDER BY lm.ordinal",
        )
        .bind(thread_id)
        .fetch_all(&self.pool)
        .await?;
        rows.into_iter()
            .map(|row| {
                let role = parse_role(&row.role)?;
                let stored = serde_json::from_str::<Vec<serde_json::Value>>(&row.content)?;
                Ok(Message::new(role, load_blocks(&stored, thread_dir)?))
            })
            .collect()
    }
}

fn parse_role(role: &str) -> Result<Role, StoreError> {
    match role {
        "user" => Ok(Role::User),
        "assistant" => Ok(Role::Assistant),
        _ => Err(StoreError::InvalidData(format!("unknown role {role}"))),
    }
}
