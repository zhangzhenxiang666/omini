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

/// 主取消建立的停止边界账本：边界内任务的完成通知不得自动唤醒主运行。
///
/// 边界同时捕获两类旧工作身份：尚在运行的任务，以及已终态但完成通知
/// 仍未交付的任务（其完成可能在边界关闭后才到达消费端）。边界开放期间
/// 注册的任务（被停止工作仍在创建的后代）同样归入停止集合；新的显式
/// 用户输入关闭边界开启新代，但停止集合中的旧任务迟到完成依然不得
/// 重新获得唤醒资格。
#[derive(Debug, Default)]
struct StopLedger {
    stopped: std::collections::HashSet<String>,
    open: bool,
    /// 单调递增的停止代数:每次建立停止边界递增一次,永不复位。
    /// 供跨 await 的注册路径在等待前后比较——代数变化即说明等待期间
    /// 发生过停止边界建立(即便随后被新输入关闭),迟到的注册不得逃过。
    generation: u64,
}

/// 通知交付账本：在途（已发出、未结算交付）与已交付的任务身份。
///
/// 两类身份由同一把锁持有：交付标记的 pending→delivered 转移与新完成的
/// 在途登记在各自临界区内原子完成，重复完成不可能落进“既非在途也非
/// 已交付”或“同时在途又已交付”的中间状态。
#[derive(Debug, Default)]
struct NotificationLedger {
    pending: std::collections::HashSet<String>,
    /// 已交付（已落库并/或已注入内存）的任务身份，跨整个实例生命周期
    /// 保留：迟到的重复完成据此被整体压制，不会重新入队或再次注入。
    delivered: std::collections::HashSet<String>,
}

/// 统一管理一个 owner thread 的后台任务索引、并发槽位和跨层生命周期事件。
pub(crate) struct TaskManager {
    records: Mutex<HashMap<String, TaskInfo>>,
    cancellation: Mutex<HashMap<String, TaskCancellation>>,
    active_background: Mutex<usize>,
    max_background: usize,
    output: OutputHandle,
    host: Arc<dyn AgentHost>,
    completion_tx: mpsc::UnboundedSender<TaskCompletion>,
    notification_ledger: Mutex<NotificationLedger>,
    stop_ledger: Mutex<StopLedger>,
    /// 停止通知收件箱：已结算持久化（或待重试）的停止完成，按到达顺序
    /// 保存，等待下一次显式输入前注入内存模型上下文。与引擎队列隔离，
    /// 避免旧完成在运行边界触发额外续跑。
    stopped_inbox: Mutex<Vec<TaskCompletion>>,
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
            notification_ledger: Mutex::new(NotificationLedger::default()),
            stop_ledger: Mutex::new(StopLedger::default()),
            stopped_inbox: Mutex::new(Vec::new()),
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

    /// 注册后台任务；`inherit_stop` 表示该任务是被停止工作的后代（按父
    /// 身份继承停止状态），须无条件归入停止集合。
    ///
    /// 归入停止集合的任务同时会被实际取消（触发传入的取消信号），而非
    /// 只压制其完成通知——包括停止边界开放期间迟到的根任务注册。
    pub async fn register(
        &self,
        task: TaskInfo,
        cancellation: Option<TaskCancellation>,
        inherit_stop: bool,
    ) -> Result<(), String> {
        let register_cancel = cancellation.clone();
        if let Some(cancellation) = register_cancel {
            self.cancellation
                .lock()
                .expect("task cancellation lock poisoned")
                .insert(task.task_id.clone(), cancellation);
        }
        self.publish(task, inherit_stop, cancellation.as_ref())
            .await
    }

    pub async fn update(&self, task: TaskInfo) -> Result<(), String> {
        if task.status.is_terminal() {
            self.cancellation
                .lock()
                .expect("task cancellation lock poisoned")
                .remove(&task.task_id);
        }
        self.publish(task, false, None).await
    }

