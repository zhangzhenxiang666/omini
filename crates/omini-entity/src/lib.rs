//! omini 实体层:SQLite 数据模型的一文件一实体声明(toasty ORM)。
//!
//! 本 crate 只负责"数据长什么样"——实体(行模型)与持久化列值词表、
//! 连接与建库([`Database::open`]),以及大内容落盘 sidecar 的存储机制。
//! 表结构由实体声明经 `push_schema` 生成,模型即 schema 的单一权威,
//! 库内不存在手写 DDL。
//!
//! 查询与业务策略不在本 crate:业务代码(如 omini-server 的 store 层)
//! 直接用 toasty 查询 API 组合这些公开的行模型与 [`Database::conn`]
//! 句柄,事务同样由业务代码开启。

mod content;
mod database;
/// 一文件一实体的模型声明,含行模型与持久化列值词表。
pub mod entity;
/// 集成测试与下游 crate 测试共用的夹具(临时库、固定时间、样例实体)。
pub mod test_support;

pub use content::{
    CONTENT_SIZE_THRESHOLD, MAX_SIDECAR_LOAD_BYTES, PreparedBlocks, PreparedUiContent,
    cleanup_created_files, finish_prepared_write, load_asset, load_blocks, load_ui_content,
    persist_staged_asset, prepare_blocks, prepare_ui_content, stored_asset_path,
};
pub use database::{Database, StoreError};
pub use entity::*;
