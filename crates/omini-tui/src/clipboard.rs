use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use std::io::{self, Write};
use std::process::{Command, Stdio};

pub fn copy_to_clipboard(text: &str) {
    // 后台线程执行，避免在 WSL2 等环境下 spawn 外部进程阻塞 TUI 事件循环。
    let text = text.to_owned();
    std::thread::spawn(move || {
        if copy_to_system_clipboard(&text) {
            return;
        }
        if std::env::var_os("TMUX").is_some() && copy_to_tmux_clipboard(&text) {
            return;
        }
        copy_to_terminal_clipboard(&text);
    });
}

fn write_to_command_stdin(program: &str, args: &[&str], text: &str) -> bool {
    let Ok(mut child) = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };

    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        return false;
    };
    if stdin.write_all(text.as_bytes()).is_err() {
        let _ = child.kill();
        return false;
    }
    drop(stdin);

    child.wait().is_ok_and(|status| status.success())
}

fn copy_to_tmux_clipboard(text: &str) -> bool {
    if write_to_command_stdin("tmux", &["load-buffer", "-w", "-"], text) {
        true
    } else {
        // 较老或受限的 tmux 版本可能不支持 -w，但至少
        // 会在 tmux 粘贴缓冲区中保留选区。
        let _ = write_to_command_stdin("tmux", &["load-buffer", "-"], text);
        false
    }
}

fn copy_to_system_clipboard(text: &str) -> bool {
    if is_wsl() && copy_to_windows_clipboard(text) {
        return true;
    }

    let commands: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(target_os = "windows") {
        &[("clip", &[])]
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    } else {
        &[
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
            ("wl-copy", &[]),
        ]
    };

    commands
        .iter()
        .any(|(program, args)| write_to_command_stdin(program, args, text))
}

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::env::var_os("WSL_INTEROP").is_some()
        || std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .map(|release| release.to_ascii_lowercase().contains("microsoft"))
            .unwrap_or(false)
}

fn copy_to_windows_clipboard(text: &str) -> bool {
    let text = text.replace('\n', "\r\n");
    let encoded = BASE64_STANDARD.encode(text.as_bytes());
    write_to_command_stdin(
        "powershell.exe",
        &[
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$b64 = [Console]::In.ReadToEnd(); $bytes = [Convert]::FromBase64String($b64); Set-Clipboard -Value ([Text.Encoding]::UTF8.GetString($bytes))",
        ],
        &encoded,
    ) || write_to_command_stdin("clip.exe", &[], &text)
}

fn copy_to_terminal_clipboard(text: &str) {
    let encoded = BASE64_STANDARD.encode(text.as_bytes());
    let mut stderr = io::stderr();
    let sequence = format!("\x1b]52;c;{}\x07", encoded);
    let bytes = if std::env::var_os("TMUX").is_some() {
        // tmux 仅在包装为 passthrough DCS 时才将 OSC 序列转发到外部终端，
        // 负载中的 ESC 字节必须加倍。
        let escaped = sequence.replace('\x1b', "\x1b\x1b");
        format!("\x1bPtmux;{}\x1b\\", escaped)
    } else {
        sequence
    };
    let _ = stderr.write_all(bytes.as_bytes());
    let _ = stderr.flush();
}
