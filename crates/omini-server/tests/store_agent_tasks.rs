mod support;

use crate::support::store::*;
use omini_domain::conversation::{
    AgentMessage, AssistantMessage, AssistantMessageBlock, ConversationEntry, SystemEvent,
    ToolResultRecord, UserInput,
};
use omini_domain::input::{InputPart, UserInputIntent};
use omini_domain::task::TaskStatus;
use omini_model::message::{ContentBlock, Message, Role};
use omini_runtime_contract::persistence::ClientMessage;
use omini_runtime_contract::thread_domain::{AgentTaskResult, DeliveryKey};
use omini_server::history;

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
        db.enqueue_agent_message("task-agent", "owner", "child-agent", &source)
            .await
            .unwrap()
    );
    assert!(
        !db.enqueue_agent_message("task-agent", "owner", "child-agent", &source)
            .await
            .unwrap()
    );
    let changed = AgentMessage {
        text: "changed".into(),
        ..source.clone()
    };
    assert!(
        db.enqueue_agent_message("task-agent", "owner", "child-agent", &changed)
            .await
            .is_err()
    );
    let ui_before_injection =
        history::load_messages(&db, "child-agent", &project.thread("child-agent")).await;
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
    db.inject_task_message("child-agent", &key, &model_message)
        .await
        .unwrap();
    db.inject_task_message("child-agent", &key, &model_message)
        .await
        .unwrap();

    // 则子会话保存来源结构，模型仍收到带来源标注的 User 角色消息。
    let history = history::load_messages(&db, "child-agent", &project.thread("child-agent")).await;
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
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let task = test_agent_task("task-client", "child-client", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("child-client", "owner"),
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
        db.enqueue_client_message("task-client", "owner", "child-client", &first)
            .await
            .unwrap()
    );
    assert!(
        !db.enqueue_client_message("task-client", "owner", "child-client", &first)
            .await
            .unwrap()
    );
    assert!(
        db.enqueue_client_message("task-client", "owner", "child-client", &second)
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
        db.enqueue_client_message("task-client", "owner", "child-client", &changed)
            .await
            .is_err()
    );

    // 当 UI 先记录两条消息时，模型上下文仍可保持原有顺序。
    let ui_before_injection =
        history::load_messages(&db, "child-client", &project.thread("child-client")).await;
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
        db.inject_task_message("child-client", &key, &model_message)
            .await
            .unwrap();
    }
    let pending = ClientMessage {
        client_id: "client-c".into(),
        client_echo_id: "echo-1".into(),
        input: input.clone(),
    };
    db.enqueue_client_message("task-client", "owner", "child-client", &pending)
        .await
        .unwrap();
    db.initialize().await.unwrap();
    db.initialize().await.unwrap();

    // 则快照来源键对应三条 UI 消息，模型仅有两条，任务结果记录未送达数量。
    let history =
        history::load_messages(&db, "child-client", &project.thread("child-client")).await;
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
    assert_eq!(
        db.get_owned_task("owner", "task-client")
            .await
            .unwrap()
            .unwrap()
            .result
            .unwrap()
            .undelivered_messages,
        Some(1)
    );
    assert_eq!(
        db.client_delivery("task-client", &pending)
            .await
            .unwrap()
            .unwrap()
            .1,
        "failed"
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
    db.enqueue_agent_message("task-order", "owner", "child-order", &agent)
        .await
        .unwrap();
    db.enqueue_client_message("task-order", "owner", "child-order", &client)
        .await
        .unwrap();

    // 当安全边界以相反顺序消费时，模型历史仍按实际消费顺序追加。
    let client_model = Message::from_user_text("second in UI".into());
    let agent_model = Message::from_user_text("来自主 Agent 的消息：\nfirst in UI".into());
    db.inject_task_message(
        "child-order",
        &DeliveryKey::from_client("task-order", &client.client_id, &client.client_echo_id),
        &client_model,
    )
    .await
    .unwrap();
    db.inject_task_message(
        "child-order",
        &DeliveryKey::from_agent("task-order", &agent.source_run_id, &agent.tool_use_id),
        &agent_model,
    )
    .await
    .unwrap();

    // 则两个序列各自保留顺序，且快照按两个来源键去重。
    let history = history::load_messages(&db, "child-order", &project.thread("child-order")).await;
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
        &Message::from_user_text("start".into()),
    )
    .await
    .unwrap();
    let message = AgentMessage {
        source_run_id: "run-1".into(),
        tool_use_id: "tool-1".into(),
        text: "check".into(),
    };
    db.enqueue_agent_message("task-cancel", "owner", "child-cancel", &message)
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
    let child = history::load_messages(&db, "child-cancel", &project.thread("child-cancel")).await;
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
        history::load_messages(&db, "owner", &project.thread("owner"))
            .await
            .is_empty()
    );
    assert!(
        db.enqueue_agent_message("task-cancel", "owner", "child-cancel", &message)
            .await
            .is_err()
    );
    let key = DeliveryKey::from_agent("task-cancel", &message.source_run_id, &message.tool_use_id);
    assert!(
        db.inject_task_message(
            "child-cancel",
            &key,
            &Message::from_user_text("来自主 Agent 的消息：\ncheck".into()),
        )
        .await
        .is_err()
    );
}

