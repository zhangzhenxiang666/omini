use omini_protocol::ProtocolError;
use reqwest::Method;

use crate::client::*;
pub fn project_threads_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/threads",
        connection.addr, connection.project_id
    )
}

pub fn project_models_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/models",
        connection.addr, connection.project_id
    )
}

pub fn project_model_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/model",
        connection.addr, connection.project_id
    )
}

pub fn project_thinking_effort_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/thinking-effort",
        connection.addr, connection.project_id
    )
}

pub fn project_agents_url(connection: &ProjectConnection) -> String {
    format!(
        "http://{}/v1/projects/{}/agents",
        connection.addr, connection.project_id
    )
}

pub fn project_agents_url_with_target(
    connection: &ProjectConnection,
    target_thread_id: Option<&str>,
) -> String {
    let url = project_agents_url(connection);
    match target_thread_id {
        Some(thread_id) => {
            format!("{url}?target_thread_id={}", percent_encode(thread_id))
        }
        None => url,
    }
}

pub fn project_agent_generate_url(connection: &ProjectConnection) -> String {
    format!("{}/generate", project_agents_url(connection))
}

pub fn project_agent_url(
    connection: &ProjectConnection,
    agent_id: &str,
    target_thread_id: Option<&str>,
) -> String {
    let url = format!(
        "{}/{}",
        project_agents_url(connection),
        percent_encode(agent_id)
    );
    match target_thread_id {
        Some(thread_id) => {
            format!("{url}?target_thread_id={}", percent_encode(thread_id))
        }
        None => url,
    }
}

pub fn thread_base_url(connection: &ProjectConnection, thread_id: &str) -> String {
    format!("{}/{}", project_threads_url(connection), thread_id)
}

pub async fn get_json<T>(http: &reqwest::Client, url: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let response = http
        .get(url)
        .send()
        .await
        .map_err(|err| format!("GET {url}: {err}"))?;
    decode_response(response, url).await
}

pub async fn post_json<B, T>(
    http: &reqwest::Client,
    url: &str,
    client_id: &str,
    body: &B,
) -> Result<T, String>
where
    B: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
{
    let response = http
        .post(url)
        .header(CLIENT_ID_HEADER, client_id)
        .json(body)
        .send()
        .await
        .map_err(|err| format!("POST {url}: {err}"))?;
    decode_response(response, url).await
}

/// 发送带客户端身份的命令，并确认服务端返回空的 204 响应。
pub async fn post_no_content<B: serde::Serialize + ?Sized>(
    http: &reqwest::Client,
    url: &str,
    client_id: &str,
    body: &B,
) -> Result<(), String> {
    let response = http
        .post(url)
        .header(CLIENT_ID_HEADER, client_id)
        .json(body)
        .send()
        .await
        .map_err(|err| format!("POST {url}: {err}"))?;
    expect_no_content(response, url).await
}

/// 发送无需客户端身份的命令，并确认服务端返回空的 204 响应。
pub async fn post_no_content_without_client<B: serde::Serialize + ?Sized>(
    http: &reqwest::Client,
    url: &str,
    body: &B,
) -> Result<(), String> {
    let response = http
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|err| format!("POST {url}: {err}"))?;
    expect_no_content(response, url).await
}

pub async fn post_json_without_client<B, T>(
    http: &reqwest::Client,
    url: &str,
    body: &B,
) -> Result<T, String>
where
    B: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
{
    let response = http
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|err| format!("POST {url}: {err}"))?;
    decode_response(response, url).await
}

pub async fn post_empty_without_client<T>(http: &reqwest::Client, url: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let response = http
        .post(url)
        .send()
        .await
        .map_err(|err| format!("POST {url}: {err}"))?;
    decode_response(response, url).await
}

pub async fn send_empty(
    http: &reqwest::Client,
    method: Method,
    url: &str,
    client_id: &str,
) -> Result<(), String> {
    let response = http
        .request(method, url)
        .header(CLIENT_ID_HEADER, client_id)
        .send()
        .await
        .map_err(|err| format!("request {url}: {err}"))?;
    expect_no_content(response, url).await
}

pub async fn send_empty_without_client(
    http: &reqwest::Client,
    method: Method,
    url: &str,
) -> Result<(), String> {
    let response = http
        .request(method, url)
        .send()
        .await
        .map_err(|err| format!("request {url}: {err}"))?;
    expect_no_content(response, url).await
}

async fn expect_no_content(response: reqwest::Response, url: &str) -> Result<(), String> {
    if response.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(());
    }
    if response.status().is_success() {
        return Err(format!(
            "{url} returned {}, expected 204",
            response.status()
        ));
    }
    decode_response::<serde_json::Value>(response, url)
        .await
        .map(|_| ())
}

pub async fn decode_response<T>(response: reqwest::Response, url: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let status = response.status();
    if status.is_success() {
        return response
            .json()
            .await
            .map_err(|err| format!("decode response {url}: {err}"));
    }
    let text = response.text().await.unwrap_or_default();
    // server 错误优先按协议错误显示给用户，保留原始响应只作为兜底诊断信息。
    if let Ok(error) = serde_json::from_str::<ProtocolError>(&text) {
        Err(error.message)
    } else {
        Err(format!("{url} returned {status}: {text}"))
    }
}
