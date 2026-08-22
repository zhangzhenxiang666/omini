use chrono::Local;
use std::path::PathBuf;

const COMMAND_SHELL: &str = "sh -c";

#[derive(Debug, Clone)]
pub struct EnvironmentContext {
    cwd: PathBuf,
    command_shell: String,
    login_shell: Option<String>,
    current_date: String,
    timezone: String,
    platform: String,
    os: Option<String>,
    kernel: Option<String>,
    architecture: String,
}

impl EnvironmentContext {
    pub fn detect(cwd: &std::path::Path) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            command_shell: COMMAND_SHELL.to_string(),
            login_shell: non_empty_env("SHELL"),
            current_date: Local::now().format("%Y-%m-%d").to_string(),
            timezone: detect_timezone(),
            platform: std::env::consts::OS.to_string(),
            os: detect_os_pretty_name(),
            kernel: detect_kernel(),
            architecture: std::env::consts::ARCH.to_string(),
        }
    }
}

pub fn environment_context_section(env: &EnvironmentContext) -> String {
    let mut section = String::new();
    section.push_str("<environment_context>\n");
    section.push_str("## Runtime\n\n");
    section.push_str(&format!("- Working directory: `{}`\n", env.cwd.display()));
    section.push_str(&format!("- Command shell: `{}`\n", env.command_shell));
    if let Some(shell) = &env.login_shell {
        section.push_str(&format!("- Login shell: `{shell}`\n"));
    } else {
        section.push_str("- Login shell: `unknown`\n");
    }
    section.push_str(&format!("- Current date: `{}`\n", env.current_date));
    section.push_str(&format!("- Timezone: `{}`\n", env.timezone));
    section.push_str(&format!("- Platform: `{}`\n", env.platform));
    if let Some(os) = &env.os {
        section.push_str(&format!("- OS: `{os}`\n"));
    } else {
        section.push_str("- OS: `unknown`\n");
    }
    if let Some(kernel) = &env.kernel {
        section.push_str(&format!("- Kernel: `{kernel}`\n"));
    } else {
        section.push_str("- Kernel: `unknown`\n");
    }
    section.push_str(&format!("- Architecture: `{}`\n", env.architecture));
    section.push_str("\n## Notes\n\n");
    section.push_str("- Paths are local filesystem paths.\n");
    section.push_str("- Commands run relative to the working directory unless stated otherwise.\n");
    section.push_str(
        "- This prompt does not assume a sandbox, approval system, or isolated filesystem.\n",
    );
    section.push_str("</environment_context>");
    section
}

fn non_empty_env(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn detect_timezone() -> String {
    if let Some(tz) = non_empty_env("TZ") {
        return tz;
    }

    if let Ok(timezone) = std::fs::read_to_string("/etc/timezone") {
        let timezone = timezone.trim();
        if !timezone.is_empty() {
            return timezone.to_string();
        }
    }

    "unknown".to_string()
}

fn detect_os_pretty_name() -> Option<String> {
    let content = std::fs::read_to_string("/etc/os-release").ok()?;
    for line in content.lines() {
        let Some(value) = line.strip_prefix("PRETTY_NAME=") else {
            continue;
        };
        return Some(unquote_os_release_value(value));
    }
    None
}

fn unquote_os_release_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('"') && trimmed.ends_with('"') {
        trimmed[1..trimmed.len() - 1].to_string()
    } else {
        trimmed.to_string()
    }
}

fn detect_kernel() -> Option<String> {
    let os = std::fs::read_to_string("/proc/sys/kernel/ostype").ok()?;
    let release = std::fs::read_to_string("/proc/sys/kernel/osrelease").ok()?;
    let os = os.trim();
    let release = release.trim();
    if os.is_empty() || release.is_empty() {
        return None;
    }
    Some(format!("{os} {release}"))
}
