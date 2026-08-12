//! `omini-permissions`：运行时权限决策层。
//!
//! 该 crate 负责把来自 `omini_config::permissions::PermissionSources` 的原始权限配置
//! 解析为工具规则和 bash 规则，并根据 `PermissionPreview`、active profile、
//! 编译时内嵌 bash 策略和代码层安全底线做 allow / ask / deny 决策。
//!
//! **配置文件加载不在这里**：读 `~/.omini/config.toml [permissions]`、
//! `<cwd>/.omini/permissions.toml` 项目权限配置、扫描 `~/.omini/rules/*.rules`
//! 与 `<cwd>/.omini/rules/*.rules` 全部由
//! [`omini_config::permissions::load_permission_sources`] 完成；本 crate 只
//! 消费 `PermissionSources`、做规则 DSL 解析与最终决策。

mod bash_parser;
mod bash_safety;
mod embedded;
mod engine;
mod path_matcher;
mod shell;
mod tool_rules;

// 公开 API re-exports。
pub use engine::{PermissionCheck, PermissionDecision, PermissionEngine};
