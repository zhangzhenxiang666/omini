use crate::execution::handle::OutputHandle;
use crate::execution::host::AgentHost;
use jiff::Timestamp;
use omini_domain::task::{TaskChangedEvent, TaskCompletion, TaskInfo, TaskOutputDelta, TaskStatus};
use omini_runtime_contract::RuntimeToServerEvent;
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
    output: OutputHandle,
    host: Arc<dyn AgentHost>,
    completion_tx: mpsc::UnboundedSender<TaskCompletion>,
    pending_notifications: Mutex<std::collections::HashSet<String>>,
    idle: Notify,
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
        output: OutputHandle,
        host: Arc<dyn AgentHost>,
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
            output,
            host,
            completion_tx,
            pending_notifications: Mutex::new(std::collections::HashSet::new()),
            idle: Notify::new(),
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

    /// 任务索引先落库、成功后广播；失败不广播，调用方按错误处理。
    async fn publish(&self, task: TaskInfo) -> Result<(), String> {
        self.records
            .lock()
            .expect("task lock poisoned")
            .insert(task.task_id.clone(), task.clone());
        self.host
            .upsert_background_task(&task)
            .await
            .map_err(|error| error.to_string())?;
        if !self
            .output
            .send_event(RuntimeToServerEvent::TaskChanged(TaskChangedEvent { task }))
            .await
        {
            return Err("agent output closed".to_string());
        }
        Ok(())
    }

    pub async fn output(&self, output: TaskOutputDelta) {
        let _ = self
            .output
            .send_event(RuntimeToServerEvent::TaskOutputDelta(output))
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
            task.updated_at = Timestamp::now();
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
        task.updated_at = Timestamp::now();
        task.completed_at = Some(task.updated_at);
        task.result_summary = Some(result_summary.clone());
        self.update(task.clone()).await?;
        self.notify_completed(TaskCompletion {
            task_id: task.task_id,
            kind: task.kind,
            label,
            title: task.title,
            status,
            summary: Some(completion_summary(&result_summary)),
        });
        Ok(())
    }

    pub fn notify_completed(&self, completion: TaskCompletion) {
        self.pending_notifications
            .lock()
            .expect("notification lock poisoned")
            .insert(completion.task_id.clone());
        let _ = self.completion_tx.send(completion);
    }

    pub(crate) fn has_pending_notifications(&self) -> bool {
        !self
            .pending_notifications
            .lock()
            .expect("notification lock poisoned")
            .is_empty()
    }

    pub(crate) fn mark_notifications_delivered(&self, ids: &[String]) {
        let mut pending = self
            .pending_notifications
            .lock()
            .expect("notification lock poisoned");
        for id in ids {
            pending.remove(id);
        }
    }

    pub(crate) fn has_active_tasks(&self) -> bool {
        *self
            .active_background
            .lock()
            .expect("background slot lock poisoned")
            > 0
    }

    pub(crate) fn cancel_all(&self) {
        let signals = self
            .cancellation
            .lock()
            .expect("task cancellation lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for signal in signals {
            signal.cancel();
        }
    }

    pub(crate) async fn wait_until_idle(&self) {
        loop {
            let notified = self.idle.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.has_active_tasks() {
                return;
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
        self.idle.notify_waiters();
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
    use crate::test_support::RecordingHost;
    use omini_domain::task::TaskKind;

    fn task(task_id: &str, owner: &str, status: TaskStatus) -> TaskInfo {
        let now = Timestamp::now();
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
        Arc<RecordingHost>,
    ) {
        let (output, events) = crate::execution::handle::OutputHandle::new(16);
        let host = Arc::new(crate::test_support::RecordingHost::default());
        let (completion_tx, _) = mpsc::unbounded_channel();
        (
            TaskManager::new(
                output,
                host.clone(),
                Vec::new(),
                max_background,
                completion_tx,
            ),
            events_into_runtime_events(events),
            host,
        )
    }

    /// 把输出流裁剪成纯事件流，供现有断言复用。
    fn events_into_runtime_events(
        mut events: crate::execution::AgentEvents,
    ) -> mpsc::Receiver<RuntimeToServerEvent> {
        let (tx, rx) = mpsc::channel(16);
        tokio::spawn(async move {
            while let Some(crate::execution::AgentOutput::Event(event)) = events.recv().await {
                let _ = tx.send(*event).await;
            }
        });
        rx
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
