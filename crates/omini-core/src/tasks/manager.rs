use chrono::Utc;
use omini_domain::task::{TaskChangedEvent, TaskCompletion, TaskInfo, TaskOutputDelta, TaskStatus};
use omini_runtime_contract::RuntimeToServerEvent;
use omini_runtime_contract::persistence::RuntimePersistenceEvent;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc};

const RECENT_TASK_LIMIT: usize = 30;
pub(crate) const DEFAULT_MAX_BACKGROUND_TASKS: usize = 8;

/// 统一管理一个 owner thread 的后台任务索引、并发槽位和跨层生命周期事件。
pub(crate) struct TaskManager {
    records: Mutex<HashMap<String, TaskInfo>>,
    cancellation: Mutex<HashMap<String, TaskCancellation>>,
    active_background: Mutex<usize>,
    max_background: usize,
    changed: Notify,
    event_tx: mpsc::Sender<RuntimeToServerEvent>,
    persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
    completion_tx: mpsc::UnboundedSender<TaskCompletion>,
}

impl std::fmt::Debug for TaskManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskManager")
            .field(
                "task_count",
                &self.records.lock().expect("task lock poisoned").len(),
            )
            .field("max_background", &self.max_background)
            .finish_non_exhaustive()
    }
}

impl TaskManager {
    pub fn new(
        event_tx: mpsc::Sender<RuntimeToServerEvent>,
        persistence_tx: mpsc::Sender<RuntimePersistenceEvent>,
        initial: Vec<TaskInfo>,
        max_background: usize,
        completion_tx: mpsc::UnboundedSender<TaskCompletion>,
    ) -> Arc<Self> {
        let active_background = initial
            .iter()
            .filter(|task| task.status == TaskStatus::Running)
            .count();
        Arc::new(Self {
            records: Mutex::new(
                initial
                    .into_iter()
                    .map(|task| (task.task_id.clone(), task))
                    .collect(),
            ),
            cancellation: Mutex::new(HashMap::new()),
            active_background: Mutex::new(active_background),
            max_background,
            changed: Notify::new(),
            event_tx,
            persistence_tx,
            completion_tx,
        })
    }

    pub fn reserve_background(self: &Arc<Self>) -> Result<BackgroundTaskReservation, String> {
        let mut active = self
            .active_background
            .lock()
            .expect("background task slot lock poisoned");
        if *active >= self.max_background {
            return Err(format!(
                "background task limit reached: at most {} tasks may run concurrently",
                self.max_background
            ));
        }
        *active += 1;
        Ok(BackgroundTaskReservation {
            manager: Arc::clone(self),
        })
    }

    #[allow(dead_code)]
    pub fn list(
        &self,
        owner_thread_id: &str,
        status: Option<TaskStatus>,
        limit: usize,
    ) -> Vec<TaskInfo> {
        let records = self.records.lock().expect("task lock poisoned");
        let mut tasks = records
            .values()
            .filter(|task| task.owner_thread_id == owner_thread_id)
            .filter(|task| status.is_none_or(|status| task.status == status))
            .cloned()
            .collect::<Vec<_>>();
        tasks.sort_by_key(|task| std::cmp::Reverse(task.updated_at));
        tasks.truncate(limit.min(RECENT_TASK_LIMIT));
        tasks
    }

    pub fn get(&self, task_id: &str) -> Option<TaskInfo> {
        self.records
            .lock()
            .expect("task lock poisoned")
            .get(task_id)
            .cloned()
    }

    pub fn active_ids(&self, owner_thread_id: &str) -> Vec<String> {
        let records = self.records.lock().expect("task lock poisoned");
        let mut task_ids = records
            .values()
            .filter(|task| task.owner_thread_id == owner_thread_id && !task.status.is_terminal())
            .map(|task| task.task_id.clone())
            .collect::<Vec<_>>();
        task_ids.sort();
        task_ids
    }

    pub fn event_sender(&self) -> &mpsc::Sender<RuntimeToServerEvent> {
        &self.event_tx
    }

    pub fn persistence_sender(&self) -> &mpsc::Sender<RuntimePersistenceEvent> {
        &self.persistence_tx
    }

