//! 停止边界与唤醒隔离的确定性测试。
//!
//! 通过本地可控的 Provider 服务与可注入失败/暂停的 RecordingHost，直接
//! 装配 `AgentRuntime` 并从任务管理器注入完成通知，覆盖取消与完成的
//! 竞态边界：停止完成不唤醒、被取消运行不续跑、延迟启动被丢弃、旧完成
//! 在新输入后仍不得唤醒、通知在下一次显式输入前进入内存上下文。

use crate::execution::handle::{AgentEvents, InstanceState, OutputHandle, RunGate};
use crate::runtime::command::AgentCommand;
use crate::runtime::service::{AgentRuntime, AgentRuntimeParts, RuntimeCapabilityHandles};
use crate::test_support::{RecordingHost, TestTempDir};
use omini_config::{RawConfig, Settings};
use omini_domain::task::{TaskCompletion, TaskInfo, TaskKind, TaskStatus};
use omini_runtime_contract::RuntimeToServerEvent;
use omini_runtime_contract::thread_domain::{ActiveProfile, AgentTaskExecutionMode, AgentTaskInfo};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

fn settings_for(cwd: &Path, base_url: &str) -> Settings {
    let raw: RawConfig = toml::from_str(&format!(
        r#"
[providers.test]
protocol = "openai"
base_url = "{base_url}"
api_key = "test-key"

[providers.test.models.text-model]
context_window = 256000
thinking = false
input = ["text"]
"#
    ))
    .expect("test config should parse");
    raw.resolve()
        .expect("test config should resolve")
        .to_settings(Some("test"), Some("text-model"), None, cwd)
        .expect("test settings should build")
}

/// 按 HTTP 分帧读取一个请求:头部读到空行,再按 Content-Length 读满
/// 请求体。绝不能对活跃连接 `read_to_end`——客户端会保持连接等待
/// 响应,那样每个请求都要等到读超时才被服务器处理。
fn read_request(stream: &mut TcpStream) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "closed before headers",
            ));
        }
        buf.extend_from_slice(&chunk[..read]);
        if let Some(pos) = buf.windows(4).position(|window| window == b"\r\n\r\n") {
            break pos + 4;
        }
    };
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    while buf.len() < header_end + content_length {
        let read = stream.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
    }
    Ok(String::from_utf8_lossy(&buf).to_string())
}

/// 正常结束的本地 Provider 服务，并记录每个请求体供上下文断言。
fn spawn_recording_server(
    requests: usize,
) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let base_url = format!(
        "http://{}",
        listener.local_addr().expect("test server addr")
    );
    let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&bodies);
    let handle = thread::spawn(move || {
        for index in 0..requests {
            let (mut stream, _) = listener.accept().expect("accept test request");
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let body = read_request(&mut stream).expect("read framed test request");
            recorded
                .lock()
                .expect("recorded bodies lock poisoned")
                .push(body);
            let payload = format!(
                "data: {{\"choices\":[{{\"delta\":{{\"content\":\"answer {index}\"}},\"finish_reason\":null}}]}}\n\n\
                 data: {{\"choices\":[{{\"delta\":{{}},\"finish_reason\":\"stop\"}}],\"usage\":{{\"prompt_tokens\":1,\"completion_tokens\":1}}}}\n\n\
                 data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                payload.len(),
                payload
            );
            stream
                .write_all(response.as_bytes())
                .expect("write streaming response");
            stream.flush().expect("flush streaming response");
        }
    });
    (base_url, bodies, handle)
}

fn running_task(task_id: &str) -> TaskInfo {
    let now = jiff::Timestamp::now();
    TaskInfo {
        task_id: task_id.to_string(),
        owner_thread_id: "thread_main".to_string(),
        kind: TaskKind::Bash,
        title: task_id.to_string(),
        status: TaskStatus::Running,
        created_at: now,
        updated_at: now,
        completed_at: None,
        result_summary: None,
    }
}

