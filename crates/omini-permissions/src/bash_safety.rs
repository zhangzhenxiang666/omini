//! 代码层安全底线检查：无法用 `prefix_rule` DSL 前缀匹配表达的危险模式检测。
//! 这些检查在任何规则匹配之前执行，不可被用户规则覆盖。

use crate::shell::{shell_words, split_shell_commands};

/// 代码层不可覆盖的安全底线检查。对明确危险的 shell 写法返回 `Some(Deny)`，
/// 否则返回 `None` 交由规则系统继续决策。
pub(crate) fn check_builtin_safety_deny(command: &str) -> Option<crate::PermissionDecision> {
    if is_forbidden_bash_command(command) {
        return Some(crate::PermissionDecision::Deny {
            reason: "Blocked high-risk shell command".to_string(),
        });
    }
    None
}

/// 判断命令是否属于不可覆盖的禁止类。
fn is_forbidden_bash_command(raw_command: &str) -> bool {
    is_download_and_execute(raw_command)
        || contains_forbidden_syntax(raw_command)
        || split_shell_commands(raw_command).iter().any(|part| {
            let lower = part.to_ascii_lowercase();
            let args = shell_words(&lower);
            bash_args_forbidden(&args)
        })
}

// 提权和系统服务管理命令。
const FORBIDDEN_PREFIXES: &[&str] = &["sudo", "su", "doas", "systemctl", "launchctl"];

// 磁盘分区命令。
const FORBIDDEN_DISK_COMMANDS: &[&str] =
    &["fdisk", "parted", "sfdisk", "gdisk", "sgdisk", "wipefs"];

// 明确危险的 shell 写法（如 fork bomb）；保持列表精简，只放确定的高危误操作。
const FORBIDDEN_SUBSTRINGS: &[&str] = &[":(){ :|:& };:"];

// 网络下载命令：下载后直接执行的管道在原始命令层默认禁止。
const DOWNLOAD_COMMANDS: &[&str] = &["curl", "wget"];

/// 检测下载后紧接 shell 执行的管道/串联模式。
/// 使用已经去除纯文本引号内容影响的命令段，避免把 `sha256sum` 或引用文本误判成 shell。
fn is_download_and_execute(command: &str) -> bool {
    let mut saw_download = false;
    for command in split_shell_commands(command) {
        let args = shell_words(&command.to_ascii_lowercase());
        if args
            .first()
            .is_some_and(|cmd| DOWNLOAD_COMMANDS.contains(&cmd.as_str()))
        {
            saw_download = true;
            continue;
        }
        let executes_shell = matches!(
            args.as_slice(),
            [shell, ..] if matches!(shell.as_str(), "sh" | "bash" | "/bin/sh" | "/bin/bash")
        ) || matches!(
            args.as_slice(),
            [sudo, shell, ..]
                if sudo == "sudo"
                    && matches!(shell.as_str(), "sh" | "bash" | "/bin/sh" | "/bin/bash")
        );
        if saw_download && executes_shell {
            return true;
        }
    }
    false
}

fn contains_forbidden_syntax(command: &str) -> bool {
    std::iter::once(command)
        .chain(split_shell_commands(command).iter().map(String::as_str))
        .any(|part| {
            let executable = unquoted_text(part)
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ");
            FORBIDDEN_SUBSTRINGS
                .iter()
                .any(|needle| executable.contains(needle))
        })
}

fn unquoted_text(command: &str) -> String {
    let mut text = String::new();
    let mut quote = None;
    let mut escaped = false;

    for ch in command.chars() {
        if escaped {
            if quote.is_none() {
                text.push(ch);
            }
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            }
            continue;
        }
        if matches!(ch, '\'' | '"') {
            quote = Some(ch);
        } else {
            text.push(ch);
        }
    }
    text
}

/// 判断单条子命令的参数是否触发禁止条件。
fn bash_args_forbidden(args: &[String]) -> bool {
    let Some(cmd) = args.first().map(|arg| arg.as_str()) else {
        return false;
    };
    FORBIDDEN_PREFIXES.contains(&cmd)
        || FORBIDDEN_DISK_COMMANDS.contains(&cmd)
        || cmd.starts_with("mkfs.")
        || cmd == "mkfs"
        || rm_args_remove_root_or_home(args)
}

/// 解析 `rm` 命令参数，判断是否为 `rm -rf /` 或 `rm -rf ~` 等删除根目录/家目录的操作。
fn rm_args_remove_root_or_home(args: &[String]) -> bool {
    if args.first().map(String::as_str) != Some("rm") {
        return false;
    }

    let mut recursive = false;
    let mut force = false;
    let mut targets = Vec::new();
    let mut options_done = false;

    for arg in args.iter().skip(1).map(String::as_str) {
        if !options_done && arg == "--" {
            options_done = true;
            continue;
        }
        if !options_done && arg.starts_with("--") {
            match arg {
                "--recursive" => recursive = true,
                "--force" => force = true,
                _ => {}
            }
            continue;
        }
        if !options_done && arg.starts_with('-') && arg.len() > 1 {
            for flag in arg.chars().skip(1) {
                match flag {
                    'r' | 'R' => recursive = true,
                    'f' => force = true,
                    _ => {}
                }
            }
            continue;
        }
        targets.push(arg);
    }

    recursive && force && targets.iter().any(|target| is_root_or_home_target(target))
}

/// 判断目标路径是否指向根目录或家目录。
fn is_root_or_home_target(target: &str) -> bool {
    let trimmed = target.trim_end_matches('/');
    matches!(target, "/*")
        || absolute_path_resolves_to_root(target)
        || matches!(trimmed, "~" | "$home" | "${home}")
        || matches!(target, "~/*" | "$home/*" | "${home}/*")
}

fn absolute_path_resolves_to_root(target: &str) -> bool {
    use std::path::Component;

    let path = std::path::Path::new(target);
    if !path.is_absolute() {
        return false;
    }

    let mut depth = 0usize;
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => depth = depth.saturating_sub(1),
            Component::Normal(_) => depth += 1,
            Component::Prefix(_) => return false,
        }
    }
    depth == 0
}
