mod support;

use omini_core::util::file_lock::FileLockService;
use std::sync::Arc;
use tokio::sync::{Notify, oneshot};

#[tokio::test]
async fn file_lock_blocks_same_file_until_guard_drops() {
    let temp = support::TestTempDir::new("file-lock-same");
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
    let temp = support::TestTempDir::new("file-lock-paths");
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
