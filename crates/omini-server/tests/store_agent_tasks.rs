use jiff::SignedDuration;
mod support;

use crate::support::store::*;
use omini_domain::agent_run::{
    AgentRunSnapshot, AgentRunStatus, AgentStepSnapshot, AgentStepStatus, ToolUseExecutionSnapshot,
    ToolUseStatus,
};
use omini_domain::conversation::{
    AgentMessage, AssistantMessage, AssistantMessageBlock, ConversationEntry, SystemEvent,
    ToolResultRecord, UserInput,
};
use omini_domain::input::{InputPart, UserInputIntent};
use omini_domain::task::TaskStatus;
use omini_entity::MessageKind;
use omini_model::message::{ContentBlock, Message, Role};
use omini_runtime_contract::thread_domain::ClientMessage;
use omini_runtime_contract::thread_domain::{AgentTaskResult, DeliveryKey};
use omini_server::store::load_messages;

#[tokio::test]
async fn agent_delivery_idempotency() {
    // 给定运行中的直接子任务和同一工具调用来源。
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let task = test_agent_task("task-agent", "child-agent", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("child-agent", "owner"),
        &crate::support::store::test_user_input(&Message::from_user_text("start".to_string())),
        &Message::from_user_text("start".to_string()),
    )
    .await
    .unwrap();
    // 子 Run 的实际 thread_id 是 child-agent，父线程只通过 owner/task 查找。
    assert!(
        db.get_owned_task("owner", "task-agent")
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        db.get_owned_task("other-owner", "task-agent")
            .await
            .unwrap()
            .is_none()
    );
    let source = AgentMessage {
        source_run_id: "run-1".into(),
        tool_use_id: "tool-1".into(),
        text: "same text".into(),
    };
    let model_message = Message::from_user_text("来自主 Agent 的消息：\nsame text".to_string());

    // 当重复入队、注入时，同键只能留下一个历史和模型消息；改正文必须冲突。
    assert!(
        db.enqueue_agent_message(
            "task-agent",
            "owner",
            "child-agent",
            &source,
            &project.thread("child-agent")
        )
        .await
        .unwrap()
    );
    assert!(
        !db.enqueue_agent_message(
            "task-agent",
            "owner",
            "child-agent",
            &source,
            &project.thread("child-agent")
        )
        .await
        .unwrap()
    );
    let changed = AgentMessage {
        text: "changed".into(),
        ..source.clone()
    };
    assert!(
        db.enqueue_agent_message(
            "task-agent",
            "owner",
            "child-agent",
            &changed,
            &project.thread("child-agent")
        )
        .await
        .is_err()
    );
    let ui_before_injection =
        load_messages(&db, "child-agent", &project.thread("child-agent")).await;
    assert_eq!(
        ui_before_injection
            .iter()
            .filter(|entry| **entry
                == ConversationEntry::SystemEvent(SystemEvent::AgentMessage(source.clone())))
            .count(),
        1
    );
    let model_before_injection = db
        .load_current_llm_messages("child-agent", &project.thread("child-agent"))
        .await
        .unwrap();
    assert!(
        !model_before_injection
            .iter()
            .any(|message| message == &model_message)
    );
    let key = DeliveryKey::from_agent("task-agent", &source.source_run_id, &source.tool_use_id);
    db.inject_task_message(
        "child-agent",
        &key,
        &model_message,
        &project.thread("child-agent"),
    )
    .await
    .unwrap();
    db.inject_task_message(
        "child-agent",
        &key,
        &model_message,
        &project.thread("child-agent"),
    )
    .await
    .unwrap();

    // 则子会话保存来源结构，模型仍收到带来源标注的 User 角色消息。
    let history = load_messages(&db, "child-agent", &project.thread("child-agent")).await;
    assert_eq!(
        history
            .iter()
            .filter(|entry| **entry
                == ConversationEntry::SystemEvent(SystemEvent::AgentMessage(source.clone())))
            .count(),
        1
    );
    let llm = db
        .load_current_llm_messages("child-agent", &project.thread("child-agent"))
        .await
        .unwrap();
    assert_eq!(llm.last(), Some(&model_message));
    assert_eq!(db.projected_delivery_keys("owner").await.unwrap().len(), 1);
}