fn completed_agent_task(task_id: &str) -> AgentTaskInfo {
    let now = jiff::Timestamp::now();
    AgentTaskInfo {
        task_id: task_id.to_string(),
        thread_id: format!("thread_{task_id}"),
        parent_run_id: None,
        parent_task_id: None,
        owner_thread_id: "thread_main".to_string(),
        parent_thread_id: "thread_main".to_string(),
        spawn_tool_use_id: format!("spawn_{task_id}"),
        agent: "general".to_string(),
        title: task_id.to_string(),
        depth: 1,
        execution_mode: AgentTaskExecutionMode::Background,
        status: TaskStatus::Completed,
        result: None,
        created_at: now,
        updated_at: now,
        completed_at: Some(now),
        // 未投递：实例启动后应恢复投递（保持既有唤醒行为）。
        notification_delivered: false,
    }
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

struct Harness {
    supervisor: Arc<crate::agent::AgentTaskSupervisor>,
    cmd_tx: mpsc::Sender<AgentCommand>,
    gate: Arc<RunGate>,
    events: AgentEvents,
    host: Arc<RecordingHost>,
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    _temp: TestTempDir,
    _server: thread::JoinHandle<()>,
}

fn assemble(
    background: Vec<TaskInfo>,
    agent_tasks: Vec<AgentTaskInfo>,
    requests: usize,
) -> (Harness, Arc<Mutex<Vec<String>>>) {
    let temp = TestTempDir::new("run-loop-tests");
    let (base_url, bodies, server) = spawn_recording_server(requests);
    let recording = Arc::new(RecordingHost::default());
    let host: Arc<dyn crate::execution::host::AgentHost> = Arc::clone(&recording) as _;
    let settings = settings_for(temp.path(), &base_url);
    let project = omini_config::project::ProjectDir::from_path(temp.path().to_path_buf());
    let (output, events) = OutputHandle::new(1024);
    let (cmd_tx, cmd_rx) = mpsc::channel(256);
    let gate = Arc::new(RunGate::default());
    let state = Arc::new(InstanceState::new(
        settings.clone(),
        ActiveProfile::Main,
        Arc::clone(&gate),
    ));
    let handles = RuntimeCapabilityHandles::load(&settings);
    let runtime = AgentRuntime::assemble(AgentRuntimeParts {
        settings,
        thread_dir: project.thread("thread_main"),
        project,
        thread_id: "thread_main".to_string(),
        messages: Vec::new(),
        llm_context_version: 1,
        usage: Default::default(),
        agent_tasks,
        background_tasks: background,
        host: Arc::clone(&host),
        output,
        cmd_rx,
        gate: Arc::clone(&gate),
        state,
        handles,
    });
    let supervisor = Arc::clone(&runtime.task_supervisor);
    let cancelled = Arc::clone(&runtime.cancelled);
    runtime.spawn_task();
    (
        Harness {
            supervisor,
            cmd_tx,
            gate,
            events,
            host: recording,
            cancelled,
            _temp: temp,
            _server: server,
        },
        bodies,
    )
}

/// 轮询等待条件成立;超时后由调用方的计数断言暴露失败。
async fn until<F>(mut predicate: F)
where
    F: FnMut() -> bool,
{
    for _ in 0..300 {
        if predicate() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn cancel_main(cmd_tx: &mpsc::Sender<AgentCommand>) {
    let (ack, result) = oneshot::channel();
    cmd_tx
        .send(AgentCommand::Cancel { run_id: None, ack })
        .await
        .expect("cancel command should send");
    result
        .await
        .expect("cancel ack should return")
        .expect("cancel should succeed");
}

async fn commit_user_run(harness: &Harness, text: &str) {
    let run_id = harness.gate.reserve().expect("gate should be free");
    let (ack, result) = oneshot::channel();
    harness
        .cmd_tx
        .send(AgentCommand::CommitRun {
            run_id,
            message: omini_model::message::Message::from_user_text(text.to_string()),
            ack,
        })
        .await
        .expect("commit command should send");
    result
        .await
        .expect("commit ack should return")
        .expect("commit should succeed");
}

/// 等到下一个 `RunFinished` 输出事件。
async fn wait_run_finished(events: &mut AgentEvents) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(output) = events.recv().await {
            if let crate::execution::AgentOutput::Event(event) = output
                && matches!(&*event, RuntimeToServerEvent::RunFinished)
            {
                return;
            }
        }
        panic!("event stream closed before RunFinished");
    })
    .await
    .expect("run must finish before timeout");
}

