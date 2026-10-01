//! Agent 实例：执行实例的生命周期所有者。
//!
//! 构建阶段完成恢复数据与资源装配并返回输出接收端；宿主接好消费者后调用
//! `start` 启动实例任务。实例任务持有上下文、有效配置、能力与任务监督器，
//! 是唯一驱动执行的一方；关闭时取消前台运行与任务树、等待收尾后关闭输出。

use crate::error::CoreError;
use crate::execution::handle::{AgentEvents, AgentHandle, InstanceState, OutputHandle, RunGate};
use crate::execution::host::AgentHost;
use crate::runtime::command::AgentCommand;
use crate::runtime::service::{AgentRuntime, AgentRuntimeParts, RuntimeCapabilityHandles};
use omini_config::Settings;
use omini_config::project::ProjectDir;
use omini_domain::task::TaskInfo;
use omini_model::message::Message;
use omini_runtime_contract::thread_domain::{ActiveProfile, AgentTaskInfo, ThreadUsageSnapshot};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// 实例构建所需的恢复数据。
pub struct AgentInstanceLoad {
    pub messages: Vec<Message>,
    pub llm_context_version: i64,
    pub usage: ThreadUsageSnapshot,
    pub agent_tasks: Vec<AgentTaskInfo>,
    pub background_tasks: Vec<TaskInfo>,
}

/// 实例装配输入：宿主、身份、恢复数据与有效配置。
pub struct AgentInstanceConfig {
    pub settings: Settings,
    pub project: ProjectDir,
    /// 宿主会话 ID；作为实例关联键，不承担运行身份。
    pub thread_id: String,
    pub active_profile: ActiveProfile,
    pub load: AgentInstanceLoad,
    pub host: Arc<dyn AgentHost>,
}

/// 一次构建得到的实例三件套：句柄、输出与任务所有权。
///
/// `events` 必须在 `start` 之前交给消费者；`join` 提供实例任务的等待退出。
pub struct AgentInstance {
    handle: AgentHandle,
    runtime: Option<AgentRuntime>,
    events: Option<AgentEvents>,
    join: Option<JoinHandle<()>>,
}

impl AgentInstance {
    /// 装配实例：加载能力、建立执行引擎与任务监督器，返回命令句柄与输出接收端。
    ///
    /// 不启动任何任务；调用方接好 `AgentEvents` 消费者后调用 `start`。
    pub fn build(config: AgentInstanceConfig) -> Result<Self, CoreError> {
        let AgentInstanceConfig {
            settings,
            project,
            thread_id,
            active_profile,
            load:
                AgentInstanceLoad {
                    messages,
                    llm_context_version,
                    usage,
                    agent_tasks,
                    background_tasks,
                },
            host,
        } = config;

        let handles = RuntimeCapabilityHandles::load(&settings);
        let thread_dir = project.thread(&thread_id);
        let (cmd_tx, cmd_rx) = mpsc::channel::<AgentCommand>(256);
        let (output, events) = OutputHandle::new(1024);
        let gate = Arc::new(RunGate::default());
        // state 持有共享的待交互暂停表：快照读取键集合，引擎/监督器读写等待者。
        let state = Arc::new(InstanceState::new(
            settings.clone(),
            active_profile,
            Arc::clone(&gate),
        ));
        let handle = AgentHandle::new(
            thread_id.clone(),
            cmd_tx,
            Arc::clone(&gate),
            Arc::clone(&state),
            Arc::clone(&handles.capabilities),
            Arc::clone(&handles.mcp_manager),
        );

        let runtime = AgentRuntime::assemble(AgentRuntimeParts {
            settings,
            project,
            thread_id,
            thread_dir,
            messages,
            llm_context_version,
            usage,
            agent_tasks,
            background_tasks,
            host,
            output,
            cmd_rx,
            gate,
            state,
            handles,
        });

        Ok(Self {
            handle,
            runtime: Some(runtime),
            events: Some(events),
            join: None,
        })
    }

    /// 可克隆的命令句柄；不持有实例任务所有权。
    pub fn handle(&self) -> AgentHandle {
        self.handle.clone()
    }

