use jiff::Timestamp;
use toasty::Deferred;

use super::Thread;

/// 项目表:注册过的本地工作目录,`path` 与 `storage_key` 唯一。
#[derive(Debug, Clone, toasty::Model)]
#[table = "project"]
pub struct Project {
    /// 稳定公开 ID,也是服务缓存键和线程外键。
    #[key]
    pub id: String,
    pub name: String,
    /// 规范化工作目录,重新关联时可更新。
    #[unique]
    pub path: String,
    /// `~/.omini/projects/` 下稳定的目录名。
    #[unique]
    pub storage_key: String,
    #[auto]
    pub created_at: Timestamp,
    #[auto]
    pub updated_at: Timestamp,
    pub last_opened_at: Option<Timestamp>,
    /// 所属线程(导航用;没有删除项目的路径)。
    #[has_many]
    pub threads: Deferred<Vec<Thread>>,
}
