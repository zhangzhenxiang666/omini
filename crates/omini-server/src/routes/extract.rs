//! HTTP 输入提取器：将 Axum 默认拒绝响应转换为协议错误。

use crate::routes::{ApiError, api_error};
use crate::thread::ThreadSession;
use axum::Json;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{FromRequest, FromRequestParts, Multipart, Path, Query, Request};
use axum::http::{StatusCode, request::Parts};
use serde::de::DeserializeOwned;

/// JSON 请求体；保留 Axum 对语法、数据和 Content-Type 的状态码区分。
pub(crate) struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|error| api_error(error.status(), "invalid_json", error.body_text()))
    }
}

/// 路由路径参数；解析失败始终使用结构化 400 错误。
pub(crate) struct ApiPath<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiPath<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Path::<T>::from_request_parts(parts, state)
            .await
            .map(|Path(value)| Self(value))
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, "invalid_path", error.body_text()))
    }
}

/// Query 参数；解析失败始终使用结构化 400 错误。
pub(crate) struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|error| api_error(StatusCode::BAD_REQUEST, "invalid_query", error.body_text()))
    }
}

/// multipart 请求体；包括缺失或无效 Content-Type 的拒绝也遵守统一错误协议。
pub(crate) struct ApiMultipart(pub Multipart);

impl<S> FromRequest<S> for ApiMultipart
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Multipart::from_request(request, state)
            .await
            .map(Self)
            .map_err(|error| {
                api_error(
                    error.status(),
                    "invalid_attachment_upload",
                    error.body_text(),
                )
            })
    }
}

/// WebSocket 握手；无效 Upgrade 请求也返回协议错误体。
pub(crate) struct ApiWebSocketUpgrade(pub WebSocketUpgrade);

impl<S> FromRequestParts<S> for ApiWebSocketUpgrade
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        WebSocketUpgrade::from_request_parts(parts, state)
            .await
            .map(Self)
            .map_err(|error| {
                api_error(
                    error.status(),
                    "invalid_websocket_upgrade",
                    error.body_text(),
                )
            })
    }
}

/// 已声明的客户端身份；提取时拒绝缺失、无效或空白的 ID。
pub(crate) struct ClientId(pub String);

impl ClientId {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    /// 严格命令只允许当前 controller 执行，不隐式转移控制权。
    pub(crate) async fn require_controller(&self, thread: &ThreadSession) -> Result<(), ApiError> {
        if thread.is_controller(self.as_str()).await {
            Ok(())
        } else {
            Err(api_error(
                StatusCode::FORBIDDEN,
                "not_controller",
                "This client is observing the thread and cannot mutate it",
            ))
        }
    }

    /// 运行命令要求已有 WebSocket 连接，并允许连接中的客户端接管控制权。
    pub(crate) async fn take_control(&self, thread: &ThreadSession) -> Result<(), ApiError> {
        if !thread.is_client_connected(self.as_str()).await
            || thread.takeover_controller(self.0.clone()).await.is_none()
        {
            return Err(api_error(
                StatusCode::FORBIDDEN,
                "client_not_connected",
                "This client is not connected to the thread event stream",
            ));
        }
        Ok(())
    }
}

impl<S> FromRequestParts<S> for ClientId
where
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .headers
            .get("x-omini-client-id")
            .and_then(|value| value.to_str().ok())
            .filter(|value| !value.trim().is_empty())
            .map(|value| Self(value.to_string()))
            .ok_or_else(|| {
                api_error(
                    StatusCode::UNAUTHORIZED,
                    "missing_client_id",
                    "Mutating requests must include x-omini-client-id",
                )
            })
    }
}
