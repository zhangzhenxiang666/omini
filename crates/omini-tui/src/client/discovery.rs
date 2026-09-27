use omini_protocol as protocol;
use serde::Deserialize;
use std::fs;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;
use tokio::time::timeout;

use crate::client::*;
pub async fn fetch_calibrated_runtime_status(
    http: &reqwest::Client,
    base: &str,
) -> Option<protocol::ThreadRuntimeStatus> {
    let started_at = Instant::now();
    let response: protocol::ThreadRuntimeStatusResponse = timeout(
        Duration::from_secs(2),
        get_json(http, &format!("{base}/status")),
    )
    .await
    .ok()?
    .ok()?;
    Some(apply_runtime_status_latency(
        response.status,
        started_at.elapsed(),
    ))
}

pub fn apply_runtime_status_latency(
    mut status: protocol::ThreadRuntimeStatus,
    latency: Duration,
) -> protocol::ThreadRuntimeStatus {
    // server 端 query timer 会扣掉等待工具授权/输入的暂停时间；暂停态不叠加客户端延迟，
    // 否则等待用户期间会被误算为工作耗时。
    if should_apply_runtime_status_latency(&status)
        && let Some(activity) = &mut status.activity
    {
        activity.elapsed_ms = activity
            .elapsed_ms
            .saturating_add(duration_millis_u64(latency));
    }
    status
}

pub fn should_apply_runtime_status_latency(status: &protocol::ThreadRuntimeStatus) -> bool {
    status.activity.is_some()
        && status.pending_pauses.is_empty()
        && matches!(
            status.state,
            protocol::ThreadRuntimeState::Thinking
                | protocol::ThreadRuntimeState::Working
                | protocol::ThreadRuntimeState::Compacting
        )
}

pub fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug, Deserialize)]
pub struct DaemonHint {
    #[serde(default = "default_daemon_host")]
    host: String,
    port: u16,
}

impl DaemonHint {
    fn addr(&self) -> Result<SocketAddr, String> {
        format!("{}:{}", self.host, self.port)
            .parse()
            .map_err(|err| format!("parse daemon address: {err}"))
    }
}

pub async fn reconnect_latest_daemon(
    http: &reqwest::Client,
    connection: &mut ProjectConnection,
) -> Result<(), String> {
    let addr = discover_healthy_daemon(http).await?;
    // daemon 重启后端口和进程内 client registry 都会变；先重建身份，再按 UUID open 原项目。
    let register: protocol::RegisterClientResponse = post_json_without_client(
        http,
        &format!("http://{addr}/v1/clients"),
        &protocol::RegisterClientRequest {
            kind: Some("tui".to_string()),
        },
    )
    .await?;
    let open: protocol::OpenProjectResponse = post_empty_without_client(
        http,
        &format!("http://{addr}/v1/projects/{}/open", connection.project_id),
    )
    .await?;

    connection.addr = addr;
    connection.project_id = open.project.id.clone();
    connection.client_id = register.client_id;
    connection.open = open;
    Ok(())
}

pub async fn discover_healthy_daemon(http: &reqwest::Client) -> Result<SocketAddr, String> {
    let hint = read_daemon_hint()?;
    let addr = hint.addr()?;
    let url = format!("http://{addr}/v1/health");
    let response: protocol::DaemonHealthResponse =
        timeout(Duration::from_millis(500), get_json(http, &url))
            .await
            .map_err(|_| format!("GET {url}: timed out"))??;
    if response.ok
        && response.daemon == "omini-server"
        && response.protocol_revision == protocol::PROTOCOL_REVISION
    {
        Ok(addr)
    } else if response.protocol_revision != protocol::PROTOCOL_REVISION {
        Err(format!(
            "incompatible daemon protocol at {url}: client revision {}, server revision {}",
            protocol::PROTOCOL_REVISION,
            response.protocol_revision
        ))
    } else {
        Err(format!("daemon health check failed at {url}"))
    }
}

pub fn read_daemon_hint() -> Result<DaemonHint, String> {
    let path = daemon_run_dir()?.join("daemon.json");
    let content =
        fs::read_to_string(&path).map_err(|err| format!("read {}: {err}", path.display()))?;
    serde_json::from_str(&content).map_err(|err| format!("decode {}: {err}", path.display()))
}

pub fn daemon_run_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".omini").join("run"))
        .ok_or_else(|| "cannot find home dir".to_string())
}

pub fn default_daemon_host() -> String {
    "127.0.0.1".to_string()
}
