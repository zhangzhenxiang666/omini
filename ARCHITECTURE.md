# 架构理念

`omini` 分三层：终端客户端负责交互，本地服务负责项目与会话管理，Agent 核心负责执行。典型链路：CLI 以规范化目录注册项目，TUI 取得线程控制权并订阅事件，server 校验后经会话句柄驱动 core，core 发布领域事件，持久化经 `AgentHost` 回到 server 完成。

```text
omini-cli / omini-tui
        │ HTTP + WebSocket（omini-protocol）
        ▼
   omini-server
        │ 执行契约（omini_core::execution）与事件类型（omini-runtime-contract）
        ▼
    omini-core
```

本文只讲分层边界、核心契约与不变量；具体行为、交互与配置细节以代码和 `docs/` 为准。

## Crate 一览

| Crate | 职责 |
| --- | --- |
| `omini-cli` | 程序入口、服务发现与启动、项目注册。 |
| `omini-tui` | 终端交互、界面状态与渲染、协议请求。 |
| `omini-protocol` | 客户端与服务端共用的公开 HTTP/WebSocket 类型。 |
| `omini-server` | 本地服务、项目与会话生命周期、事件投影与重放、持久化业务层（实现 `AgentHost`）。 |
| `omini-runtime-contract` | 服务端与核心之间的命令、事件和快照。 |
| `omini-entity` | SQLite 实体声明（toasty ORM）、建库与连接、大内容 sidecar。 |
| `omini-core` | Agent 实例与运行执行、工具、提示词、Skill、子 Agent、计划、压缩及 Provider/MCP 编排。 |
| `omini-model` | Provider 对话上下文的消息、角色和内容块。 |
| `omini-config` | 用户和项目配置及 Omini 管理的文件路径。 |
| `omini-domain` | 不含传输、运行时或持久化逻辑的共享领域类型。 |
| `omini-permissions` | 权限策略解析及允许、询问、拒绝决策。 |
| `omini-provider-api` | Provider 的 HTTP/SSE 客户端及请求响应处理。 |
| `omini-mcp-client` | MCP 连接、生命周期、工具目录和远程调用。 |

## 边界原则

- 依赖方向自上而下。`omini-protocol` 是唯一的公开客户端/服务端边界，`omini-runtime-contract` 是内部服务端/核心边界；两者都不放运行时实现。
- `omini-domain` 只承载跨 crate 共享的词汇。配置、密钥、编排、持久化、传输封装和界面状态由各自 crate 管理。
- Provider、MCP 和权限是独立 crate，实现不通过协议或运行时契约泄漏。
- server 只依赖 core 公开的 `omini_core::execution` 能力，不触碰其内部模块；core 不直接持久化，持久化与资源申请经异步 `AgentHost` 接口由 server 同步完成后核心才继续。
- 输入链路按层分工：TUI 产出有序语义片段、只传不透明附件 ID；server 负责路径规范化、附件归属与控制权校验；core 负责 Skill 展开与模型上下文构造。
- TUI 内部是单向数据流：输入与服务端事件先更新状态并返回待执行副作用，应用循环执行后把结果作为事件送回；视图只读状态。

## 会话执行模型

持久会话（thread 记录）、跨轮次 Agent 实例（`AgentInstance`）与一次运行（`RunId`）三层分离：server 管理会话缓存与装配，core 管理执行与父子任务调度；主 Run 与子 Agent 任务共用同一执行器。

core 的对外契约全部集中在 `omini_core::execution`：

- `AgentHandle`：可克隆命令通道，命令附带 ack 确认。
- `AgentEvents`：独占单消费者输出，发布领域事件；输出断开即实例收尾。
- `AgentSnapshot`：权威状态查询，回收等决策以它为准。

构建阶段完成装配并交出输出接收端，宿主接好消费者后才启动实例，启动输出不会遗漏。server 侧会话以单消费者任务顺序消费核心输出，驱动协议投影与广播。子 Agent 会话由 server 经 `AgentHost::create_agent_session` 原子建立，core 不创建 thread 目录或组装记录。

## 持久化理念

- `omini-entity` 的模型即 schema 的单一权威（`push_schema` 建库）：状态/种类列直接使用领域枚举原生落库（`toasty::Embed`，视同 serde 的表示标注），无平行 DTO 与转换层。跨 crate 词表留在其语义 crate 原地派生，仅 entity 与 store 消费的纯持久化词表放 `omini-entity`。
- `omini-server` 的 store 层组合模型并承载业务策略（幂等闸门、投递结算、启动恢复、树删除）；核心事件只表达领域事实。
- 数据库无外键与级联：引用完整性由创建边界校验，树删除由业务层在事务内显式完成。
- 假设同一数据库文件只有单个 daemon 进程写入。

## 关键不变量

- **两条消息序列互相独立**：用户可见时间线用 `ConversationEntry`，Provider 上下文用 `omini-model::Message`，两者顺序可以不同；core 不构造 UI 历史项。
- **安全输入边界**：用户消息只在运行开始或运行中干预的排空点加入模型上下文。
- **先持久化后可见**：任务完成通知等事实先落盘，再进入模型历史或界面。
- **项目身份与路径分离**：`id` 与 `storage_key` 不可变，`path` 可重新关联；重连按项目 ID 恢复，路径不作为身份。
