use jiff::Timestamp;
use toasty::Deferred;

use super::Thread;

/// 附件元数据表:附件 ID 到线程目录下内容寻址文件的映射。
#[derive(Debug, Clone, toasty::Model)]
#[table = "attachment"]
#[index(name = "idx_attachment_thread", thread_id, created_at)]
pub struct Attachment {
    #[key]
    pub id: String,
    pub thread_id: String,
    pub original_name: String,
    pub mime_type: String,
    pub size: i64,
    /// 附件内容的内容寻址键。
    pub sha256: String,
    /// 线程存储目录下内容寻址文件的相对路径。
    pub relative_path: String,
    pub created_at: Timestamp,
    #[belongs_to(key = thread_id, references = id)]
    pub thread: Deferred<Thread>,
}