#[tokio::test]
async fn client_delivery_recovery() {
    // 给定两个客户端在同一子任务提交相同正文，但各有独立来源键。
    let (db, project, root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let task = test_agent_task("task-client", "child-client", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("child-client", "owner"),
        &crate::support::store::test_user_input(&Message::from_user_text("start".to_string())),
        &Message::from_user_text("start".to_string()),
    )
    .await
    .unwrap();
    let input = UserInput {
        intent: UserInputIntent::Message,
        parts: vec![InputPart::Text {
            text: "hello".into(),
        }],
        attachments: Vec::new(),
    };
    let first = ClientMessage {
        client_id: "client-a".into(),
        client_echo_id: "echo-1".into(),
        input: input.clone(),
    };
    let second = ClientMessage {
        client_id: "client-b".into(),
        client_echo_id: "echo-1".into(),
        input: input.clone(),
    };
    assert!(
        db.enqueue_client_message(
            "task-client",
            "owner",
            "child-client",
            &first,
            &project.thread("child-client")
        )
        .await
        .unwrap()
    );
    assert!(
        !db.enqueue_client_message(
            "task-client",
            "owner",
            "child-client",
            &first,
            &project.thread("child-client")
        )
        .await
        .unwrap()
    );
    assert!(
        db.enqueue_client_message(
            "task-client",
            "owner",
            "child-client",
            &second,
            &project.thread("child-client")
        )
        .await
        .unwrap()
    );
    let changed = ClientMessage {
        input: UserInput {
            parts: vec![InputPart::Text {
                text: "different".into(),
            }],
            ..input.clone()
        },
        ..first.clone()
    };
    assert!(
        db.enqueue_client_message(
            "task-client",
            "owner",
            "child-client",
            &changed,
            &project.thread("child-client")
        )
        .await
        .is_err()
    );

    // 当 UI 先记录两条消息时，模型上下文仍可保持原有顺序。
    let ui_before_injection =
        load_messages(&db, "child-client", &project.thread("child-client")).await;
    assert_eq!(
        ui_before_injection
            .iter()
            .filter(|entry| **entry == ConversationEntry::UserInput(input.clone()))
            .count(),
        2
    );
    let model_before_injection = db
        .load_current_llm_messages("child-client", &project.thread("child-client"))
        .await
        .unwrap();
    assert!(
        !model_before_injection
            .iter()
            .any(|message| { *message == Message::from_user_text("hello".to_string()) })
    );

    // 当两个来源都注入后，第三条在重启时失败，但已接受的 UI 消息仍可见。
    let model_message = Message::from_user_text("hello".to_string());
    for source in [&first, &second] {
        let key =
            DeliveryKey::from_client("task-client", &source.client_id, &source.client_echo_id);
        db.inject_task_message(
            "child-client",
            &key,
            &model_message,
            &project.thread("child-agent"),
        )
        .await
        .unwrap();
    }
    let pending = ClientMessage {
        client_id: "client-c".into(),
        client_echo_id: "echo-1".into(),
        input: input.clone(),
    };
    db.enqueue_client_message(
        "task-client",
        "owner",
        "child-client",
        &pending,
        &project.thread("child-client"),
    )
    .await
    .unwrap();
    // 模拟重启:恢复归一化在 Store::open 内执行,重开两次验证幂等。
    drop(db);
    let (db, _project, _root) = temp_db_existing(&root.path.join("omini.sqlite")).await;
    drop(db);
    let (db, project, _root) = temp_db_existing(&root.path.join("omini.sqlite")).await;

    // 则快照来源键对应三条 UI 消息，模型仅有两条，任务结果记录未送达数量。
    let history = load_messages(&db, "child-client", &project.thread("child-client")).await;
    assert_eq!(
        history
            .iter()
            .filter(|entry| **entry == ConversationEntry::UserInput(input.clone()))
            .count(),
        3
    );
    let llm = db
        .load_current_llm_messages("child-client", &project.thread("child-client"))
        .await
        .unwrap();
    assert_eq!(
        llm.iter()
            .filter(|message| **message == model_message)
            .count(),
        2
    );
    assert_eq!(db.projected_delivery_keys("owner").await.unwrap().len(), 3);
    let task_row = db
        .get_owned_task("owner", "task-client")
        .await
        .unwrap()
        .unwrap();
    let result = task_row.result.expect("task result").0;
    assert_eq!(result.undelivered_messages, Some(1));
    assert_eq!(
        db.client_delivery("task-client", &pending)
            .await
            .unwrap()
            .unwrap()
            .1,
        omini_server::store::DeliveryStatus::Failed
    );
}

#[tokio::test]
async fn child_message_order() {
    // 给定主 Agent 与客户端依次提交消息，UI 顺序在入队时固定。
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let task = test_agent_task("task-order", "child-order", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("child-order", "owner"),
        &crate::support::store::test_user_input(&Message::from_user_text("start".into())),
        &Message::from_user_text("start".into()),
    )
    .await
    .unwrap();
    let agent = AgentMessage {
        source_run_id: "main-run".into(),
        tool_use_id: "send-1".into(),
        text: "first in UI".into(),
    };
    let input = UserInput {
        intent: UserInputIntent::Message,
        parts: vec![InputPart::Text {
            text: "second in UI".into(),
        }],
        attachments: Vec::new(),
    };
    let client = ClientMessage {
        client_id: "client-a".into(),
        client_echo_id: "echo-1".into(),
        input: input.clone(),
    };
    db.enqueue_agent_message(
        "task-order",
        "owner",
        "child-order",
        &agent,
        &project.thread("child-order"),
    )
    .await
    .unwrap();
    db.enqueue_client_message(
        "task-order",
        "owner",
        "child-order",
        &client,
        &project.thread("child-agent"),
    )
    .await
    .unwrap();

    // 当安全边界以相反顺序消费时，模型历史仍按实际消费顺序追加。
    let client_model = Message::from_user_text("second in UI".into());
    let agent_model = Message::from_user_text("来自主 Agent 的消息：\nfirst in UI".into());
    db.inject_task_message(
        "child-order",
        &DeliveryKey::from_client("task-order", &client.client_id, &client.client_echo_id),
        &client_model,
        &project.thread("child-order"),
    )
    .await
    .unwrap();
    db.inject_task_message(
        "child-order",
        &DeliveryKey::from_agent("task-order", &agent.source_run_id, &agent.tool_use_id),
        &agent_model,
        &project.thread("child-order"),
    )
    .await
    .unwrap();

    // 则两个序列各自保留顺序，且快照按两个来源键去重。
    let history = load_messages(&db, "child-order", &project.thread("child-order")).await;
    let agent_pos = history.iter().position(|entry| {
        *entry == ConversationEntry::SystemEvent(SystemEvent::AgentMessage(agent.clone()))
    });
    let client_pos = history
        .iter()
        .position(|entry| *entry == ConversationEntry::UserInput(input.clone()));
    assert!(agent_pos.unwrap() < client_pos.unwrap());
    let model = db
        .load_current_llm_messages("child-order", &project.thread("child-order"))
        .await
        .unwrap();
    let client_pos = model.iter().position(|message| message == &client_model);
    let agent_pos = model.iter().position(|message| message == &agent_model);
    assert!(client_pos.unwrap() < agent_pos.unwrap());
    assert_eq!(db.projected_delivery_keys("owner").await.unwrap().len(), 2);
}

#[tokio::test]
async fn delivery_cancel_failure() {
    // 给定已落盘但尚未进入子 Agent 上下文的主 Agent 消息。
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let task = test_agent_task("task-cancel", "child-cancel", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("child-cancel", "owner"),
        &crate::support::store::test_user_input(&Message::from_user_text("start".into())),
        &Message::from_user_text("start".into()),
    )
    .await
    .unwrap();
    let message = AgentMessage {
        source_run_id: "run-1".into(),
        tool_use_id: "tool-1".into(),
        text: "check".into(),
    };
    db.enqueue_agent_message(
        "task-cancel",
        "owner",
        "child-cancel",
        &message,
        &project.thread("child-cancel"),
    )
    .await
    .unwrap();

    // 当任务取消时，待处理消息结算为失败，重复结算只返回同一累计数。
    let failed_count = db
        .fail_task_messages("task-cancel", "任务取消")
        .await
        .unwrap();
    assert_eq!(failed_count, 1);
    assert_eq!(
        db.fail_task_messages("task-cancel", "任务取消")
            .await
            .unwrap(),
        1
    );

    // 则子会话保留已接受的 UI 消息，主线程不增加独立的失败消息。
    let child = load_messages(&db, "child-cancel", &project.thread("child-cancel")).await;
    assert_eq!(
        child
            .iter()
            .filter(|entry| matches!(
                entry,
                ConversationEntry::SystemEvent(SystemEvent::AgentMessage(_))
            ))
            .count(),
        1
    );
    assert!(
        load_messages(&db, "owner", &project.thread("owner"))
            .await
            .is_empty()
    );
    assert!(
        db.enqueue_agent_message(
            "task-cancel",
            "owner",
            "child-cancel",
            &message,
            &project.thread("child-cancel")
        )
        .await
        .is_err()
    );
    let key = DeliveryKey::from_agent("task-cancel", &message.source_run_id, &message.tool_use_id);
    assert!(
        db.inject_task_message(
            "child-cancel",
            &key,
            &Message::from_user_text("来自主 Agent 的消息：\ncheck".into()),
            &project.thread("child-cancel"),
        )
        .await
        .is_err()
    );
}

#[tokio::test]
// 创建失败不得留下子线程；重启后运行中任务必须按终态恢复。
async fn agent_task_creation_and_recovery() {
    let (db, project, root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let initial = Message::from_user_text("do work".to_string());

    let task = test_agent_task("task_running", "agent_running", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("agent_running", "owner"),
        &crate::support::store::test_user_input(&initial),
        &initial,
    )
    .await
    .unwrap();
    assert_eq!(db.get_messages("agent_running").await.unwrap().len(), 2);
    assert_eq!(
        load_messages(&db, "agent_running", &project.thread("agent_running")).await,
        vec![ConversationEntry::UserInput(
            omini_domain::conversation::UserInput {
                intent: UserInputIntent::Message,
                parts: vec![InputPart::Text {
                    text: "do work".to_string(),
                }],
                attachments: Vec::new(),
            }
        )]
    );
    let follow_up = ConversationEntry::UserInput(UserInput {
        intent: UserInputIntent::Message,
        parts: vec![
            InputPart::Skill {
                name: "review".to_string(),
            },
            InputPart::File {
                path: "src/lib.rs".to_string(),
                label: Some("lib.rs".to_string()),
            },
        ],
        attachments: Vec::new(),
    });
    db.insert_conversation_entry(
        "agent_running",
        &follow_up,
        Role::User,
        None,
        fixed_time(),
        &project.thread("agent_running"),
    )
    .await
    .unwrap();
    let assistant = ConversationEntry::AssistantMessage(AssistantMessage {
        blocks: vec![AssistantMessageBlock::Text {
            text: "review complete".to_string(),
        }],
    });
    db.insert_conversation_entry(
        "agent_running",
        &assistant,
        Role::Assistant,
        Some("openai/gpt-test"),
        fixed_time(),
        &project.thread("agent_running"),
    )
    .await
    .unwrap();
    let tool_results = ConversationEntry::SystemEvent(SystemEvent::ToolResults {
        results: vec![ToolResultRecord {
            tool_use_id: "read-1".to_string(),
            is_error: false,
            content: "file contents".to_string(),
            metadata: None,
        }],
    });
    db.insert_conversation_entry(
        "agent_running",
        &tool_results,
        Role::User,
        None,
        fixed_time(),
        &project.thread("agent_running"),
    )
    .await
    .unwrap();
    assert_eq!(
        load_messages(&db, "agent_running", &project.thread("agent_running")).await,
        vec![
            ConversationEntry::UserInput(UserInput {
                intent: UserInputIntent::Message,
                parts: vec![InputPart::Text {
                    text: "do work".to_string(),
                }],
                attachments: Vec::new(),
            }),
            follow_up,
            assistant,
            tool_results,
        ]
    );
    assert_eq!(
        db.load_current_llm_messages("agent_running", &project.thread("agent_running"))
            .await
            .unwrap(),
        vec![initial.clone()]
    );

    let cancelling = test_agent_task("task_cancelling", "agent_cancelling", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &cancelling,
        &test_agent_thread("agent_cancelling", "owner"),
        &crate::support::store::test_user_input(&initial),
        &initial,
    )
    .await
    .unwrap();
    db.set_agent_tasks_cancelling(&["task_cancelling".to_string()], fixed_time())
        .await
        .unwrap();

    let invalid = test_agent_task("task_invalid", "agent_rolled_back", "missing_owner");
    assert!(
        db.create_agent_task(
            TEST_PROJECT_ID,
            &invalid,
            &test_agent_thread("agent_rolled_back", "owner"),
            &crate::support::store::test_user_input(&initial),
            &initial,
        )
        .await
        .is_err()
    );
    assert!(db.get_thread("agent_rolled_back").await.unwrap().is_none());

    let (db, _project, _root) = temp_db_existing(&root.path.join("omini.sqlite")).await;
    let tasks = db.list_agent_tasks("owner").await.unwrap();
    assert_eq!(tasks.len(), 2);
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.task_id == "task_running")
            .unwrap()
            .status,
        TaskStatus::Interrupted
    );
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.task_id == "task_cancelling")
            .unwrap()
            .status,
        TaskStatus::Interrupted
    );
    assert!(tasks.iter().all(|task| task.completed_at.is_some()));
}

