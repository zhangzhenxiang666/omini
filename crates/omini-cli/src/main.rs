use clap::Args;
use clap::Parser;
use clap::Subcommand;
use omini_core::config::project::sanitize;
use omini_protocol as protocol;
use serde::Deserialize;
use std::env;
use std::error::Error;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::process::ExitCode;
use std::process::Stdio;
use std::thread;
use std::time::Duration;
use std::time::Instant;

const DAEMON_RUNNER_COMMAND: &str = "__daemon";

#[derive(Debug, Parser)]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<CliCommand>,
}

#[derive(Debug, Subcommand)]
enum CliCommand {
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    #[command(name = DAEMON_RUNNER_COMMAND, hide = true)]
    Daemon(DaemonArgs),
}

#[derive(Debug, Args)]
struct DaemonArgs {
    #[arg(long)]
    foreground: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Subcommand)]
enum ServerCommand {
    Start,
    Stop,
    Restart,
    Status,
}

#[derive(Debug, Deserialize)]
struct DaemonHint {
    #[serde(default = "default_host")]
    host: String,
    port: u16,
    #[serde(default)]
    pid: Option<u32>,
}

impl DaemonHint {
    fn addr(&self) -> Option<SocketAddr> {
        format!("{}:{}", self.host, self.port).parse().ok()
    }
}

#[derive(Debug, Clone, Copy)]
struct DaemonStatus {
    addr: SocketAddr,
    pid: Option<u32>,
}

struct StartupLock {
    path: PathBuf,
    _file: fs::File,
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e}");
            let mut source = e.source();
            while let Some(s) = source {
                eprintln!("  cause: {s}");
                source = s.source();
            }
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, Box<dyn Error>> {
    let cli = Cli::parse();
    match cli.command {
        None => {
            let connection = connect_current_project().map_err(io::Error::other)?;
            omini_tui::run_ui(connection)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(CliCommand::Server { command }) => run_server_command(command),
        Some(CliCommand::Daemon(args)) => {
            omini_server::process::run_daemon_process(omini_server::process::ProcessOptions {
                foreground: args.foreground,
            })?;
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn run_server_command(command: ServerCommand) -> Result<ExitCode, Box<dyn Error>> {
    match command {
        ServerCommand::Start => start_server(),
        ServerCommand::Stop => stop_server(),
        ServerCommand::Restart => {
            stop_server()?;
            start_server()
        }
        ServerCommand::Status => status_server(),
    }
}

fn connect_current_project() -> Result<omini_tui::ProjectConnection, String> {
    let cwd = env::current_dir().map_err(|err| format!("read cwd: {err}"))?;
    let status = ensure_daemon()?;
    let http = daemon_http_client()?;
    let register: protocol::RegisterClientResponse = post_json_without_client(
        &http,
        &format!("http://{}/v1/clients", status.addr),
        &protocol::RegisterClientRequest {
            kind: Some("tui".to_string()),
        },
    )?;
    let project_id = sanitize(&cwd);
    let attach: protocol::ProjectAttachResponse = put_json_without_client(
        &http,
        &format!("http://{}/v1/projects/{project_id}/attach", status.addr),
        &protocol::ProjectAttachRequest {
            cwd: cwd.display().to_string(),
        },
    )?;

    Ok(omini_tui::ProjectConnection {
        addr: status.addr,
        project_id,
        client_id: register.client_id,
        attach,
    })
}

fn start_server() -> Result<ExitCode, Box<dyn Error>> {
    let status = ensure_daemon().map_err(io::Error::other)?;
    println!("omini-server is running at {}", daemon_url(status));
    Ok(ExitCode::SUCCESS)
}

fn stop_server() -> Result<ExitCode, Box<dyn Error>> {
    let http = daemon_http_client().map_err(io::Error::other)?;
    let Some(status) = discover_healthy_daemon(&http) else {
        println!("omini-server is not running");
        return Ok(ExitCode::SUCCESS);
    };

    let _: protocol::AckResponse =
        post_empty_without_client(&http, &format!("http://{}/v1/shutdown", status.addr))
            .map_err(io::Error::other)?;

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if discover_healthy_daemon(&http).is_none() {
            println!("Stopped omini-server at {}", daemon_url(status));
            return Ok(ExitCode::SUCCESS);
        }
        thread::sleep(Duration::from_millis(100));
    }

    Err(io::Error::other(format!(
        "Timed out waiting for omini-server at {} to stop",
        daemon_url(status)
    ))
    .into())
}

fn status_server() -> Result<ExitCode, Box<dyn Error>> {
    let http = daemon_http_client().map_err(io::Error::other)?;
    if let Some(status) = discover_healthy_daemon(&http) {
        println!("omini-server is running at {}", daemon_url(status));
        Ok(ExitCode::SUCCESS)
    } else {
        println!("omini-server is not running");
        Ok(ExitCode::from(1))
    }
}

fn ensure_daemon() -> Result<DaemonStatus, String> {
    let http = daemon_http_client()?;

    if let Some(status) = discover_healthy_daemon(&http) {
        return Ok(status);
    }

    let _lock = match acquire_startup_lock(&http)? {
        StartupLockResult::Acquired(lock) => lock,
        StartupLockResult::DaemonHealthy(status) => return Ok(status),
    };
    if let Some(status) = discover_healthy_daemon(&http) {
        return Ok(status);
    }

    spawn_server_process()?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = discover_healthy_daemon(&http) {
            return Ok(status);
        }
        thread::sleep(Duration::from_millis(100));
    }

    Err("Timed out waiting for omini-server to become healthy".to_string())
}

fn daemon_http_client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(500))
        .build()
        .map_err(|err| format!("build daemon client: {err}"))
}