#[tokio::test]
// 创建失败不得留下子线程；重启后运行中任务必须按终态恢复。
async fn agent_task_creation_and_recovery() {
    let (db, project, _root) = temp_db().await;
    project.create_thread("owner").unwrap();
    db.create_thread(&test_thread("owner")).await.unwrap();
    let initial = Message::from_user_text("do work".to_string());

    let task = test_agent_task("task_running", "agent_running", "owner");
    db.create_agent_task(
        TEST_PROJECT_ID,
        &task,
        &test_agent_thread("agent_running", "owner"),
        &initial,
    )
    .await
    .unwrap();
    assert_eq!(db.get_messages("agent_running").await.unwrap().len(), 2);
    assert_eq!(
        history::load_messages(&db, "agent_running", &project.thread("agent_running")).await,
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
        "user",
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
        "assistant",
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
        "user",
        None,
        fixed_time(),
        &project.thread("agent_running"),
    )
    .await
    .unwrap();
    assert_eq!(
        history::load_messages(&db, "agent_running", &project.thread("agent_running")).await,
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
            &initial,
        )
        .await
        .is_err()
    );
    assert!(db.get_thread("agent_rolled_back").await.unwrap().is_none());

    db.initialize().await.unwrap();
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
    let llm_message = Message::from_user_text("agent task completed".to_string());
    for _ in 0..2 {
        db.insert_task_notification(
            "owner",
            &notification,
            &llm_message,
            &["task_done".to_string()],
            completed_at,
        )
        .await
        .unwrap();
    }
    let after = Message::new(
        Role::Assistant,
        vec![ContentBlock::from_text("after notification".to_string())],
    );
    db.append_llm_message("owner", &after, fixed_time(), &project.thread("owner"))
        .await
        .unwrap();

    let ui_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM messages WHERE thread_id = 'owner' AND kind = 'conversation_entry'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    let llm_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM llm_messages WHERE thread_id = 'owner' AND role = 'user'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
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
        history::load_messages(&db, "owner", &project.thread("owner"))
            .await
            .as_slice(),
        [omini_domain::conversation::ConversationEntry::SystemEvent(
            omini_domain::conversation::SystemEvent::TaskNotification(restored)
        )] if restored == &notification
    ));
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

    db.insert_task_notification(
        "owner",
        &notification,
        &Message::from_user_text("bash task completed".to_string()),
        &["bash_task".to_string()],
        now,
    )
    .await
    .unwrap();

    let history = history::load_messages(&db, "owner", &project.thread("owner")).await;
    assert!(matches!(
        history.as_slice(),
        [omini_domain::conversation::ConversationEntry::SystemEvent(
            omini_domain::conversation::SystemEvent::TaskNotification(restored)
        )] if restored == &notification
    ));
    let delivered: i64 = sqlx::query_scalar(
        "SELECT notification_delivered FROM background_task WHERE task_id = 'bash_task'",
    )
    .fetch_one(&db.pool)
    .await
    .unwrap();
    assert_eq!(delivered, 1);
}