#[tokio::test]
// 重复投递同一任务完成通知不能重复写入 UI 或 LLM 历史。
async fn task_notification_is_idempotent() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let task = test_agent_task("task_done", "agent_done", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("agent_done", "owner"),
        &crate::support::store::test_user_input(&Message::from_user_text("do work".to_string())),
        &Message::from_user_text("do work".to_string()),
    )
    .await
    .unwrap();
    let completed_at = fixed_time();
    db.finish_agent_task(
        "task_done",
        TaskStatus::Completed,
        &AgentTaskResult {
            output: Some("done".to_string()),
            error: None,
            warnings: Vec::new(),
            undelivered_messages: None,
        },
        completed_at,
    )
    .await
    .unwrap();
    let before = Message::new(
        Role::Assistant,
        vec![ContentBlock::from_text("before notification".to_string())],
    );
    db.append_llm_message("owner", &before, fixed_time(), &project.thread("owner"))
        .await
        .unwrap();
    let notification = omini_domain::conversation::TaskNotification {
        tasks: vec![omini_domain::task::TaskCompletion {
            task_id: "task_done".to_string(),
            kind: omini_domain::task::TaskKind::SubAgent,
            label: "general".to_string(),
            title: "Test agent".to_string(),
            status: TaskStatus::Completed,
            summary: None,
        }],
        created_at: completed_at,
    };
    for round in 0..2 {
        let inserted = db
            .insert_task_notification(
                "owner",
                &notification,
                completed_at,
                &project.thread("owner"),
            )
            .await
            .unwrap();
        // 首次返回实际写入的新通知;重复请求没有新任务,返回 None。
        assert_eq!(inserted.is_some(), round == 0);
    }
    // 落库行内容按 domain 共享构造重建,与 core 的运行内注入一致。
    let llm_message = Message::from_user_text(omini_domain::conversation::task_notification_text(
        &notification.tasks,
    ));
    let after = Message::new(
        Role::Assistant,
        vec![ContentBlock::from_text("after notification".to_string())],
    );
    db.append_llm_message("owner", &after, fixed_time(), &project.thread("owner"))
        .await
        .unwrap();

    let ui_count = count_thread_messages(&db, "owner", MessageKind::ConversationEntry).await;
    let llm_count = count_thread_llm_messages(&db, "owner", Role::User).await;
    assert_eq!(ui_count, 1);
    assert_eq!(llm_count, 1);
    assert_eq!(
        db.load_current_llm_messages("owner", &project.thread("owner"))
            .await
            .unwrap(),
        vec![before, llm_message, after]
    );
    assert!(db.list_agent_tasks("owner").await.unwrap()[0].notification_delivered);
    assert!(matches!(
        load_messages(&db, "owner", &project.thread("owner"))
            .await
            .as_slice(),
        [omini_domain::conversation::ConversationEntry::SystemEvent(
            omini_domain::conversation::SystemEvent::TaskNotification(restored)
        )] if restored == &notification
    ));
}