/// 给定停止边界已建立,当被停止任务完成,则通知立即结算落库一次,
/// 且不创建任何自动 Run（零内部 create_agent_run、零 Provider 请求）。
#[tokio::test]
async fn stopped_completion_settles() {
    let (harness, bodies) = assemble(vec![running_task("bash_1")], Vec::new(), 1);
    cancel_main(&harness.cmd_tx).await;

    harness
        .supervisor
        .task_manager()
        .notify_completed(completion("bash_1"));

    until(|| harness.host.call_count("insert_task_notification") == 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.host.call_count("insert_task_notification"), 1);
    assert_eq!(
        harness.host.call_count("create_agent_run"),
        0,
        "stopped completion must not create an automatic run"
    );
    assert!(
        bodies.lock().expect("bodies lock poisoned").is_empty(),
        "stopped completion must not trigger a provider request"
    );
    // 通知已交付：实例不再报告待处理通知，不影响空闲回收判定。
    assert!(
        !harness
            .supervisor
            .task_manager()
            .has_pending_notifications()
    );
}

/// 给定停止后用户发起新输入,当旧任务迟到完成,则只结算不唤醒;
/// 新代任务的完成恢复自动唤醒。
#[tokio::test]
async fn stale_completion_suppressed() {
    // 三个请求:两次用户运行 + 新代任务唤醒的内部运行。
    let (mut harness, _bodies) = assemble(vec![running_task("bash_1")], Vec::new(), 3);
    commit_user_run(&harness, "first question").await;
    wait_run_finished(&mut harness.events).await;
    cancel_main(&harness.cmd_tx).await;
    // 新显式输入关闭边界开启新代。
    commit_user_run(&harness, "second question").await;
    wait_run_finished(&mut harness.events).await;

    // 旧任务迟到完成：只结算,不唤醒。
    assert!(
        harness.supervisor.completion_is_stopped("bash_1"),
        "bash_1 must be inside the stop ledger after main cancel"
    );
    harness
        .supervisor
        .task_manager()
        .notify_completed(completion("bash_1"));
    until(|| harness.host.call_count("insert_task_notification") == 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        harness.host.call_count("create_agent_run"),
        0,
        "old completion must not start an internal run"
    );

    // 新代任务完成：恢复自动唤醒（内部 Run 必经 create_agent_run）。
    harness
        .supervisor
        .task_manager()
        .notify_completed(completion("bash_fresh"));
    until(|| harness.host.call_count("create_agent_run") == 1).await;
    wait_run_finished(&mut harness.events).await;
    assert_eq!(harness.host.call_count("create_agent_run"), 1);
}

/// 给定运行中已有入队的完成,当主取消到达,则被取消运行不因残留完成
/// 自动续跑：通知被结算落库,零后续 Run 与 Provider 请求。
#[tokio::test]
async fn cancelled_run_settles() {
    let recording = Arc::new(RecordingHost::default());
    let pause = recording.pause_append();
    let host: Arc<dyn crate::execution::host::AgentHost> = Arc::clone(&recording) as _;
    let temp = TestTempDir::new("cancelled-run");
    let (base_url, _bodies, server) = spawn_recording_server(1);
    let settings = settings_for(temp.path(), &base_url);
    let project = omini_config::project::ProjectDir::from_path(temp.path().to_path_buf());
    let (output, mut events) = OutputHandle::new(1024);
    let (cmd_tx, cmd_rx) = mpsc::channel(256);
    let gate = Arc::new(RunGate::default());
    let gate_for_test = Arc::clone(&gate);
    let state = Arc::new(InstanceState::new(
        settings.clone(),
        ActiveProfile::Main,
        Arc::clone(&gate),
    ));
    let handles = RuntimeCapabilityHandles::load(&settings);
    let runtime = AgentRuntime::assemble(AgentRuntimeParts {
        settings,
        thread_dir: project.thread("thread_main"),
        project,
        thread_id: "thread_main".to_string(),
        messages: Vec::new(),
        llm_context_version: 1,
        usage: Default::default(),
        agent_tasks: Vec::new(),
        background_tasks: vec![running_task("bash_1")],
        host,
        output,
        cmd_rx,
        gate,
        state,
        handles,
    });
    let supervisor = Arc::clone(&runtime.task_supervisor);
    runtime.spawn_task();
    let _server_guard = server;

    // 用户运行在首个持久化点挂起,期间注入一条边界前的完成。
    let run_id = runtime_reserve(&gate_for_test);
    send_commit(&cmd_tx, run_id, "work please").await;
    tokio::time::timeout(Duration::from_secs(5), pause.wait_entered())
        .await
        .expect("run must reach the persistence pause");
    supervisor
        .task_manager()
        .notify_completed(completion("bash_1"));
    until(|| supervisor.task_manager().has_pending_notifications()).await;
    // 主取消先发起再释放 barrier:持久化暂停会卡住 run_loop 主任务,
    // 同步 await 取消确认会在命令 select 前阻塞测试线程,形成死等。
    // cancel_main 内部已断言确认成功,这里只需等它完成。
    let cancel_tx = cmd_tx.clone();
    let cancel = tokio::spawn(async move { cancel_main(&cancel_tx).await });
    pause.release();
    cancel.await.expect("cancel task should not panic");
    wait_run_finished(&mut events).await;

    // 残留完成被结算落库,但不触发任何后续运行。
    until(|| recording.call_count("insert_task_notification") == 1).await;
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(recording.call_count("insert_task_notification"), 1);
    assert_eq!(recording.call_count("create_agent_run"), 0);
}

