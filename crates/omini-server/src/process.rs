//! `omini-server` 独立进程的启动参数和 daemonize 流程。

use crate::runtime_state;
use omini_config::OminiRoot;
use std::fs::OpenOptions;
use std::io;
use std::path::PathBuf;

/// server 进程自己的启动选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProcessOptions {
    pub foreground: bool,
}

impl ProcessOptions {
    /// 解析 daemon 进程自己的参数；客户端参数在 CLI/TUI 层处理。
    pub fn parse_from_env() -> Result<Self, String> {
        let mut foreground = false;
        for arg in std::env::args().skip(1) {
            match arg.as_str() {
                "--foreground" => foreground = true,
                "--help" | "-h" => {
                    return Err("Usage: omini-server [--foreground]".to_string());
                }
                other => return Err(format!("unknown argument '{other}'")),
            }
        }
        Ok(Self { foreground })
    }
}

/// 默认以后台 daemon 运行，测试和调试场景可以通过 --foreground 留在前台。
pub fn daemonize_if_needed(options: ProcessOptions) -> io::Result<()> {
    if options.foreground {
        Ok(())
    } else {
        daemonize_process()?;
        Ok(())
    }
}

/// daemonize 需要发生在 Tokio runtime 创建之前，避免 fork 后携带复杂运行时状态。
pub fn run_daemon_process(options: ProcessOptions) -> Result<(), Box<dyn std::error::Error>> {
    daemonize_if_needed(options)?;
    let log_dir = crate::logging::init()?;
    tracing::info!(log_dir = %log_dir.display(), "logging initialized");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

/// 加载全局配置后进入真正的 HTTP daemon；这里不处理客户端会话语义。
pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let root = OminiRoot::init()?;
    let config = root.load_config()?;
    config.validate()?;
    crate::serve_daemon(root).await?;
    Ok(())
}

#[cfg(unix)]
fn daemonize_process() -> io::Result<()> {
    let run_dir = runtime_state::run_dir()?;
    std::fs::create_dir_all(&run_dir)?;
    let working_dir = ensure_daemon_working_dir()?;
    // 后台进程不能再依赖父终端，stdout/stderr 固定写到 run 目录便于排查启动失败。
    let stdout = OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("daemon.out"))?;
    let stderr = OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("daemon.err"))?;
    daemonize::Daemonize::new()
        .working_directory(&working_dir)
        .umask(0o077)
        .stdout(stdout)
        .stderr(stderr)
        .start()
        .map_err(io::Error::other)
}

#[cfg(unix)]
fn ensure_daemon_working_dir() -> io::Result<PathBuf> {
    let dir = std::env::temp_dir().join("omini");
    std::fs::create_dir_all(&dir)?;

    let metadata = std::fs::symlink_metadata(&dir)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "daemon working directory is not a directory: {}",
                dir.display()
            ),
        ));
    }

    use std::os::unix::fs::PermissionsExt;
    let mut permissions = metadata.permissions();
    if permissions.mode() & 0o777 != 0o700 {
        permissions.set_mode(0o700);
        std::fs::set_permissions(&dir, permissions)?;
    }

    Ok(dir)
}

#[cfg(not(unix))]
fn daemonize_process() -> io::Result<()> {
    Ok(())
}