/// 回归：同线程的其它任务可能也等待通知。置位必须只覆盖实际写入的任务，缺席任务保持
/// 未交付，其后的正规插入仍能写入新通知。
#[tokio::test]
async fn payload_keeps_gate() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    for (task_id, agent_id) in [("task_done", "agent_done"), ("task_extra", "agent_extra")] {
        let task = test_agent_task(task_id, agent_id, "owner");
        db.create_agent_task(
            TEST_PROJECT_ID,
            &task,
            &test_agent_thread(agent_id, "owner"),
            &crate::support::store::test_user_input(&Message::from_user_text(
                "do work".to_string(),
            )),
            &Message::from_user_text("do work".to_string()),
        )
        .await
        .unwrap();
        db.finish_agent_task(
            task_id,
            TaskStatus::Completed,
            &AgentTaskResult {
                output: Some("done".to_string()),
                error: None,
                warnings: Vec::new(),
                undelivered_messages: None,
            },
            fixed_time(),
        )
        .await
        .unwrap();
    }
    let completion = |task_id: &str| omini_domain::task::TaskCompletion {
        task_id: task_id.to_string(),
        kind: omini_domain::task::TaskKind::SubAgent,
        label: "general".to_string(),
        title: "Test agent".to_string(),
        status: TaskStatus::Completed,
        summary: None,
    };
    // 通知只包含 task_done，数据库中同属 owner 的 task_extra 仍未交付。
    let inserted = db
        .insert_task_notification(
            "owner",
            &omini_domain::conversation::TaskNotification {
                tasks: vec![completion("task_done")],
                created_at: fixed_time(),
            },
            fixed_time(),
            &project.thread("owner"),
        )
        .await
        .unwrap();
    assert!(inserted.is_some());
    let tasks = db.list_agent_tasks("owner").await.unwrap();
    let delivered: std::collections::HashMap<&str, bool> = tasks
        .iter()
        .map(|task| (task.task_id.as_str(), task.notification_delivered))
        .collect();
    assert!(delivered["task_done"], "present task is delivered");
    assert!(
        !delivered["task_extra"],
        "task absent from the notification must stay undelivered"
    );

    // 缺席任务随后的正规插入照常写入新通知。
    let later = db
        .insert_task_notification(
            "owner",
            &omini_domain::conversation::TaskNotification {
                tasks: vec![completion("task_extra")],
                created_at: fixed_time(),
            },
            fixed_time(),
            &project.thread("owner"),
        )
        .await
        .unwrap();
    assert!(
        later.is_some(),
        "later insertion of the absent task succeeds"
    );
    let tasks = db.list_agent_tasks("owner").await.unwrap();
    assert!(tasks.iter().all(|task| task.notification_delivered));
}

