use super::Store;
use crate::store::StoreError;
use omini_entity::{Attachment, Thread};

impl Store {
    /// 附件归属线程必须存在;模型 schema 无数据库外键,创建边界显式校验。
    pub async fn create_attachment(&self, attachment: &Attachment) -> Result<(), StoreError> {
        let mut db = self.conn();
        if Thread::filter_by_id(&attachment.thread_id)
            .first()
            .exec(&mut db)
            .await?
            .is_none()
        {
            return Err(StoreError::MissingRow(format!(
                "thread '{}'",
                attachment.thread_id
            )));
        }
        toasty::create!(Attachment {
            id: attachment.id.clone(),
            thread_id: attachment.thread_id.clone(),
            original_name: attachment.original_name.clone(),
            mime_type: attachment.mime_type.clone(),
            size: attachment.size,
            sha256: attachment.sha256.clone(),
            relative_path: attachment.relative_path.clone(),
            created_at: attachment.created_at,
        })
        .exec(&mut db)
        .await?;
        Ok(())
    }

    pub async fn get_attachment(
        &self,
        thread_id: &str,
        attachment_id: &str,
    ) -> Result<Option<Attachment>, StoreError> {
        let mut db = self.conn();
        // 主键点查后比对归属线程,等价于原 WHERE id = ? AND thread_id = ?。
        Ok(Attachment::filter_by_id(attachment_id)
            .first()
            .exec(&mut db)
            .await?
            .filter(|row| row.thread_id == thread_id))
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