    pub async fn register(
        &self,
        task: TaskInfo,
        cancellation: Option<TaskCancellation>,
    ) -> Result<(), String> {
        if let Some(cancellation) = cancellation {
            self.cancellation
                .lock()
                .expect("task cancellation lock poisoned")
                .insert(task.task_id.clone(), cancellation);
        }
        self.publish(task).await
    }

    pub async fn update(&self, task: TaskInfo) -> Result<(), String> {
        if task.status.is_terminal() {
            self.cancellation
                .lock()
                .expect("task cancellation lock poisoned")
                .remove(&task.task_id);
        }
        self.publish(task).await
    }

    async fn publish(&self, task: TaskInfo) -> Result<(), String> {
        self.records
            .lock()
            .expect("task lock poisoned")
            .insert(task.task_id.clone(), task.clone());
        self.persistence_tx
            .send(RuntimePersistenceEvent::UpsertTask { task: task.clone() })
            .await
            .map_err(|_| "task persistence channel closed".to_string())?;
        self.event_tx
            .send(RuntimeToServerEvent::TaskChanged(TaskChangedEvent { task }))
            .await
            .map_err(|_| "task event channel closed".to_string())?;
        self.changed.notify_waiters();
        Ok(())
    }

    pub async fn output(&self, output: TaskOutputDelta) {
        let _ = self
            .event_tx
            .send(RuntimeToServerEvent::TaskOutputDelta(output))
            .await;
    }

    pub async fn cancel(&self, task_id: &str) -> Result<TaskInfo, String> {
        let cancellation = self
            .cancellation
            .lock()
            .expect("task cancellation lock poisoned")
            .get(task_id)
            .cloned()
            .ok_or_else(|| format!("task '{task_id}' is not cancellable"))?;
        cancellation.cancel();
        let mut task = self
            .get(task_id)
            .ok_or_else(|| format!("unknown task '{task_id}'"))?;
        if !task.status.is_terminal() {
            task.status = TaskStatus::Cancelling;
            task.updated_at = Utc::now();
            self.update(task.clone()).await?;
        }
        Ok(task)
    }

    pub async fn complete(
        &self,
        task_id: &str,
        status: TaskStatus,
        label: String,
        result_summary: String,
    ) -> Result<(), String> {
        let mut task = self
            .get(task_id)
            .ok_or_else(|| format!("unknown task '{task_id}'"))?;
        task.status = status;
        task.updated_at = Utc::now();
        task.completed_at = Some(task.updated_at);
        task.result_summary = Some(result_summary.clone());
        self.update(task.clone()).await?;
        self.notify_completed(TaskCompletion {
            task_id: task.task_id,
            label,
            title: task.title,
            status,
            summary: Some(completion_summary(&result_summary)),
        });
        Ok(())
    }

    pub fn notify_completed(&self, completion: TaskCompletion) {
        let _ = self.completion_tx.send(completion);
    }

    pub async fn wait_for(
        &self,
        owner_thread_id: &str,
        task_ids: &[String],
    ) -> Result<Vec<TaskInfo>, String> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let (terminal, result) = {
                let records = self.records.lock().expect("task lock poisoned");
                let mut result = Vec::with_capacity(task_ids.len());
                let mut terminal = true;
                for id in task_ids {
                    let task = records
                        .get(id)
                        .ok_or_else(|| format!("unknown task '{id}'"))?;
                    if task.owner_thread_id != owner_thread_id {
                        return Err(format!("unknown task '{id}'"));
                    }
                    terminal &= task.status.is_terminal();
                    result.push(task.clone());
                }
                (terminal, result)
            };
            if terminal {
                return Ok(result);
            }
            notified.await;
        }
    }

    fn release_background(&self) {
        let mut active = self
            .active_background
            .lock()
            .expect("background task slot lock poisoned");
        *active = active
            .checked_sub(1)
            .expect("releasing an unreserved background task slot");
    }
}

