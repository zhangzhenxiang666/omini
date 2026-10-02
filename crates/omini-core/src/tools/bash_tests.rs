use super::*;
use crate::agent::AgentRegistry;
use crate::execution::handle::OutputHandle;
use crate::execution::{AgentEvents, AgentOutput};
use crate::skills::SkillRegistry;
use crate::test_support::{RecordingHost, TestTempDir};
use crate::tools::ToolRuntimeContext;
use omini_domain::task::TaskCompletion;
use omini_runtime_contract::RuntimeToServerEvent;
use std::collections::HashMap;
use std::sync::atomic::AtomicI64;

struct Harness {
    context: ToolExecutionContext,
    manager: Arc<TaskManager>,
    host: Arc<RecordingHost>,
    events: Option<AgentEvents>,
    completions: mpsc::UnboundedReceiver<TaskCompletion>,
}

impl Harness {
    fn new(depth: u8, capacity: usize) -> Self {
        let (output, events) = OutputHandle::new(64);
        let host = Arc::new(RecordingHost::default());
        let (completion_tx, completions) = mpsc::unbounded_channel();
        let manager = TaskManager::new(output, host.clone(), Vec::new(), capacity, completion_tx);
        let mut context = ToolExecutionContext::test("bash");
        let project = omini_config::project::ProjectDir::from_path(std::env::temp_dir());
        context.runtime = Some(Arc::new(ToolRuntimeContext {
            thread_id: "owner".into(),
            run_id: None,
            thread_type: "main".into(),
            agent_label: None,
            thread_dir: project.thread("owner"),
            llm_context_version: Arc::new(AtomicI64::new(1)),
            agent_depth: depth,
            task_id: None,
            owner_thread_id: "owner".into(),
            agent_registry: Arc::new(AgentRegistry {
                agents: HashMap::new(),
                diagnostics: Vec::new(),
            }),
            skill_registry: Arc::new(SkillRegistry {
                skills: HashMap::new(),
                diagnostics: Vec::new(),
            }),
            task_manager: Some(Arc::clone(&manager)),
            task_supervisor: None,
            project,
        }));
        Self {
            context,
            manager,
            host,
            events: Some(events),
            completions,
        }
    }

    /// 等待真实后台消费者完成并释放槽位，避免测试留下长命令。
    async fn finish(&mut self) -> TaskCompletion {
        let completion = tokio::time::timeout(Duration::from_secs(3), self.completions.recv())
            .await
            .expect("后台任务应及时完成")
            .expect("应收到完成通知");
        tokio::time::timeout(Duration::from_secs(3), self.manager.wait_until_idle())
            .await
            .expect("后台槽位应释放");
        completion
    }
}

fn input(command: &str, background: Option<bool>, timeout: Option<u64>) -> BashInput {
    let mut value = serde_json::json!({"command": command, "timeout": timeout});
    if let Some(background) = background {
        value["background"] = background.into();
    }
    serde_json::from_value(value).unwrap()
}

fn assert_started(result: &ToolResult) {
    assert!(!result.is_error, "{}", result.output);
    let value: serde_json::Value = serde_json::from_str(&result.output).unwrap();
    assert_eq!(value["task_id"], "test_bash");
    assert_eq!(value["status"], "running");
    assert_eq!(value["background"], true);
    assert!(value["elapsed_ms"].is_number());
}

/// 给定旧调用和显式 false，当快速命令执行，则返回原来的前台输出。
#[tokio::test]
async fn preserve_foreground_defaults() {
    let schema = schemars::schema_for!(BashInput).to_value();
    assert_eq!(schema["properties"]["background"]["type"], "boolean");
    assert_eq!(schema["properties"]["background"]["default"], false);
    assert!(
        !schema["required"]
            .as_array()
            .unwrap()
            .contains(&"background".into())
    );
    for background in [None, Some(false)] {
        let harness = Harness::new(0, 1);
        let result = BashTool
            .call(
                input("printf foreground", background, None),
                harness.context,
            )
            .await;
        assert!(!result.is_error);
        assert_eq!(result.output, "foreground");
        assert!(harness.manager.list("owner", None, 10).is_empty());
    }
}

