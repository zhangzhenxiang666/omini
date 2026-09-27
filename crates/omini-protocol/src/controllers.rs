//! 线程控制权协议。

use super::*;

/// 客户端声明或接管线程控制权后的租约状态。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ControllerLease {
    pub client_id: String,
    /// 当前控制者客户端 ID；释放或无人控制时为空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
}