    /// 取走独占的输出接收端；实例启动前必须交给消费者。
    pub fn take_events(&mut self) -> AgentEvents {
        self.events
            .take()
            .expect("agent events can only be taken once")
    }

    /// 启动实例任务；重复启动视为内部错误。
    pub fn start(&mut self) {
        assert!(
            self.events.is_none(),
            "take events before starting the instance"
        );
        assert!(
            self.join.is_none(),
            "agent instance can only be started once"
        );
        let runtime = self
            .runtime
            .take()
            .expect("agent instance can only be started once");
        self.join = Some(runtime.spawn_task());
    }

    /// 实例任务句柄；用于关闭后排空等待。
    pub fn join(&mut self) -> &mut Option<JoinHandle<()>> {
        &mut self.join
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::{AgentOutput, AgentSnapshot};
    use crate::test_support::{RecordingHost, TestTempDir, settings};
    use omini_domain::input::InputPart;
    use omini_runtime_contract::thread as thread_types;

    fn build_instance(host: Arc<RecordingHost>) -> (AgentInstance, crate::execution::AgentEvents) {
        let temp = TestTempDir::new("instance");
        let path = temp.path().to_path_buf();
        std::mem::forget(temp);
        let config = AgentInstanceConfig {
            settings: settings(&path, false),
            project: omini_config::project::ProjectDir::from_path(path),
            thread_id: "thread_main".to_string(),
            active_profile: omini_runtime_contract::thread_domain::ActiveProfile::Main,
            load: AgentInstanceLoad {
                messages: Vec::new(),
                llm_context_version: 1,
                usage: omini_runtime_contract::thread_domain::ThreadUsageSnapshot::default(),
                agent_tasks: Vec::new(),
                background_tasks: Vec::new(),
            },
            host,
        };
        let mut instance = AgentInstance::build(config).expect("instance should assemble");
        let events = instance.take_events();
        instance.start();
        (instance, events)
    }

    fn user_message() -> omini_model::message::Message {
        omini_model::message::Message::from_user_text("hello".to_string())
    }

    fn plain_input() -> thread_types::RuntimeUserInput {
        thread_types::RuntimeUserInput {
            parts: vec![InputPart::Text {
                text: "hello".to_string(),
            }],
            attachments: Vec::new(),
        }
    }

    /// 并发提交只能成功一个：第二个预留必须以 run_busy 被拒绝。
    #[tokio::test]
    async fn concurrent_submissions_admit_exactly_one() {
        let (mut instance, _events) = build_instance(Arc::new(RecordingHost::default()));
        let handle = instance.handle();

        let first = handle.reserve_run();
        let second = handle.reserve_run();

        let first = first.expect("first reservation should succeed");
        assert!(
            second.is_err_and(|error| error.code() == "run_busy"),
            "second reservation must be rejected with run_busy"
        );
        assert!(handle.snapshot().run_reserved);

        // 预留丢弃后资格自动释放，可再次预留。
        drop(first);
        assert!(!handle.snapshot().run_reserved);
        assert!(handle.reserve_run().is_ok());

        handle.close().await.expect("close should be idempotent");
        let mut join = instance.join().take();
        if let Some(join) = join.take() {
            tokio::time::timeout(std::time::Duration::from_secs(5), join)
                .await
                .expect("instance must finish before timeout")
                .expect("instance must not panic");
        }
    }

    /// 配置修改失败时确认错误，且有效快照保持不变。
    #[tokio::test]
    async fn failed_config_change_keeps_effective_snapshot() {
        let host = Arc::new(RecordingHost::default());
        host.fail_operation("update_thread_config", "db unavailable");
        let (mut instance, mut events) = build_instance(host.clone());
        let handle = instance.handle();

        let before: AgentSnapshot = handle.snapshot();
        // 测试配置的 providers.test 只含 text-model；选中同一有效模型仍会
        // 走持久化路径，宿主注入的失败必须把整个修改回滚。
        let error = handle
            .set_model(thread_types::SetModelCommand {
                provider: "test".to_string(),
                model: "text-model".to_string(),
                thinking_effort: None,
            })
            .await
            .expect_err("model change must fail when persistence fails");
        assert_eq!(error.code(), "persistence_error");

        let after = handle.snapshot();
        assert_eq!(after.provider, before.provider);
        assert_eq!(after.model, before.model);
        assert_eq!(
            host.call_count("update_thread_config"),
            1,
            "persistence must have been attempted exactly once"
        );

        handle.close().await.expect("close should succeed");
        while let Some(output) = events.recv().await {
            if let AgentOutput::Event(event) = output
                && matches!(
                    &*event,
                    omini_runtime_contract::RuntimeToServerEvent::ModelChanged { .. }
                )
            {
                panic!("no ModelChanged may be emitted for a failed change");
            }
        }
        let mut join = instance.join().take();
        if let Some(join) = join.take() {
            tokio::time::timeout(std::time::Duration::from_secs(5), join)
                .await
                .expect("instance must finish before timeout")
                .expect("instance must not panic");
        }
    }

    /// 运行中关闭不被拒绝；重复关闭幂等；实例最终完整结束。
    #[tokio::test]
    async fn close_during_run_is_accepted_and_idempotent() {
        let host = Arc::new(RecordingHost::default());
        host.fail_operation("append_llm_message", "controlled persistence failure");
        let pause = host.pause_append();
        let (mut instance, mut events) = build_instance(host.clone());
        let handle = instance.handle();
        let reservation = handle.reserve_run().unwrap();
        reservation.commit(user_message()).await.unwrap();
        // 给定在提交模型上下文边界暂停的运行，当发起关闭，则不能提前返回完成。
        tokio::time::timeout(std::time::Duration::from_secs(5), pause.wait_entered())
            .await
            .unwrap();
        assert!(handle.snapshot().current_run.is_some());
        let close_handle = handle.clone();
        let closing = tokio::spawn(async move { close_handle.close().await });
        tokio::task::yield_now().await;
        assert!(!closing.is_finished());
        pause.release();
        tokio::time::timeout(std::time::Duration::from_secs(5), closing)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        handle.close().await.unwrap();
        assert_eq!(
            handle.snapshot().lifecycle,
            crate::execution::AgentLifecycle::Closed
        );
        assert_eq!(host.call_count("append_llm_message"), 1);
        let mut closed = false;
        while let Some(output) = events.recv().await {
            if matches!(output, AgentOutput::Closed) {
                closed = true;
            }
        }
        assert!(closed);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            instance.join().take().unwrap(),
        )
        .await
        .unwrap()
        .unwrap();
    }

