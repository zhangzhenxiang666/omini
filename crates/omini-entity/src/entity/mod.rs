//! 一文件一实体的模型声明。
//!
//! 模型即领域类型:每个表只有一个模型结构(无平行 DTO、无转换层),
//! 状态/种类列直接使用领域枚举(`toasty::Embed` 原生落库,snake_case
//! 标签与历史词表一致),时间列是 toasty 原生 jiff::Timestamp。
//!
//! 声明即 API:`#[key]`/`#[unique]`/`#[index]` 生成 `get_by_*`、
//! `upsert_by_*`、`filter_by_*` 访问器;关系字段生成延迟导航与作用域
//! 查询;`#[auto] updated_at` 提供"更新即触碰"语义(显式赋值可覆盖)。
//! 表结构由这些声明经 push_schema 落库——模型是 schema 的单一权威。
//!
//! 数据库层面没有外键强制与级联:删除父行的业务(如线程树删除)由
//! 业务代码在事务内显式删除全部关联行。JSON 载荷(result/input/payload
//! 等)以 TEXT 列存储,序列化由业务代码负责。
//!
//! 字段注释按需:struct 文档讲表职责与访问器语义,字段 `///` 只写
//! 名字与类型表达不了的语义——不变量与生命周期、统计口径、可空
//! 含义、跨表引用责任;自明字段(`#[key]` 主键、`#[auto]` 时间戳等)
//! 不加注释。

mod agent_run;
mod agent_step;
mod agent_task;
mod agent_task_delivery;
mod attachment;
mod background_task;
mod llm_message;
mod message;
mod project;
mod thread;
mod tool_use_execution;

pub use agent_run::AgentRun;
pub use agent_step::AgentStep;
pub use agent_task::AgentTask;
pub use agent_task_delivery::{AgentTaskDelivery, DeliveryStatus, SourceKind};
pub use attachment::Attachment;
pub use background_task::BackgroundTask;
pub use llm_message::LlmMessage;
pub use message::{Message, MessageKind};
pub use project::Project;
pub use thread::{Thread, ThreadType, parse_thinking_effort, thread_from_runtime};
pub use tool_use_execution::ToolUseExecution;
