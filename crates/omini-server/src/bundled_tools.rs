use omini_protocol as protocol;
use sha2::Digest;
use sha2::Sha256;
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::Mutex;
use std::time::Duration;
use uuid::Uuid;

const RELEASE_REPOSITORY: &str = "zhangzhenxiang666/omini";

/// server 拥有 bundled rg 的可用性与恢复流程，所有客户端共享同一状态。
pub struct BundledTools {
    rg_path: PathBuf,
    status: Mutex<protocol::BundledToolStatus>,
    restore_lock: Mutex<()>,
}

impl BundledTools {
    pub fn new(root: &Path) -> Self {
        Self {
            rg_path: root.join("bin").join(rg_binary_name()),
            status: Mutex::new(protocol::BundledToolStatus {
                state: protocol::BundledToolState::Restoring,
                message: None,
            }),
            restore_lock: Mutex::new(()),
        }
    }

    pub fn status(&self) -> protocol::BundledToolStatus {
        self.status
            .lock()
            .expect("bundled tools lock poisoned")
            .clone()
    }

    pub fn ensure_rg(&self) -> Result<(), String> {
        let _restore = self.restore_lock.lock().expect("restore lock poisoned");
        if bundled_rg_is_usable(&self.rg_path) {
            self.set_status(protocol::BundledToolState::Ready, None);
            return Ok(());
        }
        self.set_status(protocol::BundledToolState::Restoring, None);
        match self.download_rg() {
            Ok(()) if bundled_rg_is_usable(&self.rg_path) => {
                self.set_status(protocol::BundledToolState::Ready, None);
                Ok(())
            }
            Ok(()) => {
                let message = format!(
                    "downloaded ripgrep is not executable at {}; rerun the omini installer",
                    self.rg_path.display()
                );
                self.set_status(
                    protocol::BundledToolState::Unavailable,
                    Some(message.clone()),
                );
                Err(message)
            }
            Err(error) => {
                self.set_status(protocol::BundledToolState::Unavailable, Some(error.clone()));
                Err(error)
            }
        }
    }

    fn download_rg(&self) -> Result<(), String> {
        let target = release_target()?;
        let version = env!("CARGO_PKG_VERSION");
        let release_base =
            format!("https://github.com/{RELEASE_REPOSITORY}/releases/download/v{version}");
        let asset_name = format!("rg-{target}");
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| format!("build ripgrep download client: {error}"))?;
        let checksums = client
            .get(format!("{release_base}/SHA256SUMS"))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("download ripgrep checksums: {error}"))?
            .text()
            .map_err(|error| format!("read ripgrep checksums: {error}"))?;
        let expected = checksum_for_asset(&checksums, &asset_name)?;
        let bytes = client
            .get(format!("{release_base}/{asset_name}"))
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .map_err(|error| format!("download bundled ripgrep for {target}: {error}"))?
            .bytes()
            .map_err(|error| format!("read bundled ripgrep download: {error}"))?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual != expected {
            return Err(format!(
                "bundled ripgrep checksum mismatch for {asset_name}: expected {expected}, got {actual}"
            ));
        }

        let parent = self
            .rg_path
            .parent()
            .ok_or_else(|| "cannot determine bundled ripgrep directory".to_string())?;
        fs::create_dir_all(parent)
            .map_err(|error| format!("create {}: {error}", parent.display()))?;
        let temporary = self
            .rg_path
            .with_extension(format!("download-{}", Uuid::new_v4()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|error| format!("create ripgrep temporary file: {error}"))?;
        file.write_all(&bytes)
            .map_err(|error| format!("write bundled ripgrep: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("sync bundled ripgrep: {error}"))?;
        drop(file);
        set_executable_permissions(&temporary)?;
        fs::rename(&temporary, &self.rg_path)
            .map_err(|error| format!("install bundled ripgrep: {error}"))?;
        Ok(())
    }

    fn set_status(&self, state: protocol::BundledToolState, message: Option<String>) {
        *self.status.lock().expect("bundled tools lock poisoned") =
            protocol::BundledToolStatus { state, message };
    }
}

fn bundled_rg_is_usable(path: &Path) -> bool {
    path.is_file()
        && Command::new(path)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
}

fn rg_binary_name() -> &'static str {
    if cfg!(windows) { "rg.exe" } else { "rg" }
}

fn checksum_for_asset(checksums: &str, asset_name: &str) -> Result<String, String> {
    checksums
        .lines()
        .find_map(|line| {
            let (checksum, name) = line.split_once(char::is_whitespace)?;
            (name.trim_start().trim_start_matches('*') == asset_name).then_some(checksum)
        })
        .filter(|checksum| {
            checksum.len() == 64
                && checksum
                    .chars()
                    .all(|character| character.is_ascii_hexdigit())
        })
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| format!("release checksum is missing for {asset_name}"))
}

fn release_target() -> Result<&'static str, String> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok("x86_64-unknown-linux-gnu")
    } else {
        Err("this omini-server build does not support automatic ripgrep recovery".to_string())
    }
}

#[cfg(unix)]
fn set_executable_permissions(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
        .map_err(|error| format!("mark bundled ripgrep executable: {error}"))
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_checksum_for_asset() {
        let checksums = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa  rg-aarch64-apple-darwin\n";
        assert_eq!(
            checksum_for_asset(checksums, "rg-aarch64-apple-darwin").unwrap(),
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        );
    }
}