    /// 无客户端（输出消费者断开）时实例自行回收结束。
    #[tokio::test]
    async fn dropping_the_consumer_reclaims_the_instance() {
        let host = Arc::new(RecordingHost::default());
        let (mut instance, events) = build_instance(host);
        let handle = instance.handle();

        drop(events);
        let join = instance.join().take();
        if let Some(join) = join {
            tokio::time::timeout(std::time::Duration::from_secs(5), join)
                .await
                .expect("instance must exit once the consumer is gone")
                .expect("instance task should not panic");
        }
        // 消费者断开后的命令按实例不可用报告。
        assert!(matches!(
            handle.reserve_run(),
            Err(error) if error.code() == "run_busy"
        ));
    }

    /// prepare_run 在不进入实例循环的情况下完成输入组装。
    #[tokio::test]
    async fn prepare_run_assembles_submission_without_a_run() {
        let (mut instance, _events) = build_instance(Arc::new(RecordingHost::default()));
        let handle = instance.handle();

        let prepared = handle
            .prepare_run(thread_types::SubmitRunCommand {
                input: plain_input(),
                client_echo_id: Some("echo_1".to_string()),
                intent: thread_types::RunIntent::SubmitMessage,
            })
            .expect("plain input should assemble");
        assert_eq!(prepared.client_echo_id.as_deref(), Some("echo_1"));
        assert!(!prepared.message.content.is_empty());

        handle.close().await.expect("close should succeed");
        let mut join = instance.join().take();
        if let Some(join) = join.take() {
            tokio::time::timeout(std::time::Duration::from_secs(5), join)
                .await
                .expect("instance must finish before timeout")
                .expect("instance must not panic");
        }
    }
}