/// 给定显式后台请求，当快速命令成功或非零退出，则也走后台输出和通知链路。
#[tokio::test]
async fn deliver_background_results() {
    for (code, expected) in [(0, TaskStatus::Completed), (7, TaskStatus::Failed)] {
        let mut harness = Harness::new(0, 1);
        let result = BashTool
            .call(
                input(
                    &format!("printf stdout; printf stderr >&2; exit {code}"),
                    Some(true),
                    None,
                ),
                harness.context.clone(),
            )
            .await;
        assert_started(&result);
        let completion = harness.finish().await;
        assert_eq!(completion.status, expected);
        let task = harness.manager.get("test_bash").unwrap();
        assert_eq!(task.kind, TaskKind::Bash);
        assert_eq!(task.owner_thread_id, "owner");
        assert_eq!(task.status, expected);
        assert_eq!(
            task.result_summary.as_deref(),
            Some(format!("Exit code: {code}\nstdout\nstderr").as_str())
        );
        assert!(harness.manager.reserve_background().is_ok());

        let mut streams = HashMap::new();
        let mut statuses = Vec::new();
        let events = harness.events.as_mut().unwrap();
        // 完成通知在终态事件发送后发出，因此此处可读到完整事件序列。
        while let Some(AgentOutput::Event(event)) = events.recv().await {
            match *event {
                RuntimeToServerEvent::TaskOutputDelta(delta) => {
                    streams
                        .entry(format!("{:?}", delta.stream))
                        .or_insert_with(String::new)
                        .push_str(&delta.delta);
                }
                RuntimeToServerEvent::TaskChanged(event) => {
                    statuses.push(event.task.status);
                    if event.task.status.is_terminal() {
                        break;
                    }
                }
                _ => {}
            }
        }
        assert_eq!(streams["Stdout"], "stdout");
        assert_eq!(streams["Stderr"], "stderr");
        assert_eq!(statuses, vec![TaskStatus::Running, expected]);
        assert_eq!(harness.host.call_count("upsert_background_task"), 2);
        assert_eq!(harness.host.call_count("create_agent_run"), 0);
        assert!(harness.completions.try_recv().is_err());
    }
}

/// 给定后台能力不足或请求已取消，当显式请求执行，则命令不产生文件副作用。
#[tokio::test]
async fn reject_background_requests() {
    for case in ["runtime", "child", "manager", "capacity", "cancelled"] {
        let dir = TestTempDir::new(case);
        let mut harness = Harness::new(u8::from(case == "child"), usize::from(case != "capacity"));
        match case {
            "runtime" => harness.context.runtime = None,
            "manager" => {
                Arc::make_mut(harness.context.runtime.as_mut().unwrap()).task_manager = None
            }
            "cancelled" => harness.context.cancelled.store(true, Ordering::Relaxed),
            _ => {}
        }
        let mut command = input("printf executed > marker", Some(true), None);
        command.workdir = Some(dir.path().to_string_lossy().into_owned());
        let result = BashTool.call(command, harness.context).await;
        assert!(result.is_error, "{case}");
        assert!(
            result.output.contains("command was not started"),
            "{}",
            result.output
        );
        assert!(!dir.path().join("marker").exists(), "{case}");
        assert!(!harness.manager.has_active_tasks());
        assert!(harness.manager.list("owner", None, 10).is_empty());
    }
}

/// 给定进程启动失败，当返回工具错误，则预留槽位立即释放且无任务记录。
#[tokio::test]
async fn release_spawn_failure() {
    let harness = Harness::new(0, 1);
    let dir = TestTempDir::new("bash-spawn");
    let mut command = input("printf unreachable", Some(true), None);
    command.workdir = Some(dir.path().join("missing").to_string_lossy().into_owned());
    let result = BashTool.call(command, harness.context).await;
    assert!(result.is_error);
    assert!(result.output.contains("Failed to spawn shell"));
    assert!(!harness.manager.has_active_tasks());
    assert!(harness.manager.reserve_background().is_ok());
    assert!(harness.manager.list("owner", None, 10).is_empty());
}

/// 给定持久化或事件发送失败，当注册已启动的长命令失败，则回收进程并收敛终态。
#[tokio::test]
async fn settle_registration_failure() {
    for case in ["storage", "output"] {
        let mut harness = Harness::new(0, 1);
        if case == "storage" {
            harness
                .host
                .fail_operation("upsert_background_task", "storage unavailable");
        } else {
            drop(harness.events.take());
        }
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            BashTool.call(
                input("sleep 60; printf unreachable", Some(true), None),
                harness.context.clone(),
            ),
        )
        .await
        .expect("注册失败应取消并回收进程，而非等命令自然结束");
        assert!(result.is_error);
        assert!(result.output.contains("may have started and was stopped"));
        assert_eq!(
            harness.manager.get("test_bash").unwrap().status,
            TaskStatus::Failed
        );
        assert!(!harness.manager.has_active_tasks());
        assert!(harness.manager.reserve_background().is_ok());
        assert!(harness.manager.cancel("test_bash").await.is_err());
        assert!(harness.completions.try_recv().is_err());
        assert_eq!(harness.host.call_count("upsert_background_task"), 2);
    }
}