fn runtime_reserve(gate: &Arc<RunGate>) -> String {
    gate.reserve().expect("gate should be free")
}

async fn send_commit(cmd_tx: &mpsc::Sender<AgentCommand>, run_id: String, text: &str) {
    let (ack, result) = oneshot::channel();
    cmd_tx
        .send(AgentCommand::CommitRun {
            run_id,
            message: omini_model::message::Message::from_user_text(text.to_string()),
            ack,
        })
        .await
        .expect("commit command should send");
    result
        .await
        .expect("commit ack should return")
        .expect("commit should succeed");
}

/// 给定停止边界开放时丢弃被推迟的通知续跑,当预留释放,则闸门的
/// 待重试标记同步清除,实例回到可回收的空闲态。
#[tokio::test]
async fn deferred_start_dropped() {
    let (harness, bodies) = assemble(vec![running_task("bash_1")], Vec::new(), 1);
    // 给定宿主已预留运行槽位，完成通知触发真实的内部启动尝试，
    // 同时建立运行时的 deferred_start 与闸门的 deferred 标记。
    let reservation = harness.gate.reserve().expect("gate should be free");
    harness
        .supervisor
        .task_manager()
        .notify_completed(completion("bash_1"));
    until(|| harness.gate.has_deferred()).await;
    assert!(
        harness.gate.has_deferred(),
        "completion must defer its start"
    );

    // 当主停止到达，通知在释放预留之前就应结算；释放后运行循环
    // 必须丢弃真实的延迟启动，不能创建新 Run 或调用 Provider。
    cancel_main(&harness.cmd_tx).await;
    assert_eq!(harness.host.call_count("insert_task_notification"), 1);
    harness.gate.release(&reservation);

    until(|| !harness.gate.has_deferred()).await;
    // 则没有遗留的启动或通知标记，且结算不产生自动运行。
    assert!(!harness.gate.has_deferred());
    assert!(harness.gate.is_free());
    assert!(
        !harness
            .supervisor
            .task_manager()
            .has_pending_notifications()
    );
    assert_eq!(harness.host.call_count("insert_task_notification"), 1);
    assert_eq!(harness.host.call_count("create_agent_run"), 0);
    assert!(bodies.lock().expect("bodies lock poisoned").is_empty());
}

/// 给定停止通知已结算,当用户发起下一次输入,则通知消息先于新用户
/// 消息进入模型上下文（Provider 请求体中通知在前、用户消息在后）。
#[tokio::test]
async fn notification_precedes_input() {
    let (mut harness, bodies) = assemble(vec![running_task("bash_1")], Vec::new(), 1);
    cancel_main(&harness.cmd_tx).await;
    harness
        .supervisor
        .task_manager()
        .notify_completed(completion("bash_1"));
    until(|| harness.host.call_count("insert_task_notification") == 1).await;

    commit_user_run(&harness, "next question").await;
    wait_run_finished(&mut harness.events).await;
    let bodies = bodies.lock().expect("bodies lock poisoned").clone();
    assert_eq!(bodies.len(), 1, "exactly one provider request expected");
    let notification_pos = bodies[0]
        .find("task_notifications")
        .expect("notification text must precede the user message");
    let question_pos = bodies[0]
        .find("next question")
        .expect("user message must be in context");
    assert!(
        notification_pos < question_pos,
        "stopped notification must be injected before the new user message"
    );
}

