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
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "omini_filelock_test_{}_{}_{}",
            std::process::id(),
            line!(),
            name
        ))
    }

    #[tokio::test]
    async fn acquire_blocks_concurrent_access_to_same_path() {
        let path = temp_path("same.txt");
        std::fs::write(&path, b"hi").unwrap();

        let service = FileLockService::new_uninit();
        let guard_a = service.acquire(&path).await;
        let enter = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(AtomicUsize::new(0));
        let entered_inner = Arc::clone(&entered);
        let enter_inner = Arc::clone(&enter);
        let path_inner = path.clone();
        let join = tokio::spawn(async move {
            let _guard = service.acquire(&path_inner).await;
            entered_inner.store(1, Ordering::SeqCst);
            enter_inner.notify_one();
        });

        // 第二个 acquire 必须等第一个 drop 之后才能进入。
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(entered.load(Ordering::SeqCst), 0);

        drop(guard_a);

        // 给另一个 task 一点时间获得锁并进入临界区。
        let notified = tokio::time::timeout(Duration::from_secs(1), enter.notified()).await;
        assert!(notified.is_ok(), "second acquire never entered");
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        join.await.unwrap();

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn acquire_allows_concurrent_access_to_different_paths() {
        let path_a = temp_path("a.txt");
        let path_b = temp_path("b.txt");
        std::fs::write(&path_a, b"a").unwrap();
        std::fs::write(&path_b, b"b").unwrap();

        let service = FileLockService::new_uninit();
        let _guard_a = service.acquire(&path_a).await;
        // 不同的 path 不应被 block。
        let guard_b = tokio::time::timeout(Duration::from_millis(200), service.acquire(&path_b))
            .await
            .expect("different path should not be blocked");

        drop(guard_b);

        let _ = std::fs::remove_file(path_a);
        let _ = std::fs::remove_file(path_b);
    }

    #[tokio::test]
    async fn acquire_releases_on_drop() {
        let path = temp_path("drop.txt");
        std::fs::write(&path, b"x").unwrap();

        let service = FileLockService::new_uninit();
        {
            let _g = service.acquire(&path).await;
        }

        // 立刻再 acquire 应能成功(不超时)。
        let acquire = tokio::time::timeout(Duration::from_millis(200), service.acquire(&path))
            .await
            .expect("lock should be released after drop");

        drop(acquire);

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn acquire_canonicalizes_relative_paths() {
        let dir = temp_path("canon");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("f.txt");
        std::fs::write(&file, b"x").unwrap();

        let service = FileLockService::new_uninit();
        let guard_a = service.acquire(&file).await;

        // 不同的字符串表示(相对 vs 绝对)走 canonicalize 后应被视作同一路径。
        let rel_path = std::env::current_dir().unwrap().join(&file);
        let enter = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(AtomicUsize::new(0));
        let entered_inner = Arc::clone(&entered);
        let enter_inner = Arc::clone(&enter);
        let service_inner_path = rel_path.clone();
        let join = tokio::spawn(async move {
            let _g = service.acquire(&service_inner_path).await;
            entered_inner.store(1, Ordering::SeqCst);
            enter_inner.notify_one();
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(entered.load(Ordering::SeqCst), 0);
        drop(guard_a);

        let _ = tokio::time::timeout(Duration::from_secs(1), enter.notified()).await;
        assert_eq!(entered.load(Ordering::SeqCst), 1);
        join.await.unwrap();

        let _ = std::fs::remove_dir_all(dir);
    }
}
