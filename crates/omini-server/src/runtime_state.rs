//! daemon 发现文件的读写位置和内容形状。

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process;

/// 写到 run 目录中的 daemon 连接信息。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonRuntimeState {
    pub host: String,
    pub port: u16,
    pub pid: u32,
}

impl DaemonRuntimeState {
    /// 当前 daemon 只监听 loopback，运行状态文件不要暴露可远程访问的地址。
    pub fn localhost(port: u16) -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port,
            pid: process::id(),
        }
    }
}

/// 写入客户端发现 daemon 所需的最小状态：地址、端口和进程号。
pub fn write(port: u16) -> io::Result<()> {
    let run_dir = run_dir()?;
    fs::create_dir_all(&run_dir)?;
    let state = DaemonRuntimeState::localhost(port);
    write_atomic(
        &run_dir.join("daemon.json"),
        &serde_json::to_string(&state).map_err(io::Error::other)?,
    )?;
    write_atomic(&run_dir.join("daemon.pid"), &state.pid.to_string())
}

/// daemon 退出时清理发现文件；失败被忽略，因为下次启动会覆盖这些文件。
pub fn cleanup() {
    if let Ok(run_dir) = run_dir() {
        let _ = fs::remove_file(run_dir.join("daemon.json"));
        let _ = fs::remove_file(run_dir.join("daemon.pid"));
    }
}

/// 运行状态固定放在用户 home 下，避免项目切换后客户端找不到已有 daemon。
pub fn run_dir() -> io::Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot find home dir"))?;
    Ok(home.join(".omini").join("run"))
}

fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content)?;
    fs::rename(tmp, path)
}
