use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

const MAX_CACHED_FILE_LOCKS: usize = 256;

/// Per-file 异步互斥锁服务。
///
/// 每个被访问过的绝对文件路径对应一个容量为 1 的 `Semaphore`,保证同一文件
/// 不会被并发地 read-modify-write。锁只在 execute 临界区持有,以避免阻塞
/// 权限审批阶段的并发工作。空闲路径锁按 LRU 最多缓存 256 个;仍被持有或有
/// 等待者的锁不会被淘汰,以保持同一路径的互斥。
pub struct FileLockService {
    inner: Arc<Mutex<FileLockState>>,
}

struct FileLockState {
    locks: HashMap<PathBuf, Arc<Semaphore>>,
    lru: VecDeque<PathBuf>,
    max_cached_locks: usize,
}

impl FileLockService {
    pub fn new_uninit() -> Self {
        Self::new_with_capacity(MAX_CACHED_FILE_LOCKS)
    }

    fn new_with_capacity(max_cached_locks: usize) -> Self {
        assert!(
            max_cached_locks > 0,
            "file lock cache capacity must be positive"
        );
        Self {
            inner: Arc::new(Mutex::new(FileLockState {
                locks: HashMap::new(),
                lru: VecDeque::new(),
                max_cached_locks,
            })),
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
            let mut state = self.inner.lock().expect("file lock map poisoned");
            let sem = state
                .locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Semaphore::new(1)))
                .clone();
            state.touch(key);
            state.evict_idle_locks();
            sem
        };
        let permit = sem
            .acquire_owned()
            .await
            .expect("file lock semaphore closed");
        FileLockGuard {
            permit: Some(permit),
            inner: Arc::clone(&self.inner),
        }
    }
}

impl FileLockState {
    fn touch(&mut self, path: PathBuf) {
        if let Some(index) = self.lru.iter().position(|existing| existing == &path) {
            self.lru.remove(index);
        }
        self.lru.push_back(path);
    }

    fn evict_idle_locks(&mut self) {
        while self.locks.len() > self.max_cached_locks {
            let Some(index) = self.lru.iter().position(|path| {
                self.locks
                    .get(path)
                    .is_some_and(|sem| Arc::strong_count(sem) == 1)
            }) else {
                return;
            };
            let path = self
                .lru
                .remove(index)
                .expect("LRU entry should exist at selected index");
            self.locks.remove(&path);
        }
    }
}

/// 持锁时存在,析构时自动释放。
pub struct FileLockGuard {
    permit: Option<OwnedSemaphorePermit>,
    inner: Arc<Mutex<FileLockState>>,
}

impl Drop for FileLockGuard {
    fn drop(&mut self) {
        drop(self.permit.take());
        self.inner
            .lock()
            .expect("file lock map poisoned")
            .evict_idle_locks();
    }
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

    #[tokio::test]
    async fn file_lock_evicts_the_least_recently_used_idle_path() {
        let temp = crate::test_support::TestTempDir::new("file-lock-lru");
        let first = temp.path().join("first.txt");
        let second = temp.path().join("second.txt");
        let third = temp.path().join("third.txt");
        let service = FileLockService::new_with_capacity(2);

        drop(service.acquire(&first).await);
        drop(service.acquire(&second).await);
        drop(service.acquire(&first).await);
        drop(service.acquire(&third).await);

        let state = service
            .inner
            .lock()
            .expect("file lock map should not be poisoned");
        assert_eq!(state.locks.len(), 2);
        assert!(state.locks.contains_key(&first));
        assert!(!state.locks.contains_key(&second));
        assert!(state.locks.contains_key(&third));
    }

    #[tokio::test]
    async fn file_lock_keeps_held_and_waiting_paths_out_of_lru_eviction() {
        let temp = crate::test_support::TestTempDir::new("file-lock-live-lru");
        let target = temp.write("target.txt", "x");
        let service = Arc::new(FileLockService::new_with_capacity(2));
        let first = service.acquire(&target).await;

        let (first_waiter_entered_tx, mut first_waiter_entered_rx) = oneshot::channel();
        let release_first_waiter = Arc::new(Notify::new());
        let waiter_service = Arc::clone(&service);
        let waiter_target = target.clone();
        let waiter_release = Arc::clone(&release_first_waiter);
        let first_waiter = tokio::spawn(async move {
            let _guard = waiter_service.acquire(&waiter_target).await;
            let _ = first_waiter_entered_tx.send(());
            waiter_release.notified().await;
        });

        tokio::task::yield_now().await;
        assert!(first_waiter_entered_rx.try_recv().is_err());
        for name in ["first.txt", "second.txt", "third.txt"] {
            drop(service.acquire(&temp.path().join(name)).await);
        }

        let (contender_entered_tx, mut contender_entered_rx) = oneshot::channel();
        let contender_service = Arc::clone(&service);
        let contender_target = target.clone();
        let contender = tokio::spawn(async move {
            let _guard = contender_service.acquire(&contender_target).await;
            let _ = contender_entered_tx.send(());
        });

        tokio::task::yield_now().await;
        assert!(contender_entered_rx.try_recv().is_err());
        drop(first);
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            &mut first_waiter_entered_rx,
        )
        .await
        .expect("first waiter should acquire after the original guard drops")
        .expect("first waiter should report entry");
        assert!(contender_entered_rx.try_recv().is_err());

        release_first_waiter.notify_one();
        tokio::time::timeout(std::time::Duration::from_secs(1), &mut contender_entered_rx)
            .await
            .expect("contender should acquire after the first waiter drops")
            .expect("contender should report entry");
        first_waiter.await.expect("first waiter should finish");
        contender.await.expect("contender should finish");
        assert!(
            service
                .inner
                .lock()
                .expect("file lock map should not be poisoned")
                .locks
                .len()
                <= 2
        );
    }
}