/// 回归：模型路径写入的时间列必须无损往返且保持时间序。
/// 全部写入方均为 toasty 后,字节格式不再被外部依赖,断言落在
/// 往返等值与排序行为上。
#[tokio::test]
async fn native_timestamp_column_round_trips_and_orders() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    db.upsert_task(&omini_domain::task::TaskInfo {
        task_id: "fmt_probe".to_string(),
        owner_thread_id: "owner".to_string(),
        kind: omini_domain::task::TaskKind::Bash,
        title: "probe".to_string(),
        status: TaskStatus::Running,
        created_at: fixed_time(),
        updated_at: fixed_time(),
        completed_at: None,
        result_summary: None,
    })
    .await
    .unwrap();
    let later = fixed_time() + SignedDuration::from_mins(1);
    let mut with_fraction = omini_domain::task::TaskInfo {
        task_id: "fmt_probe".to_string(),
        owner_thread_id: "owner".to_string(),
        kind: omini_domain::task::TaskKind::Bash,
        title: "probe".to_string(),
        status: TaskStatus::Running,
        created_at: fixed_time(),
        updated_at: fixed_time(),
        completed_at: None,
        result_summary: None,
    };
    with_fraction.task_id = "fmt_probe_later".to_string();
    with_fraction.created_at = later;
    with_fraction.updated_at = later;
    db.upsert_task(&with_fraction).await.unwrap();
    let stored = background_task_row(&db, "fmt_probe")
        .await
        .expect("probe row")
        .created_at;
    assert_eq!(stored, fixed_time(), "timestamp must round-trip losslessly");
    let mut conn = db.conn();
    let ordered: Vec<String> = omini_entity::BackgroundTask::filter(
        omini_entity::BackgroundTask::fields()
            .task_id()
            .in_list(vec!["fmt_probe".to_string(), "fmt_probe_later".to_string()]),
    )
    .order_by(omini_entity::BackgroundTask::fields().created_at().asc())
    .select(omini_entity::BackgroundTask::fields().task_id())
    .exec(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        ordered,
        vec!["fmt_probe".to_string(), "fmt_probe_later".to_string()],
        "created_at ordering must follow chronological order"
    );
    // 亚秒差也必须保持时间序:字典序 = 时间序的关键场景。
    let mut sub_second = with_fraction.clone();
    sub_second.task_id = "fmt_probe_sub".to_string();
    sub_second.created_at = fixed_time() + SignedDuration::from_millis(500);
    sub_second.updated_at = sub_second.created_at;
    db.upsert_task(&sub_second).await.unwrap();
    let mut conn = db.conn();
    let ordered: Vec<String> = omini_entity::BackgroundTask::filter(
        omini_entity::BackgroundTask::fields()
            .task_id()
            .in_list(vec!["fmt_probe".to_string(), "fmt_probe_sub".to_string()]),
    )
    .order_by(omini_entity::BackgroundTask::fields().created_at().asc())
    .select(omini_entity::BackgroundTask::fields().task_id())
    .exec(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        ordered,
        vec!["fmt_probe".to_string(), "fmt_probe_sub".to_string()],
        "sub-second timestamps must keep chronological ordering"
    );
}

