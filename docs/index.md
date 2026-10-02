# Omini 文档

从[使用指南](usage.md)开始；配置模型服务时查阅[配置参考](configuration.md)。

## 使用与配置

| 文档 | 解决的问题 |
| --- | --- |
| [使用指南](usage.md) | 启动项目、管理会话、使用命令和停止任务 |
| [配置参考](configuration.md) | 配置 Provider、模型、凭据、MCP 和自动压缩 |
| [权限配置](permissions.md) | 决定哪些工具或命令允许执行、需要确认或禁止执行 |
| [Agent 指令](instructions.md) | 用 `AGENTS.md` 定义全局偏好与项目规范 |
| [Skills](skills.md) | 将重复工作写成可调用的技能 |

## 配置文件位置

| 类型 | 用户级路径 | 项目级路径 |
| --- | --- | --- |
| 主配置 | `~/.omini/config.toml` | `<project>/.omini/config.toml` |
| 工具权限 | 主配置的 `[permissions]` 段 | 主配置的 `[permissions]` 段和 `<project>/.omini/permissions.toml` |
| Bash 规则 | `~/.omini/rules/*.rules` | `<project>/.omini/rules/*.rules` |
| Agent 指令 | `~/.omini/AGENTS.md` | `<project>/AGENTS.md` |
| Skills | `~/.omini/skills/<name>/SKILL.md` | `<project>/.omini/skills/<name>/SKILL.md` |

运行 Agent 前需要配置至少一个 Provider 和模型。其余配置按需添加；项目级配置可覆盖全局配置，具体合并规则见各文档。

## 开发与接入

| 文档 | 解决的问题 |
| --- | --- |
| [开发指南](development.md) | 从源码构建、运行、验证和预览组件 |
| [客户端协议](protocol.md) | 注册客户端、提交输入、处理子任务与图片附件 |
| [架构说明](../ARCHITECTURE.md) | 理解 crate 职责、执行契约和持久化边界 |

[返回项目首页](../README.md)