fn completion_summary(summary: &str) -> String {
    const MAX_SUMMARY_BYTES: usize = 8 * 1024;
    if summary.len() <= MAX_SUMMARY_BYTES {
        return summary.to_string();
    }
    let mut end = MAX_SUMMARY_BYTES;
    while !summary.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}\n[output summary truncated]", &summary[..end])
}

/// 可由任意任务适配器持有并交给管理器的通用取消信号。
#[derive(Clone)]
pub(crate) struct TaskCancellation {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl TaskCancellation {
    pub fn new(cancelled: Arc<AtomicBool>, notify: Arc<Notify>) -> Self {
        Self { cancelled, notify }
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
        self.notify.notify_waiters();
    }
}

/// Drop 时释放一个共享后台任务槽位。
pub(crate) struct BackgroundTaskReservation {
    manager: Arc<TaskManager>,
}

impl Drop for BackgroundTaskReservation {
    fn drop(&mut self) {
        self.manager.release_background();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omini_domain::task::TaskKind;

    fn task(task_id: &str, owner: &str, status: TaskStatus) -> TaskInfo {
        let now = Utc::now();
        TaskInfo {
            task_id: task_id.to_string(),
            owner_thread_id: owner.to_string(),
            kind: TaskKind::Bash,
            title: task_id.to_string(),
            status,
            created_at: now,
            updated_at: now,
            completed_at: status.is_terminal().then_some(now),
            result_summary: None,
        }
    }

    fn manager(
        max_background: usize,
    ) -> (
        Arc<TaskManager>,
        mpsc::Receiver<RuntimeToServerEvent>,
        mpsc::Receiver<RuntimePersistenceEvent>,
    ) {
        let (event_tx, event_rx) = mpsc::channel(16);
        let (persistence_tx, persistence_rx) = mpsc::channel(16);
        let (completion_tx, _) = mpsc::unbounded_channel();
        (
            TaskManager::new(
                event_tx,
                persistence_tx,
                Vec::new(),
                max_background,
                completion_tx,
            ),
            event_rx,
            persistence_rx,
        )
    }

    #[tokio::test]
    async fn background_slots_release_and_lists_are_owner_and_status_scoped() {
        let (manager, _events, _persistence) = manager(1);
        let reservation = manager
            .reserve_background()
            .expect("first slot is available");
        assert!(manager.reserve_background().is_err());
        drop(reservation);
        assert!(manager.reserve_background().is_ok());

        manager
            .register(task("a", "owner_a", TaskStatus::Running), None)
            .await
            .unwrap();
        manager
            .register(task("b", "owner_a", TaskStatus::Completed), None)
            .await
            .unwrap();
        manager
            .register(task("c", "owner_b", TaskStatus::Running), None)
            .await
            .unwrap();

        assert_eq!(manager.list("owner_a", None, 10).len(), 2);
        assert_eq!(
            manager.list("owner_a", Some(TaskStatus::Running), 10)[0].task_id,
            "a"
        );
        assert_eq!(manager.list("owner_b", None, 10)[0].task_id, "c");
    }

    #[tokio::test]
    async fn wait_rejects_tasks_owned_by_another_thread_and_returns_terminal_state() {
        let (manager, _events, _persistence) = manager(1);
        manager
            .register(task("owned", "owner_a", TaskStatus::Completed), None)
            .await
            .unwrap();
        let ids = vec!["owned".to_string()];

        assert!(manager.wait_for("owner_b", &ids).await.is_err());
        assert_eq!(
            manager.wait_for("owner_a", &ids).await.unwrap()[0].status,
            TaskStatus::Completed
        );
    }

    #[tokio::test]
    async fn cancellation_uses_an_adapter_supplied_generic_signal() {
        let (manager, _events, _persistence) = manager(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let notify = Arc::new(Notify::new());
        manager
            .register(
                task("cancel_me", "owner_a", TaskStatus::Running),
                Some(TaskCancellation::new(
                    Arc::clone(&cancelled),
                    Arc::clone(&notify),
                )),
            )
            .await
            .unwrap();

        let task = manager.cancel("cancel_me").await.unwrap();

        assert!(cancelled.load(Ordering::Relaxed));
        assert_eq!(task.status, TaskStatus::Cancelling);
    }
}