/// 回归：通知投递后再重放 UpsertTask（如任务完成后 TaskManager 再次 update），
/// 不得把 notification_delivered 幂等闸门重置回 false，也不得覆盖 created_at。
#[tokio::test]
async fn upsert_task_replay_keeps_delivery_gate_and_creation_time() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let now = fixed_time();
    let task = omini_domain::task::TaskInfo {
        task_id: "bash_task".to_string(),
        owner_thread_id: "owner".to_string(),
        kind: omini_domain::task::TaskKind::Bash,
        title: "Run build command".to_string(),
        status: TaskStatus::Completed,
        created_at: now,
        updated_at: now,
        completed_at: Some(now),
        result_summary: Some("build finished".to_string()),
    };
    db.upsert_task(&task).await.unwrap();

    let notification = omini_domain::conversation::TaskNotification {
        tasks: vec![omini_domain::task::TaskCompletion {
            task_id: "bash_task".to_string(),
            kind: omini_domain::task::TaskKind::Bash,
            label: "Bash".to_string(),
            title: "Run build command".to_string(),
            status: TaskStatus::Completed,
            summary: Some("build finished".to_string()),
        }],
        created_at: now,
    };
    db.insert_task_notification("owner", &notification, now, &project.thread("owner"))
        .await
        .unwrap();

    // 投递完成后，以更晚的 created_at/updated_at 重放同键 UpsertTask。
    let later = now + SignedDuration::from_hours(1);
    let mut replay = task.clone();
    replay.updated_at = later;
    replay.created_at = later;
    db.upsert_task(&replay).await.unwrap();

    let row = background_task_row(&db, "bash_task")
        .await
        .expect("task row");
    assert!(
        row.notification_delivered,
        "delivery gate must survive upsert replay"
    );
    assert_eq!(
        row.created_at, now,
        "conflict update must keep the first creation time"
    );
    // 幂等闸门保持后，重复的通知插入不应再写入第二条时间线记录。
    let replayed = db
        .insert_task_notification("owner", &notification, later, &project.thread("owner"))
        .await
        .unwrap();
    assert!(
        replayed.is_none(),
        "fully delivered replay must report no new content"
    );
    let count = count_thread_messages(&db, "owner", MessageKind::ConversationEntry).await;
    assert_eq!(count, 1);
}

