# 架构概览

`omini` 由终端客户端、本地服务和 Agent 核心组成：客户端负责交互，服务端负责项目与线程管理，核心负责 Agent 执行。

```text
omini-cli / omini-tui
        │ HTTP + WebSocket（omini-protocol）
        ▼
   omini-server
        │ 命令、事件、快照（omini-runtime-contract）
        ▼
    omini-core
```

## Crate 职责

| Crate | 职责 |
| --- | --- |
| `omini-cli` | 程序入口、服务发现与启动、项目注册、启动 TUI。 |
| `omini-tui` | 终端交互、界面状态与渲染、协议请求。 |
| `omini-protocol` | 客户端与服务端共用的公开 HTTP/WebSocket 类型。 |
| `omini-server` | 本地服务、项目和线程生命周期、事件投影与重放、SQLite 持久化。 |
| `omini-runtime-contract` | 服务端与核心之间的命令、事件、快照和持久化请求。 |
| `omini-core` | Agent 执行、工具、提示词、Skill、子 Agent、计划、压缩及 Provider/MCP 编排。 |
| `omini-config` | 用户和项目配置，以及 Omini 管理的文件路径。 |
| `omini-domain` | 不含传输、运行时或持久化逻辑的共享领域类型。 |
| `omini-permissions` | 权限策略解析及允许、询问、拒绝决策。 |
| `omini-provider-api` | Provider 的 HTTP/SSE 客户端及请求响应处理。 |
| `omini-mcp-client` | MCP 连接、生命周期、工具目录和远程调用。 |

依赖边界：

- `omini-protocol` 是公开客户端/服务端边界；`omini-runtime-contract` 是内部服务端/核心边界，两者不放运行时实现。
- `omini-domain` 只承载共享词汇。配置、密钥、编排、持久化、传输封装和界面状态由各自 crate 管理。
- Provider、MCP 和权限逻辑由独立 crate 提供，不通过协议或运行时契约泄漏实现。
- SQLite、事务、事件重放和持久化投影归服务端；核心事件只表达领域事实。
- 服务端通过核心公开的项目/线程能力工作，不依赖核心内部的 Skill、任务、工具或引擎模块。

## 项目身份与存储

| 字段 | 含义 |
| --- | --- |
| `id` | 稳定公开 ID，也是服务缓存键和线程外键。 |
| `path` | 规范化后的工作目录，可通过重新关联更新。 |
| `storage_key` | `~/.omini/projects/` 下稳定的目录名。 |
| `name` | 面向用户的显示名称。 |

`id` 和 `storage_key` 不变；重新关联只更新 `path`，且项目有运行中或已连接的缓存线程时拒绝操作。所有线程（包括分支和子 Agent 任务）都属于某个项目。

## 运行流程

1. CLI 启动或连接本地服务，注册当前规范化目录，并打开服务返回的项目 ID。
2. 服务端从 SQLite 解析项目，按需创建 `ProjectManager`，使用当前路径和稳定存储目录。
3. TUI 创建或选择线程、取得控制权并订阅事件。
4. 服务端校验请求和控制权，再经运行时边界调用核心能力。提交用户输入时，服务端先保存并广播用户可见消息，再派发执行。
5. 核心运行 Agent 并发出运行时和持久化事件；服务端负责保存、投影和广播。核心只在安全输入边界（运行开始或运行中干预排空点）将用户消息加入模型上下文。
6. 重连时按项目 ID 恢复；项目路径不作为身份。

## Agent Run 与工具调用