    /// 任务索引先落库、成功后广播；失败不广播，调用方按错误处理。
    ///
    /// 停止归集与索引写入在同一临界区内完成：`open_stop_boundary` 的快照
    /// 持有同一把任务索引锁，注册与边界建立严格串行化，消除“检查时边界
    /// 尚未开放、发布时已开放”的跨界逃逸。
    async fn publish(
        &self,
        task: TaskInfo,
        inherit_stop: bool,
        cancellation: Option<&TaskCancellation>,
    ) -> Result<(), String> {
        {
            let mut records = self.records.lock().expect("task lock poisoned");
            let mut stop_now = inherit_stop;
            {
                let mut ledger = self.stop_ledger.lock().expect("stop ledger lock poisoned");
                if inherit_stop || ledger.open {
                    ledger.stopped.insert(task.task_id.clone());
                    stop_now = true;
                }
            }
            records.insert(task.task_id.clone(), task.clone());
            if stop_now && let Some(cancellation) = cancellation {
                // 归集与取消在同一临界区完成：注册逃过边界的窗口不存在，
                // 注册成功即意味着取消信号已被触发。
                cancellation.cancel();
            }
        }
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

    /// 完成通知进入待交付集合并唤醒消费端；在途或已交付的重复完成被
    /// 整体压制（不重复发送、不重复入队）。
    ///
    /// 已交付检查必须先于在途登记：若顺序颠倒，已交付的重复完成会把
    /// 身份留在待交付集合又不发送通道，`has_pending_notifications` 将
    /// 永远报告待处理工作，实例无法回到可回收的空闲态。
    pub fn notify_completed(&self, completion: TaskCompletion) {
        {
            let mut ledger = self
                .notification_ledger
                .lock()
                .expect("notification ledger lock poisoned");
            if ledger.delivered.contains(&completion.task_id)
                || !ledger.pending.insert(completion.task_id.clone())
            {
                return;
            }
        }
        let _ = self.completion_tx.send(completion);
    }

    pub(crate) fn has_pending_notifications(&self) -> bool {
        !self
            .notification_ledger
            .lock()
            .expect("notification ledger lock poisoned")
            .pending
            .is_empty()
    }

    /// 该任务的通知是否已交付（落库并/或注入内存）。
    ///
    /// 收件箱冲刷据此区分“可直接注入内存”与“仍需重试持久化”。
    pub(crate) fn notification_is_delivered(&self, task_id: &str) -> bool {
        self.notification_ledger
            .lock()
            .expect("notification ledger lock poisoned")
            .delivered
            .contains(task_id)
    }

    /// 停止完成进入收件箱等待下一次显式输入前注入内存上下文。
    ///
    /// 按任务去重：同一任务的重复完成（重试/恢复路径）只保留首份，
    /// 防止同一通知被注入两次。
    pub(crate) fn enqueue_stopped_completion(&self, completion: TaskCompletion) {
        let mut inbox = self
            .stopped_inbox
            .lock()
            .expect("stopped inbox lock poisoned");
        if inbox
            .iter()
            .any(|existing| existing.task_id == completion.task_id)
        {
            return;
        }
        inbox.push(completion);
    }

    /// 取走收件箱全部停止完成（保序）。
    pub(crate) fn take_stopped_inbox(&self) -> Vec<TaskCompletion> {
        std::mem::take(
            &mut *self
                .stopped_inbox
                .lock()
                .expect("stopped inbox lock poisoned"),
        )
    }

    /// 交付标记的 pending→delivered 转移在单一临界区内原子完成。
    ///
    /// 若分两段加锁，重复的 `notify_completed` 可能在“已移出在途、尚未
    /// 登记交付”的窗口里把同一完成再次发送到通道，造成重复注入。
    pub(crate) fn mark_notifications_delivered(&self, ids: &[String]) {
        let mut ledger = self
            .notification_ledger
            .lock()
            .expect("notification ledger lock poisoned");
        for id in ids {
            ledger.pending.remove(id);
            ledger.delivered.insert(id.clone());
        }
        // 注意：停止集合在此刻意保留。已结算的通知进入收件箱等待下一
        // 次显式输入注入内存上下文；若此时把它们移出停止集合，它们会
        // 重新获得唤醒资格，违背停止边界的语义。
    }

    pub(crate) fn has_active_tasks(&self) -> bool {
        *self
            .active_background
            .lock()
            .expect("background slot lock poisoned")
            > 0
    }

    /// 建立停止边界：当前索引中全部已知任务（无论终态）立即失去自动唤醒
    /// 资格；此后注册的任务（被停止工作的后代）同样归入停止集合。必须在
    /// 发出取消信号之前调用。
    ///
    /// 全量捕获是有意为之：任务记录写入与完成通知入队之间存在窗口
    /// （终态先落索引、后发通知），按终态过滤会漏掉“已终态但通知未发出”
    /// 的任务，其通知在边界关闭后到达就会重新唤醒。多捕获的已交付历史
    /// 任务不会再完成，无行为影响。
    ///
    /// 快照与 `publish` 的注册写入共用任务索引锁（方向统一为索引→账本），
    /// 边界建立与任务注册严格串行化，二者不存在跨界逃逸窗口。
    pub(crate) fn open_stop_boundary(&self) {
        let records = self.records.lock().expect("task lock poisoned");
        let mut ledger = self.stop_ledger.lock().expect("stop ledger lock poisoned");
        ledger.open = true;
        ledger.generation += 1;
        for task_id in records.keys() {
            ledger.stopped.insert(task_id.clone());
        }
    }

    /// 当前停止代数;见 [`StopLedger::generation`]。
    pub(crate) fn stop_generation(&self) -> u64 {
        self.stop_ledger
            .lock()
            .expect("stop ledger lock poisoned")
            .generation
    }

    /// 新的显式用户输入关闭边界、开启新代：此后注册与完成的任务恢复
    /// 正常唤醒资格；停止集合保留，旧任务迟到完成仍不得唤醒。
    pub(crate) fn close_stop_boundary(&self) {
        self.stop_ledger
            .lock()
            .expect("stop ledger lock poisoned")
            .open = false;
    }

    pub(crate) fn stop_boundary_open(&self) -> bool {
        self.stop_ledger
            .lock()
            .expect("stop ledger lock poisoned")
            .open
    }

    /// 完成是否属于被停止的工作：边界开放期间一律成立（覆盖先于边界
    /// 完成但尚未被消费的滞留通知），否则按停止集合逐任务判定。
    pub(crate) fn completion_is_stopped(&self, task_id: &str) -> bool {
        let ledger = self.stop_ledger.lock().expect("stop ledger lock poisoned");
        ledger.open || ledger.stopped.contains(task_id)
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
            .register(task("a", "owner_a", TaskStatus::Running), None, false)
            .await
            .unwrap();
        manager
            .register(task("b", "owner_a", TaskStatus::Completed), None, false)
            .await
            .unwrap();
        manager
            .register(task("c", "owner_b", TaskStatus::Running), None, false)
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
                false,
            )
            .await
            .unwrap();

        let task = manager.cancel("cancel_me").await.unwrap();

        assert!(cancelled.load(Ordering::Relaxed));
        assert_eq!(task.status, TaskStatus::Cancelling);
    }

