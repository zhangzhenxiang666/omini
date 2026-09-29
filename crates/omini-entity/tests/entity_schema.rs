//! schema 门与建库幂等性:push_schema 不带 IF NOT EXISTS,重开必须跳过。

use omini_entity::Database;

fn temp_path(label: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "omini-entity-schema-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

fn drop_db(path: &std::path::Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}

/// 全新库建出全部表;同一文件重复 open 不再执行 push_schema。
#[tokio::test]
async fn fresh_open_creates_schema_and_reopen_is_idempotent() {
    let path = temp_path("reopen");
    drop_db(&path);
    let db = Database::open(&path).await.expect("first open");
    drop(db);

    let db = Database::open(&path).await.expect("second open");
    let mut conn = db.conn();
    let threads = omini_entity::Thread::all().exec(&mut conn).await.unwrap();
    assert!(threads.is_empty(), "reopened database must be usable");
    drop(db);
    drop_db(&path);
}

/// 部分建表的数据库文件必须显式报错并引导重建,不做猜测性修复。
#[tokio::test]
async fn partial_schema_file_is_rejected() {
    let path = temp_path("partial");
    drop_db(&path);
    let db = Database::open(&path).await.expect("seed open");
    // 删掉一张表,模拟建库中断或文件损坏后只剩部分表。
    toasty::sql::statement("DROP TABLE attachment")
        .exec(&mut db.conn())
        .await
        .expect("drop table");
    drop(db);

    let error = match Database::open(&path).await {
        Ok(_) => panic!("partial schema must be rejected"),
        Err(error) => error,
    };
    assert!(
        error.to_string().contains("incompatible schema"),
        "unexpected error: {error}"
    );
    drop_db(&path);
}

/// 未知词表值必须按数据损坏拒绝,而非静默默认。
#[test]
fn closed_vocab_parses_reject_unknown_values() {
    use omini_entity::{SourceKind, StoreError, ThreadType};

    assert!(ThreadType::parse("main").is_ok());
    assert!(matches!(ThreadType::parse("agent"), Ok(ThreadType::Agent)));
    assert!(matches!(
        ThreadType::parse("bogus"),
        Err(StoreError::InvalidData(_))
    ));

    assert!(matches!(SourceKind::parse("agent"), Ok(SourceKind::Agent)));
    assert!(matches!(
        SourceKind::parse("client"),
        Ok(SourceKind::Client)
    ));
    assert!(matches!(
        SourceKind::parse("bogus"),
        Err(StoreError::InvalidData(_))
    ));

    // 运行时记录携带的未知线程类型/思考力度同样在映射边界被拒绝。
    use omini_entity::test_support::test_agent_thread;
    let mut runtime = test_agent_thread("child", "parent");
    runtime.thread_type = "bogus".to_string();
    assert!(matches!(
        omini_entity::thread_from_runtime("p1", &runtime),
        Err(StoreError::InvalidData(_))
    ));
    let mut runtime = test_agent_thread("child", "parent");
    runtime.thinking_effort = Some("ultra".to_string());
    assert!(matches!(
        omini_entity::thread_from_runtime("p1", &runtime),
        Err(StoreError::InvalidData(_))
    ));
}
