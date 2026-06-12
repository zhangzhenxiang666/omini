use std::path::Path;
use std::process::Command;

/// 通过 `git branch --show-current` 检测当前分支名。
///
/// 不在 git 仓库中时返回 `None`。
/// detached HEAD 时回退到 `git rev-parse --short HEAD`，返回 `"detached <sha>"`。
pub fn detect_git_branch(cwd: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !branch.is_empty() {
        return Some(branch);
    }

    // detached HEAD：回退到 rev-parse 获取短 SHA
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(cwd)
        .output()
        .ok()?;

    if output.status.success() {
        let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !sha.is_empty() {
            return Some(format!("detached {sha}"));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_none_outside_repo() {
        let dir = temp_dir();
        assert_eq!(detect_git_branch(&dir), None);
    }

    #[test]
    fn detect_branch_in_repo() {
        let repo_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let branch = detect_git_branch(&repo_root);
        assert!(branch.is_some(), "should detect branch in project repo");
    }

    fn temp_dir() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("omini-git-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}
