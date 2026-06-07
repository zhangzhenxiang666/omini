use std::io;
use std::path::PathBuf;
use tracing_subscriber::EnvFilter;

const DEFAULT_FILTER: &str = "omini_core=info,omini_server=info,warn";
const DEFAULT_MAX_LOG_FILES: usize = 14;
const LOG_FILE_SUFFIX: &str = "omini-server.jsonl";

pub(crate) fn init() -> io::Result<PathBuf> {
    let filter = std::env::var("OMINI_LOG")
        .or_else(|_| std::env::var("RUST_LOG"))
        .unwrap_or_else(|_| DEFAULT_FILTER.to_string());
    let filter = EnvFilter::try_new(filter).unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));
    let log_dir = log_dir()?;
    std::fs::create_dir_all(&log_dir)?;
    let max_log_files = max_log_files();
    let writer = tracing_appender::rolling::RollingFileAppender::builder()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_suffix(LOG_FILE_SUFFIX)
        .max_log_files(max_log_files)
        .build(&log_dir)
        .map_err(io::Error::other)?;

    let _ = tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_current_span(true)
        .with_span_list(true)
        .with_target(true)
        .with_file(true)
        .with_line_number(true)
        .try_init();
    Ok(log_dir)
}

fn max_log_files() -> usize {
    std::env::var("OMINI_LOG_MAX_FILES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_MAX_LOG_FILES)
}

fn log_dir() -> io::Result<PathBuf> {
    let home = dirs::home_dir()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cannot find home dir"))?;
    Ok(home.join(".omini").join("logs").join("server"))
}
