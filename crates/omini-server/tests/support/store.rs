//! store 层集成测试的公共夹具:临时库、实体样例与类型化断言助手。

use omini_config::project::ProjectDir;
// 各测试文件按需取用,glob 导入下未逐项使用是预期形态。
#[allow(unused_imports)]
pub use omini_entity::CONTENT_SIZE_THRESHOLD;
#[allow(unused_imports)]
pub use omini_entity::test_support::{
    TEST_PROJECT_ID, TestRoot, fixed_time, test_agent_task, test_agent_thread, test_thread,
};
use omini_entity::{BackgroundTask, LlmMessage, Message, MessageKind};
use omini_model::message::Role;
use omini_server::store::{Project, Store};

pub async fn temp_db() -> (Store, ProjectDir, TestRoot) {
    let root = TestRoot::new();
    let db = Store::open(&root.path.join("omini.sqlite")).await.unwrap();
    let now = fixed_time();
    db.create_project(&Project {
        id: TEST_PROJECT_ID.to_string(),
        name: "test project".to_string(),
        path: root.path.join("cwd").display().to_string(),
        storage_key: "test-project".to_string(),
        created_at: now,
        updated_at: now,
        last_opened_at: None,
        threads: Default::default(),
    })
    .await
    .unwrap();
    (db, ProjectDir::from_path(root.path.join("project")), root)
}

/// 按线程与形态统计 UI 消息数(替代原 raw SQL COUNT 断言)。
pub async fn count_thread_messages(db: &Store, thread_id: &str, kind: MessageKind) -> usize {
    Message::filter(
        Message::fields()
            .thread_id()
            .eq(thread_id)
            .and(Message::fields().kind().eq(kind)),
    )
    .count()
    .exec(&mut db.conn())
    .await
    .expect("message count query") as usize
}

/// 按线程与角色统计模型上下文消息数。
pub async fn count_thread_llm_messages(db: &Store, thread_id: &str, role: Role) -> usize {
    LlmMessage::filter(
        LlmMessage::fields()
            .thread_id()
            .eq(thread_id)
            .and(LlmMessage::fields().role().eq(role)),
    )
    .count()
    .exec(&mut db.conn())
    .await
    .expect("llm message count query") as usize
}

/// 取指定上下文版本的模型消息行数。
pub async fn count_llm_version(db: &Store, thread_id: &str, version: i64) -> usize {
    LlmMessage::filter(
        LlmMessage::fields()
            .thread_id()
            .eq(thread_id)
            .and(LlmMessage::fields().context_version().eq(version)),
    )
    .count()
    .exec(&mut db.conn())
    .await
    .expect("llm version count query") as usize
}

/// 取模型上下文消息的持久化内容(替代原 raw SQL SELECT content 断言)。
pub async fn llm_content_at(
    db: &Store,
    thread_id: &str,
    version: i64,
    ordinal: i64,
) -> Option<String> {
    let mut conn = db.conn();
    LlmMessage::filter(
        LlmMessage::fields()
            .thread_id()
            .eq(thread_id)
            .and(LlmMessage::fields().context_version().eq(version))
            .and(LlmMessage::fields().ordinal().eq(ordinal)),
    )
    .first()
    .exec(&mut conn)
    .await
    .expect("llm content query")
    .map(|row| row.content)
}

/// 取线程首条模型上下文消息的持久化内容。
pub async fn first_llm_content(db: &Store, thread_id: &str) -> Option<String> {
    let mut conn = db.conn();
    LlmMessage::filter(LlmMessage::fields().thread_id().eq(thread_id))
        .order_by(LlmMessage::fields().ordinal().asc())
        .first()
        .exec(&mut conn)
        .await
        .expect("llm content query")
        .map(|row| row.content)
}

/// 按任务 ID 取后台任务行(通知闸门与创建时间保持断言)。
pub async fn background_task_row(db: &Store, task_id: &str) -> Option<BackgroundTask> {
    let mut conn = db.conn();
    BackgroundTask::filter_by_task_id(task_id)
        .first()
        .exec(&mut conn)
        .await
        .expect("background task query")
}

/// 在既有数据库文件上重开 Store(模拟服务重启,触发恢复归一化)。
pub async fn temp_db_existing(db_path: &std::path::Path) -> (Store, ProjectDir, TestRoot) {
    let root = TestRoot::from_path(db_path.parent().expect("db parent"));
    let db = Store::open(db_path).await.unwrap();
    (db, ProjectDir::from_path(root.path.join("project")), root)
}

/// 从样例模型消息派生原样的展示输入，仅用于保留旧测试夹具的输入意图。
pub fn test_user_input(
    message: &omini_model::message::Message,
) -> omini_domain::conversation::UserInput {
    omini_domain::conversation::UserInput {
        intent: omini_domain::input::UserInputIntent::Message,
        parts: message
            .content
            .iter()
            .filter_map(|block| match block {
                omini_model::message::ContentBlock::Text(text) => {
                    Some(omini_domain::input::InputPart::Text {
                        text: text.text.clone(),
                    })
                }
                _ => None,
            })
            .collect(),
        attachments: Vec::new(),
    }
}