/// 给定主取消标志残留（如压缩期间取消后未复位）,当新的显式输入提交,
/// 则标志被复位,新运行正常完成而不被立即取消。
#[tokio::test]
async fn commit_resets_cancelled() {
    let (mut harness, bodies) = assemble(Vec::new(), Vec::new(), 1);
    // 直接预置残留取消标志,模拟压缩期间主停止后的状态。
    harness
        .cancelled
        .store(true, std::sync::atomic::Ordering::Relaxed);
    commit_user_run(&harness, "fresh work").await;
    wait_run_finished(&mut harness.events).await;
    assert_eq!(
        bodies.lock().expect("bodies lock poisoned").len(),
        1,
        "fresh run must complete normally instead of being cancelled instantly"
    );
}

/// 给定恢复快照含未投递的终态后台任务,当实例启动,则保持原有的
/// 自动投递唤醒（非回归）。
#[tokio::test]
async fn recovered_notification_wakes() {
    let (mut harness, _bodies) =
        assemble(Vec::new(), vec![completed_agent_task("recovered_task")], 1);
    until(|| harness.host.call_count("create_agent_run") == 1).await;
    until(|| harness.host.call_count("insert_task_notification") == 1).await;
    wait_run_finished(&mut harness.events).await;
    assert_eq!(harness.host.call_count("create_agent_run"), 1);
}

/// 给定停止通知已结算交付,当同一任务的完成重复到达（重试/恢复路径）,
/// 则重复被整体压制：不再落库、不唤醒;下一次输入的上下文只含一份通知。
#[tokio::test]
async fn duplicate_notify_suppressed() {
    let (mut harness, bodies) = assemble(vec![running_task("bash_1")], Vec::new(), 1);
    cancel_main(&harness.cmd_tx).await;
    harness
        .supervisor
        .task_manager()
        .notify_completed(completion("bash_1"));
    until(|| harness.host.call_count("insert_task_notification") == 1).await;

    // 结算交付后的迟到重复：被交付身份压制,不再触发第二次落库或唤醒。
    harness
        .supervisor
        .task_manager()
        .notify_completed(completion("bash_1"));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(harness.host.call_count("insert_task_notification"), 1);
    assert_eq!(harness.host.call_count("create_agent_run"), 0);

    // 下一次显式输入：收件箱冲刷只注入一份通知（在用户消息之前）。
    commit_user_run(&harness, "after duplicate").await;
    wait_run_finished(&mut harness.events).await;
    let bodies = bodies.lock().expect("bodies lock poisoned").clone();
    assert_eq!(bodies.len(), 1);
    assert_eq!(
        bodies[0].matches("<task_notifications>").count(),
        1,
        "duplicate completion must not inject a second notification"
    );
}

/// 给定宿主闸门已含某任务而管理器在途账本尚未同步（跨实例/并发窗口）,
/// 当停止结算得到成功确认（`Ok(None)` 全部已交付或 `Some` 部分新交付）,
/// 则全部提交身份从在途集合退役——实例不残留待处理通知,可回到可回
/// 收的空闲态;注入与广播仍只含 fresh 内容。
#[tokio::test]
async fn settle_retires_pending() {
    let (mut harness, _bodies) =
        assemble(vec![running_task("a"), running_task("b")], Vec::new(), 1);
    // 模拟持久层交付位早于内存账本:宿主认为 a 已交付,管理器在途集合
    // 仍登记 a(其 notify 未被 manager 侧 delivered 压制)。
    harness.host.preset_delivered("a");
    cancel_main(&harness.cmd_tx).await;

    // 全部已交付路径(Ok(None)):提交身份 a 也必须退役。
    harness
        .supervisor
        .task_manager()
        .notify_completed(completion("a"));
    until(|| harness.host.call_count("insert_task_notification") == 1).await;
    assert!(
        !harness
            .supervisor
            .task_manager()
            .has_pending_notifications(),
        "successful Ok(None) ack must retire all submitted pending identities"
    );

    // 部分重叠路径(Some(fresh)):b 为新交付,同样退役。
    harness
        .supervisor
        .task_manager()
        .notify_completed(completion("b"));
    until(|| harness.host.call_count("insert_task_notification") == 2).await;
    assert!(
        !harness
            .supervisor
            .task_manager()
            .has_pending_notifications(),
        "successful Some(fresh) ack must retire all submitted pending identities"
    );
    // b 仍进入收件箱补内存,a 不重复注入:下一次输入只含 b 一份通知。
    commit_user_run(&harness, "after settle").await;
    wait_run_finished(&mut harness.events).await;
    assert_eq!(
        harness.host.call_count("create_agent_run"),
        0,
        "settled completions must not wake any run"
    );
}
