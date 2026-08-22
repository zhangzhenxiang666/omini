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