fn discover_healthy_daemon(http: &reqwest::blocking::Client) -> Option<DaemonStatus> {
    let hint = read_daemon_hint()?;
    let addr = hint.addr()?;
    let response = http
        .get(format!("http://{addr}/v1/health"))
        .send()
        .ok()?
        .json::<protocol::DaemonHealthResponse>()
        .ok()?;
    (response.ok && response.daemon == "omini-server").then_some(DaemonStatus {
        addr,
        pid: hint.pid,
    })
}

enum StartupLockResult {
    Acquired(StartupLock),
    DaemonHealthy(DaemonStatus),
}

fn acquire_startup_lock(http: &reqwest::blocking::Client) -> Result<StartupLockResult, String> {
    let run_dir = daemon_run_dir()?;
    fs::create_dir_all(&run_dir).map_err(|err| format!("create daemon run dir: {err}"))?;
    let path = run_dir.join("daemon.lock");
    let started = Instant::now();

    loop {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                return Ok(StartupLockResult::Acquired(StartupLock {
                    path,
                    _file: file,
                }));
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                if let Some(status) = discover_healthy_daemon(http) {
                    return Ok(StartupLockResult::DaemonHealthy(status));
                }
                if started.elapsed() > Duration::from_secs(3) {
                    let _ = fs::remove_file(&path);
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(err) => return Err(format!("acquire daemon startup lock: {err}")),
        }
    }
}

fn spawn_server_process() -> Result<(), String> {
    let mut command = server_process_command()?;
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|err| format!("start daemon: {err}"))
}

fn server_process_command() -> Result<ProcessCommand, String> {
    if let Some(path) = env::var_os("OMINI_SERVER_BIN").filter(|value| !value.is_empty()) {
        return Ok(ProcessCommand::new(PathBuf::from(path)));
    }

    let exe = env::current_exe().map_err(|err| format!("find current executable: {err}"))?;
    let mut command = ProcessCommand::new(exe);
    command.arg(DAEMON_RUNNER_COMMAND);
    Ok(command)
}

fn read_daemon_hint() -> Option<DaemonHint> {
    let content = fs::read_to_string(daemon_run_dir().ok()?.join("daemon.json")).ok()?;
    serde_json::from_str(&content).ok()
}

fn daemon_run_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".omini").join("run"))
        .ok_or_else(|| "cannot find home dir".to_string())
}

fn default_host() -> String {
    "127.0.0.1".to_string()
}

fn daemon_url(status: DaemonStatus) -> String {
    match status.pid {
        Some(pid) => format!("http://{} (pid {pid})", status.addr),
        None => format!("http://{}", status.addr),
    }
}

fn post_json_without_client<B, T>(
    http: &reqwest::blocking::Client,
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
        .map_err(|err| format!("POST {url}: {err}"))?;
    decode_response(response, url)
}

fn put_json_without_client<B, T>(
    http: &reqwest::blocking::Client,
    url: &str,
    body: &B,
) -> Result<T, String>
where
    B: serde::Serialize + ?Sized,
    T: serde::de::DeserializeOwned,
{
    let response = http
        .put(url)
        .json(body)
        .send()
        .map_err(|err| format!("PUT {url}: {err}"))?;
    decode_response(response, url)
}

fn post_empty_without_client<T>(http: &reqwest::blocking::Client, url: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let response = http
        .post(url)
        .send()
        .map_err(|err| format!("POST {url}: {err}"))?;
    decode_response(response, url)
}

fn decode_response<T>(response: reqwest::blocking::Response, url: &str) -> Result<T, String>
where
    T: serde::de::DeserializeOwned,
{
    let status = response.status();
    if status.is_success() {
        return response
            .json()
            .map_err(|err| format!("decode response {url}: {err}"));
    }
    let text = response.text().unwrap_or_default();
    if let Ok(error) = serde_json::from_str::<protocol::ProtocolError>(&text) {
        Err(error.message)
    } else {
        Err(format!("{url} returned {status}: {text}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_without_args_enters_tui() {
        let cli = Cli::try_parse_from(["omini"]).unwrap();

        assert!(cli.command.is_none());
    }

    #[test]
    fn cli_parses_server_status() {
        let cli = Cli::try_parse_from(["omini", "server", "status"]).unwrap();

        assert!(matches!(
            cli.command,
            Some(CliCommand::Server {
                command: ServerCommand::Status
            })
        ));
    }

    #[test]
    fn cli_parses_server_lifecycle_commands() {
        for (arg, expected) in [
            ("start", ServerCommand::Start),
            ("stop", ServerCommand::Stop),
            ("restart", ServerCommand::Restart),
        ] {
            let cli = Cli::try_parse_from(["omini", "server", arg]).unwrap();

            assert!(matches!(
                cli.command,
                Some(CliCommand::Server { command }) if command == expected
            ));
        }
    }
}
