use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Per-file 异步互斥锁服务。
///
/// 每个被访问过的绝对文件路径对应一个容量为 1 的 `Semaphore`,保证同一文件
/// 不会被并发地 read-modify-write。锁只在 execute 临界区持有,以避免阻塞
/// 权限审批阶段的并发工作。
pub struct FileLockService {
    inner: Mutex<HashMap<PathBuf, Arc<Semaphore>>>,
}

impl FileLockService {
    pub fn new_uninit() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// 全局单例,服务整个 daemon 进程。
    pub fn instance() -> &'static Self {
        static INSTANCE: OnceLock<FileLockService> = OnceLock::new();
        INSTANCE.get_or_init(Self::new_uninit)
    }

    /// 获取指定路径的写锁。`Drop` guard 时自动释放。
    pub async fn acquire(&self, path: &Path) -> FileLockGuard {
        let key = canonical_or_fallback(path);
        let sem = {
            let mut map = self.inner.lock().expect("file lock map poisoned");
            map.entry(key)
                .or_insert_with(|| Arc::new(Semaphore::new(1)))
                .clone()
        };
        let permit = sem
            .acquire_owned()
            .await
            .expect("file lock semaphore closed");
        FileLockGuard { _permit: permit }
    }
}

/// 持锁时存在,析构时自动释放。
pub struct FileLockGuard {
    _permit: OwnedSemaphorePermit,
}

fn canonical_or_fallback(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::{Notify, oneshot};

    #[tokio::test]
    async fn file_lock_blocks_same_file_until_guard_drops() {
        let temp = crate::test_support::TestTempDir::new("file-lock-same");
        let file = temp.write("file.txt", "x");
        let service = FileLockService::new_uninit();
        let first = service.acquire(&file).await;
        let (entered_tx, mut entered_rx) = oneshot::channel();
        let release = Arc::new(Notify::new());
        let release_waiter = Arc::clone(&release);
        let task = tokio::spawn(async move {
            let _second = service.acquire(&file).await;
            let _ = entered_tx.send(());
            release_waiter.notified().await;
        });

        tokio::task::yield_now().await;
        assert!(entered_rx.try_recv().is_err());
        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut entered_rx)
            .await
            .expect("second waiter should enter after guard drop")
            .expect("second waiter should report entry");
        release.notify_one();
        task.await.expect("lock task should finish");
    }

    #[tokio::test]
    async fn file_lock_distinguishes_paths_and_canonicalizes_equivalent_paths() {
        let temp = crate::test_support::TestTempDir::new("file-lock-paths");
        let first_file = temp.write("nested/file.txt", "x");
        let second_file = temp.write("other.txt", "x");
        let equivalent = temp.path().join("nested/../nested/file.txt");
        let service = FileLockService::new_uninit();
        let first = service.acquire(&first_file).await;

        let different = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            service.acquire(&second_file),
        )
        .await
        .expect("different file should not block");
        drop(different);

        let (entered_tx, mut entered_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _equivalent = service.acquire(&equivalent).await;
            let _ = entered_tx.send(());
        });
        tokio::task::yield_now().await;
        assert!(entered_rx.try_recv().is_err());
        drop(first);
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut entered_rx)
            .await
            .expect("canonical-equivalent path should acquire after release")
            .expect("waiter should report entry");
        task.await.expect("lock task should finish");
    }
}
