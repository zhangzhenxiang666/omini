use super::*;

impl Database {
    pub async fn create_attachment(&self, attachment: &Attachment) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO attachment(
                id, thread_id, original_name, mime_type, size, sha256, relative_path, created_at
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&attachment.id)
        .bind(&attachment.thread_id)
        .bind(&attachment.original_name)
        .bind(&attachment.mime_type)
        .bind(attachment.size)
        .bind(&attachment.sha256)
        .bind(&attachment.relative_path)
        .bind(attachment.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_attachment(
        &self,
        thread_id: &str,
        attachment_id: &str,
    ) -> Result<Option<Attachment>, StoreError> {
        sqlx::query_as::<_, Attachment>(
            "SELECT id, thread_id, original_name, mime_type, size, sha256, relative_path, created_at
             FROM attachment WHERE id = ? AND thread_id = ?",
        )
        .bind(attachment_id)
        .bind(thread_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(Into::into)
    }

    pub async fn get_attachments(
        &self,
        thread_id: &str,
        attachment_ids: &[String],
    ) -> Result<Vec<Attachment>, StoreError> {
        let mut attachments = Vec::with_capacity(attachment_ids.len());
        for attachment_id in attachment_ids {
            let Some(attachment) = self.get_attachment(thread_id, attachment_id).await? else {
                return Err(StoreError::AttachmentNotFound(attachment_id.clone()));
            };
            attachments.push(attachment);
        }
        Ok(attachments)
    }
}
