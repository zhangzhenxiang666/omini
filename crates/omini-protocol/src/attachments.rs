//! 图片附件协议。

use super::*;

/// 附件上传接口响应。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct AttachmentUploadResponse {
    pub attachment_id: String,
}

/// 附件上传的 multipart 表单形态；服务端按流处理实际文件内容。
#[derive(utoipa::ToSchema)]
pub struct AttachmentUploadForm {
    #[schema(content_media_type = "application/octet-stream")]
    pub file: Vec<u8>,
}
