//! 使用无副作用替身验证真实 Bash 策略与工具注册表的审批边界。

use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use crate::engine::ToolPauseResolver;
use crate::tools::bash_tool::{BashInput, BashPermissionPolicy, BashTool};
use crate::tools::*;

struct BashProbe(Arc<AtomicUsize>);
struct ProbePolicy;

#[async_trait]
impl Tool for BashProbe {
    type Input = BashInput;

    fn name(&self) -> &str {
        "bash"
    }

    fn description(&self) -> &str {
        "无副作用的 Bash 执行替身"
    }

    async fn call(&self, _: BashInput, _: ToolExecutionContext) -> ToolResult {
        self.0.fetch_add(1, Ordering::SeqCst);
        ToolResult::ok("executed")
    }
}

#[async_trait]
impl ToolPolicy<BashProbe> for ProbePolicy {
    async fn preflight(
        &self,
        input: &BashInput,
        ctx: &ToolExecutionContext,
    ) -> Result<Option<PermissionPreview>, ToolResult> {
        <BashPermissionPolicy as ToolPolicy<BashTool>>::preflight(&BashPermissionPolicy, input, ctx)
            .await
    }
}

fn probe_input(command: &str) -> HashMap<String, Value> {
    HashMap::from([("command".into(), Value::String(command.into()))])
}

/// 审批前不执行；批准只释放当前一次调用，重复响应也不会重复执行。
#[tokio::test]
async fn approve_bash_once() {
    let calls = Arc::new(AtomicUsize::new(0));
    let tool = RegisteredTool::with_policy(BashProbe(Arc::clone(&calls)), ProbePolicy);
    let mut ctx = ToolExecutionContext::test_with_cwd("bash", PathBuf::from("/workspace"));
    let (event_tx, mut event_rx) = mpsc::channel(8);
    ctx.event_tx = event_tx;
    let resolver = ToolPauseResolver::new(Arc::clone(&ctx.pending_tool_pauses));
    for expected_calls in 1..=2 {
        let execution = tokio::spawn({
            let tool = tool.clone();
            let ctx = ctx.clone();
            async move { tool.execute(probe_input("jj git push"), ctx).await }
        });
        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .expect("风险调用应暂停审批")
            .expect("审批事件通道应保持打开");
        let EngineToRuntimeEvent::ToolPauseRequested(request) = event else {
            panic!("应收到审批请求");
        };
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls - 1);
        assert!(!execution.is_finished());
        assert!(matches!(
            request.kind,
            ToolPauseKind::Permission(PermissionPreview::Bash(_))
        ));
        assert!(request.permission_source.is_some());
        for _ in 0..2 {
            resolver
                .resolve_tool_pause(
                    &request.tool_use_id,
                    ToolPauseResponse::Permission {
                        approved: true,
                        note: None,
                    },
                )
                .expect("批准和重复批准都应安全完成");
        }
        assert!(!execution.await.expect("执行替身应完成").is_error);
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
        assert!(ctx.pending_tool_pauses.lock().unwrap().is_empty());
    }
    assert!(event_rx.try_recv().is_err());
}

/// 拒绝或取消不会进入工具执行，也会清理暂停请求。
#[tokio::test]
async fn reject_bash_execution() {
    for response in [
        ToolPauseResponse::Permission {
            approved: false,
            note: Some("先检查目标".into()),
        },
        ToolPauseResponse::Cancelled,
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let tool = RegisteredTool::with_policy(BashProbe(Arc::clone(&calls)), ProbePolicy);
        let mut ctx = ToolExecutionContext::test_with_cwd("bash", PathBuf::from("/workspace"));
        let (event_tx, mut event_rx) = mpsc::channel(8);
        ctx.event_tx = event_tx;
        let pending = Arc::clone(&ctx.pending_tool_pauses);
        let resolver = ToolPauseResolver::new(Arc::clone(&pending));
        let execution =
            tokio::spawn(async move { tool.execute(probe_input("rm file"), ctx).await });
        let Some(EngineToRuntimeEvent::ToolPauseRequested(request)) =
            tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
                .await
                .unwrap()
        else {
            panic!("应收到审批请求");
        };
        resolver
            .resolve_tool_pause(&request.tool_use_id, response)
            .unwrap();
        assert!(execution.await.unwrap().is_error);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert!(pending.lock().unwrap().is_empty());
    }
}

/// 默认放行直接执行，硬拒绝直接返回错误，两者都不创建审批事件。
#[tokio::test]
async fn apply_bash_defaults() {
    for (command, expected_calls) in [("uv run script.py", 1), ("rm -rf /", 0)] {
        let calls = Arc::new(AtomicUsize::new(0));
        let tool = RegisteredTool::with_policy(BashProbe(Arc::clone(&calls)), ProbePolicy);
        let mut ctx = ToolExecutionContext::test_with_cwd("bash", PathBuf::from("/workspace"));
        let (event_tx, mut event_rx) = mpsc::channel(8);
        ctx.event_tx = event_tx;
        let result = tool.execute(probe_input(command), ctx.clone()).await;
        assert_eq!(result.is_error, expected_calls == 0);
        assert_eq!(calls.load(Ordering::SeqCst), expected_calls);
        assert!(event_rx.try_recv().is_err());
        assert!(ctx.pending_tool_pauses.lock().unwrap().is_empty());
    }
}