- `AgentRun` 是一次运行的治理和查询单位，关联 Thread、可选父 Run、类型、状态、时间和累计 Token。Agent Run 的每次模型调用是一个 `AgentStep`；同一响应产生的多个 ToolUse 记录在该 Step 下，并在全部收敛后继续下一 Step。Bash 等非 Agent Run 不创建虚假的 Step。
- runtime 控制事件用可选 `run_id` 统一面向主 Run 与子 Run：`None` 表示当前 Thread 的主 Run，`Some(id)` 表示指定子 Run。取消主 Run 会同时取消其子任务；取消子 Run 会影响其后代。取消和插话各自保留单一事件类型，具体目标由该字段区分。
- Run、Step、ToolUse 的事实保存在服务端 SQLite，并经 runtime contract 的持久化意图写入。协议 revision 4 暴露 Run 快照、详情、归档状态和状态事件，并增加通用后台任务状态及 Bash 输出增量事件；Run 记录通过归档标记隐藏，不物理删除。
- `Tool` 只描述强类型输入、名称、说明、Schema 和调用行为。`ToolPolicy<T>` 按具体 Tool 类型绑定，在注册时提供外部预检与权限预览；参数解析、profile 策略、权限暂停和执行编排由 ToolRegistry/运行时负责。
- 子 Agent 的 Run ID 与其 task ID 相同，并由父 Run 关联。服务重启会中断主运行、取消后台子任务；等待审批的主 Run 元数据及 ToolUse 保留供恢复流程识别。

用户可见消息按发言时间保存，因此重放顺序可能与运行中干预进入模型上下文的顺序不同。`UserMessageInjected` 仅属于客户端协议：普通输入由 server 保存后投影，任务通知在持久化成功后投影；核心只发领域事实和模型上下文消息，不构造 UI 历史项。

## 输入与附件

- TUI 将输入框内容转为有序语义片段；文本、Skill、项目文件/目录和子 Agent 保持相对顺序。图片标记先上传，协议只传不透明附件 ID，不传客户端路径。
- 服务端负责路径规范化、附件归属与完整性校验、控制权校验；核心负责 Skill 和 `/init` 展开、子 Agent 校验、模型模态检查，并构造单条 Provider 用户消息。
- UI 历史保存原始输入意图和附件元数据；展开后的 Skill 内容、内部 `/init` 提示词及图片数据只进入模型上下文。
- 附件内容存于线程目录下的内容寻址文件，SQLite 将附件 ID 映射到文件。

## 子 Agent 任务

派生深度上限为 `MAX_AGENT_DEPTH = 2`：主 Agent 可创建后台任务，一级任务可在工具策略允许时同步运行二级 Agent，二级任务不能继续派生。主线程最多同时运行 8 个后台任务和 10 个同步任务；超限请求作为工具错误拒绝，不创建任务。

`TaskManager` 为后台任务提供统一 ID、类型、owner、状态、并发槽位、列表、查询、等待、取消和完成通知。执行器持有进程或子线程等专属状态，并向 Manager 注册通用取消信号；完成通知使用通用 `TaskCompletion`。SubAgent 适配器继续管理子线程、消息、AgentRun 与后代取消；同步 `run_agent` 保留在 SubAgent 执行路径。

只有主 Agent 可启动异步 SubAgent，或将运行超过 30 秒的 Bash 命令转为后台任务；达到阈值但并发槽位已满时，Bash 保持前台执行并遵循原超时。`read_task`、`wait_tasks` 和 `cancel_task` 可操作不同类型的后台任务。管理器按 owner 提供近期跨类型列表与状态筛选，但本期不增加 Agent 列表工具或 Client 查询 API。

SubAgent 任务归属于主线程并拥有子线程；Bash 任务关联原始工具调用。Bash stdout/stderr 以带 task ID、tool use ID 和流类型的 `TaskOutput` 事件增量发送，服务端在进程内为重连保留受限近期输出尾部。Client 接收通用任务状态和输出事件，自行决定呈现方式。最终结果持久化，增量输出只驻留内存；服务重启或关闭不恢复进程，启动时将未结束任务标记为 `interrupted`。

已提交消息是持久化边界。完成通知先持久化，再进入模型历史或界面，以保持“当前轮次 → 任务通知 → 下一轮回复”的顺序。服务重启后不恢复运行中的任务，未提交的增量可丢弃。取消任务会影响其后代，不影响兄弟任务；取消前台运行或关闭运行时也会取消主线程拥有的任务树。