    fn completion(task_id: &str) -> TaskCompletion {
        TaskCompletion {
            task_id: task_id.to_string(),
            kind: TaskKind::Bash,
            label: "Bash".to_string(),
            title: task_id.to_string(),
            status: TaskStatus::Completed,
            summary: None,
        }
    }

    /// 给定停止边界建立,当检查全部已知任务（含已终态、通知未发出的）
    /// 与边界期间注册的后代,则它们全部失去唤醒资格;新代任务恢复资格。
    #[tokio::test]
    async fn stop_boundary_captures() {
        let (manager, _events, _persistence) = manager(8);
        manager
            .register(task("running", "owner_a", TaskStatus::Running), None, false)
            .await
            .unwrap();
        // 已终态但通知尚未发出的任务:终态落库与通知入队之间存在窗口,
        // 边界必须按“全部已知身份”捕获而不能按终态过滤。
        manager
            .register(task("done", "owner_a", TaskStatus::Completed), None, false)
            .await
            .unwrap();

        manager.open_stop_boundary();
        // 边界开放期间注册的迟到后代（被停止工作仍在创建）同样归入。
        manager
            .register(
                task("late_child", "owner_a", TaskStatus::Running),
                None,
                false,
            )
            .await
            .unwrap();
        manager.close_stop_boundary();

        for task_id in ["running", "done", "late_child"] {
            assert!(
                manager.completion_is_stopped(task_id),
                "{task_id} must lose wake eligibility"
            );
        }

        // 新代（关闭后注册）恢复正常资格。
        manager
            .register(task("fresh", "owner_a", TaskStatus::Running), None, false)
            .await
            .unwrap();
        assert!(!manager.completion_is_stopped("fresh"));

        // 边界关闭后注册、但按父身份继承停止状态的迟到同步后代。
        manager
            .register(task("orphan", "owner_a", TaskStatus::Running), None, true)
            .await
            .unwrap();
        assert!(manager.completion_is_stopped("orphan"));

        // 停止代数随每次边界建立单调递增且不因关闭回退:跨 await 的
        // 注册路径据此识别等待期间发生过的停止。
        let generation = manager.stop_generation();
        manager.open_stop_boundary();
        assert!(manager.stop_generation() > generation);
        manager.close_stop_boundary();
        assert_eq!(manager.stop_generation(), generation + 1);
    }

