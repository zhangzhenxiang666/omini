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
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEMP_DIR_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    struct TestTempDir(std::path::PathBuf);

    impl TestTempDir {
        fn new(label: &str) -> Self {
            let sequence = TEMP_DIR_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "omini-server-git-{label}-{}-{sequence}",
                std::process::id()
            ));
            std::fs::create_dir(&path).expect("test directory should be created");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn git_branch_non_repository_returns_none() {
        let directory = TestTempDir::new("outside-repository");

        assert_eq!(detect_git_branch(directory.path()), None);
    }

    #[test]
    fn git_branch_symbolic_head_returns_branch_name() {
        let directory = TestTempDir::new("symbolic-head");
        run_git(directory.path(), ["init", "--quiet"]);
        run_git(
            directory.path(),
            ["symbolic-ref", "HEAD", "refs/heads/topic"],
        );

        assert_eq!(
            detect_git_branch(directory.path()),
            Some("topic".to_string())
        );
    }

    #[test]
    fn git_branch_detached_head_returns_short_commit_identifier() {
        let directory = TestTempDir::new("detached-head");
        run_git(directory.path(), ["init", "--quiet"]);
        std::fs::write(directory.path().join("README.md"), "test\n")
            .expect("test file should be written");
        run_git(directory.path(), ["add", "README.md"]);
        run_git(
            directory.path(),
            [
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "--quiet",
                "-m",
                "test",
            ],
        );
        let short = git_output(directory.path(), ["rev-parse", "--short", "HEAD"]);
        run_git(directory.path(), ["checkout", "--quiet", "--detach"]);

        assert_eq!(
            detect_git_branch(directory.path()),
            Some(format!("detached {short}"))
        );
    }

    fn run_git<const N: usize>(directory: &Path, args: [&str; N]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(directory)
            .status()
            .expect("git command should start");
        assert!(status.success(), "git command should succeed: {status}");
    }

    fn git_output<const N: usize>(directory: &Path, args: [&str; N]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(directory)
            .output()
            .expect("git command should start");
        assert!(output.status.success(), "git command should succeed");
        String::from_utf8(output.stdout)
            .expect("git output should be UTF-8")
            .trim()
            .to_string()
    }
}
