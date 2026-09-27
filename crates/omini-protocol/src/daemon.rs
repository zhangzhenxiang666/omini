//! 服务状态与客户端注册协议。

use super::*;

/// daemon 健康检查响应，用于客户端确认本地服务可用并识别服务名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct DaemonHealthResponse {
    pub ok: bool,
    pub daemon: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub protocol_revision: u32,
    pub bundled_rg: BundledToolStatus,
}

/// server 管理的内置依赖状态；客户端只展示状态，不负责下载或修复。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundledToolState {
    Ready,
    Restoring,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct BundledToolStatus {
    pub state: BundledToolState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// 客户端注册成功后返回的连接身份，后续 HTTP/WS 请求通过它关联同一个客户端。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct RegisterClientResponse {
    pub client_id: String,
}
