//! 不可覆盖的安全底线；使用与规则匹配相同的 AST 分析结果，避免包装器绕过检查。

use std::path::Path;

use crate::shell::{ShellAnalysis, ShellCommand, ShellWord};

/// 保留 fork bomb、下载后执行、递归强制删除根目录或家目录三类硬拒绝。
pub(crate) fn check_safety(
    analysis: &ShellAnalysis,
    home: Option<&Path>,
) -> Option<crate::PermissionDecision> {
    let mut saw_download = false;
    for command in &analysis.commands {
        if matches!(command.basename(), Some("curl" | "wget")) {
            saw_download = true;
        } else if saw_download && matches!(command.basename(), Some("sh" | "bash" | "zsh")) {
            return Some(blocked());
        }
        if removes_root_home(command, home) {
            return Some(blocked());
        }
    }
    analysis.fork_bomb.then(blocked)
}

fn blocked() -> crate::PermissionDecision {
    crate::PermissionDecision::Deny {
        reason: "Blocked high-risk shell command".to_string(),
    }
}

/// 动态参数不参与选项推断；明确的递归与强制标志仍可与已知根目录目标组合。
fn removes_root_home(command: &ShellCommand, home: Option<&Path>) -> bool {
    if command.basename() != Some("rm") {
        return false;
    }
    let mut recursive = false;
    let mut force = false;
    let mut root_target = false;
    let mut options_done = false;
    for word in command.words.iter().skip(1) {
        if let Some(arg) = word.literal.as_deref() {
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
                recursive |= arg[1..].contains(['r', 'R']);
                force |= arg[1..].contains('f');
                continue;
            }
        }
        root_target |= is_root_home(word, home);
    }
    recursive && force && root_target
}

fn is_root_home(word: &ShellWord, home: Option<&Path>) -> bool {
    if let Some(target) = &word.literal {
        let path = Path::new(target);
        return resolves_to_root(target)
            || home.is_some_and(|home| {
                path.is_absolute()
                    && crate::path_matcher::normalize_lexically(path)
                        == crate::path_matcher::normalize_lexically(home)
            });
    }
    // 保留 HOME 展开和根目录通配符的安全底线，单引号字面量不作变量展开处理。
    let target = word.source.replace('"', "");
    let target = target.as_str();
    let trimmed = target.trim_end_matches('/');
    matches!(target, "/*")
        || matches!(trimmed, "~" | "$HOME" | "${HOME}")
        || matches!(target, "~/*" | "$HOME/*" | "${HOME}/*")
}

fn resolves_to_root(target: &str) -> bool {
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