#[tokio::test]
async fn bash_task_notification_is_persisted_as_a_generic_task_event() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let now = fixed_time();
    db.upsert_task(&omini_domain::task::TaskInfo {
        task_id: "bash_task".to_string(),
        owner_thread_id: "owner".to_string(),
        kind: omini_domain::task::TaskKind::Bash,
        title: "Run build command".to_string(),
        status: TaskStatus::Completed,
        created_at: now,
        updated_at: now,
        completed_at: Some(now),
        result_summary: Some("build finished".to_string()),
    })
    .await
    .unwrap();
    let notification = omini_domain::conversation::TaskNotification {
        tasks: vec![omini_domain::task::TaskCompletion {
            task_id: "bash_task".to_string(),
            kind: omini_domain::task::TaskKind::Bash,
            label: "Bash".to_string(),
            title: "Run build command".to_string(),
            status: TaskStatus::Completed,
            summary: Some("build finished".to_string()),
        }],
        created_at: now,
    };

    let inserted = db
        .insert_task_notification("owner", &notification, now, &project.thread("owner"))
        .await
        .unwrap();
    assert!(inserted.is_some());

    let history = load_messages(&db, "owner", &project.thread("owner")).await;
    assert!(matches!(
        history.as_slice(),
        [omini_domain::conversation::ConversationEntry::SystemEvent(
            omini_domain::conversation::SystemEvent::TaskNotification(restored)
        )] if restored == &notification
    ));
    let delivered = background_task_row(&db, "bash_task")
        .await
        .expect("task row")
        .notification_delivered;
    assert!(delivered);
}