/// 给定后台长命令，当超时、单任务取消或主运行取消，则完成状态和槽位正确结算。
#[tokio::test]
async fn settle_background_termination() {
    for case in ["timeout", "task", "run", "stop"] {
        let mut harness = Harness::new(0, 1);
        let timeout = if case == "timeout" { 50 } else { 60_000 };
        let command = if case == "task" {
            "sleep 60 & printf ready; wait"
        } else {
            "exec sleep 60"
        };
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            BashTool.call(
                input(command, Some(true), Some(timeout)),
                harness.context.clone(),
            ),
        )
        .await
        .expect("显式后台请求应立即返回");
        assert_started(&result);
        match case {
            "task" => {
                // ready 在子进程创建之后输出，确保取消覆盖实际进程树。
                let events = harness.events.as_mut().unwrap();
                tokio::time::timeout(Duration::from_secs(3), async {
                    while let Some(AgentOutput::Event(event)) = events.recv().await {
                        if let RuntimeToServerEvent::TaskOutputDelta(delta) = *event
                            && delta.delta == "ready"
                        {
                            return;
                        }
                    }
                    panic!("输出流关闭前应收到子进程就绪信号");
                })
                .await
                .expect("应及时收到子进程就绪信号");
                harness.manager.cancel("test_bash").await.unwrap();
            }
            "run" => {
                harness.context.cancelled.store(true, Ordering::Relaxed);
                harness.context.cancel_notify.notify_waiters();
            }
            "stop" => {
                harness.manager.open_stop_boundary();
                harness.manager.cancel_all();
                harness.manager.close_stop_boundary();
            }
            _ => {}
        }
        let completion = harness.finish().await;
        let expected = if case == "timeout" {
            TaskStatus::Failed
        } else {
            TaskStatus::Cancelled
        };
        assert_eq!(completion.status, expected);
        assert_eq!(harness.manager.get("test_bash").unwrap().status, expected);
        if case == "timeout" {
            assert!(completion.summary.unwrap().contains("timed out after 50ms"));
        }
        if case == "stop" {
            assert!(harness.manager.completion_is_stopped("test_bash"));
        }
        assert!(harness.manager.reserve_background().is_ok());
    }
}

/// 给定停止边界内的迟到根任务，当注册后新输入关闭边界，则旧完成仍无唤醒资格。
#[tokio::test]
async fn capture_stopped_registration() {
    let mut harness = Harness::new(0, 1);
    harness.manager.open_stop_boundary();
    let result = BashTool
        .call(
            input("exec sleep 60", Some(true), None),
            harness.context.clone(),
        )
        .await;
    assert_started(&result);
    harness.manager.close_stop_boundary();
    assert_eq!(harness.finish().await.status, TaskStatus::Cancelled);
    assert!(harness.manager.completion_is_stopped("test_bash"));
}

/// 给定默认或 false 调用，当命令超过 30 秒，则仍自动转后台；无槽位则继续前台到超时。
#[tokio::test]
async fn preserve_automatic_background() {
    async fn run_case(background: Option<bool>, capacity: usize) {
        let mut harness = Harness::new(0, capacity);
        let result = tokio::time::timeout(
            Duration::from_secs(35),
            BashTool.call(
                input("exec sleep 60", background, Some(31_000)),
                harness.context.clone(),
            ),
        )
        .await
        .expect("应在 30 秒自动移交或在总时限内超时");
        if capacity == 0 {
            assert!(result.is_error);
            assert!(result.output.contains("timed out after 31000ms"));
            assert!(harness.manager.get("test_bash").is_none());
        } else {
            assert_started(&result);
            let value: serde_json::Value = serde_json::from_str(&result.output).unwrap();
            assert!(value["elapsed_ms"].as_u64().unwrap() >= 30_000);
            // 自动移交不重置 31 秒总时限，后台应在剩余约一秒内超时。
            let completion = harness.finish().await;
            assert_eq!(completion.status, TaskStatus::Failed);
            assert!(
                completion
                    .summary
                    .unwrap()
                    .contains("timed out after 31000ms")
            );
        }
    }
    tokio::join!(
        run_case(None, 1),
        run_case(Some(false), 1),
        run_case(Some(false), 0)
    );
}