    /// 给定停止收件箱,当同一任务的完成重复入箱,则只保留首份,防止注入两次。
    #[tokio::test]
    async fn stopped_inbox_dedupes() {
        let (manager, _events, _persistence) = manager(8);
        manager.enqueue_stopped_completion(completion("dup"));
        manager.enqueue_stopped_completion(completion("dup"));
        manager.enqueue_stopped_completion(completion("other"));

        let inbox = manager.take_stopped_inbox();
        assert_eq!(inbox.len(), 2);
        assert_eq!(inbox[0].task_id, "dup");
        assert_eq!(inbox[1].task_id, "other");
        assert!(manager.take_stopped_inbox().is_empty());
    }

    /// 给定通知已交付,当同一任务的完成重复到达,则既不进入在途集合
    /// (无残留待处理工作)也不再次发送通道完成。
    #[tokio::test]
    async fn delivered_duplicate_suppressed() {
        let (output, _events) = crate::execution::handle::OutputHandle::new(16);
        let host = Arc::new(RecordingHost::default());
        let (completion_tx, mut completion_rx) = mpsc::unbounded_channel();
        let manager = TaskManager::new(output, host, Vec::new(), 8, completion_tx);

        // 首次完成:登记在途并唤醒消费端。
        manager.notify_completed(completion("task_a"));
        assert!(manager.has_pending_notifications());
        assert!(completion_rx.try_recv().is_ok());

        // 交付结算:在途清空,身份转入已交付。
        manager.mark_notifications_delivered(&["task_a".to_string()]);
        assert!(!manager.has_pending_notifications());

        // 迟到的重复完成:不得留下待处理工作,也不得再次发送通道完成。
        manager.notify_completed(completion("task_a"));
        assert!(
            !manager.has_pending_notifications(),
            "delivered duplicate must not leave pending work"
        );
        assert!(
            completion_rx.try_recv().is_err(),
            "delivered duplicate must not enqueue a channel completion"
        );
    }
}