/// 回归:启动恢复只取消"未终态"的工具调用;已完成/已失败的历史记录
/// 与等待审批步骤的 finished_at 必须原样保留(旧顺序化 UPDATE 的语义)。
#[tokio::test]
async fn recovery_keeps_terminal_tool_uses_and_approval_finish_time() {
    let (db, project, root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let now = fixed_time();
    let task = test_agent_task("task_recovery", "child_recovery", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("child_recovery", "owner"),
        &crate::support::store::test_user_input(&Message::from_user_text("start".into())),
        &Message::from_user_text("start".into()),
    )
    .await
    .unwrap();

    // 构造:带父 Run 的子 Run 处于 cancelled(恢复规则 4 的目标形态),
    // 名下两个工具调用一个已完成一个 pending。
    db.create_agent_run(&AgentRunSnapshot {
        id: "parent_run".to_string(),
        thread_id: "owner".to_string(),
        parent_run_id: None,
        status: AgentRunStatus::Running,
        created_at: now,
        started_at: Some(now),
        finished_at: None,
        total_tokens: 0,
        archived_at: None,
    })
    .await
    .unwrap();
    db.create_agent_run(&AgentRunSnapshot {
        id: "child_cancelled".to_string(),
        thread_id: "child_recovery".to_string(),
        parent_run_id: Some("parent_run".to_string()),
        status: AgentRunStatus::Cancelled,
        created_at: now,
        started_at: Some(now),
        finished_at: Some(now),
        total_tokens: 0,
        archived_at: None,
    })
    .await
    .unwrap();
    let step = AgentStepSnapshot {
        id: "step_r".to_string(),
        run_id: "child_cancelled".to_string(),
        step_no: 0,
        status: AgentStepStatus::Running,
        started_at: now,
        finished_at: None,
        input_tokens: 0,
        output_tokens: 0,
    };
    db.upsert_agent_step(&step).await.unwrap();
    let done_tool = ToolUseExecutionSnapshot {
        id: "tool_done".to_string(),
        step_id: "step_r".to_string(),
        name: "read".to_string(),
        input: serde_json::json!({"path": "a"}),
        status: ToolUseStatus::Completed,
        updated_at: now,
    };
    let pending_tool = ToolUseExecutionSnapshot {
        id: "tool_pending".to_string(),
        step_id: "step_r".to_string(),
        name: "bash".to_string(),
        input: serde_json::json!({"cmd": "ls"}),
        status: ToolUseStatus::Pending,
        updated_at: now,
    };
    db.upsert_tool_use_execution(&done_tool, ToolUseStatus::Completed)
        .await
        .unwrap();
    db.upsert_tool_use_execution(&pending_tool, ToolUseStatus::Pending)
        .await
        .unwrap();
    // 另造一个无父 Run 的 waiting_approval,名下 running 步骤不得获得 finished_at。
    let approval_step = AgentStepSnapshot {
        id: "step_approval".to_string(),
        run_id: "approval_run".to_string(),
        step_no: 0,
        status: AgentStepStatus::Running,
        started_at: now,
        finished_at: None,
        input_tokens: 0,
        output_tokens: 0,
    };
    db.create_agent_run(&AgentRunSnapshot {
        id: "approval_run".to_string(),
        thread_id: "owner".to_string(),
        parent_run_id: None,
        status: AgentRunStatus::WaitingApproval,
        created_at: now,
        started_at: Some(now),
        finished_at: None,
        total_tokens: 0,
        archived_at: None,
    })
    .await
    .unwrap();
    db.upsert_agent_step(&approval_step).await.unwrap();

    drop(db);
    let (db, _project, _root) = temp_db_existing(&root.path.join("omini.sqlite")).await;

    let tools = db
        .list_tool_use_executions("child_cancelled")
        .await
        .unwrap();
    let by_id = |id: &str| tools.iter().find(|tool| tool.id == id).unwrap();
    assert_eq!(by_id("tool_done").status, ToolUseStatus::Completed);
    assert_eq!(by_id("tool_done").updated_at, now);
    assert_eq!(by_id("tool_pending").status, ToolUseStatus::Cancelled);
    let steps = db.list_agent_steps("approval_run").await.unwrap();
    assert_eq!(steps[0].status, AgentStepStatus::WaitingApproval);
    assert_eq!(
        steps[0].finished_at, None,
        "approval wait must not gain a finish time"
    );
}
