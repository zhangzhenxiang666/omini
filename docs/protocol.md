# Client / Server Protocol

Omini 的公开 client/server 协议位于 `/v1`，当前 `protocol_revision` 为 `9`。客户端必须在连接前检查 `GET /v1/health` 返回的 `protocol_revision`。完整 HTTP 路径和响应描述见 `GET /v1/openapi.json`；调试构建还提供 `/docs`。

Revision 8 调整 HTTP 状态与响应形状：注册客户端及创建项目、线程返回 `201`，提交异步运行返回 `202` 和 `run_id`；没有返回数据的命令返回 `204` 空响应。查询和返回配置快照的接口仍返回 `200`。单线程状态接口直接返回状态对象，不再包一层 `status`。`POST /v1/clients` 不定义请求体。`/threads/{thread_id}/open` 与 `/runs/{run_id}/messages` 已删除。所有 HTTP 错误体使用 `{ "code": "...", "message": "..." }`，包括 JSON、路径和查询参数解析错误。

## 会话历史

线程快照中的每条历史记录使用 `HistoryItem` 的 `type` 区分 `user_input`、`assistant_message` 和 `system_event`。系统事件使用各自的事件类型保存计划、压缩摘要、子 Agent 通知和工具结果。用户提交的原始输入与助手可见消息不会复用 Provider 上下文的 `Message` 结构。

工具结果由系统执行工具后生成：它在 Provider 上下文中仍是 `role: "user"` 的 ToolResult 消息，并紧跟对应的 ToolUse；用户可见历史另保存 `system_event` 类型的 `tool_results` 记录。两者用途不同，模型上下文顺序和会话历史顺序分别保持。

线程快照中的 `agent_tasks` 只包含主线程的直接异步子 Agent，每个任务的 `history` 使用同一套 `ConversationEntry` 语义恢复初始提示、用户输入、助手消息和工具结果。同步 `run_agent` 与嵌套任务不作为独立 TUI 会话提供。

## 用户输入

一次普通提交使用有序的语义 `parts`，图片则只通过无序的附件 ID 集合引用：

```json
{
  "type": "submit_message",
  "input": {
    "parts": [
      { "type": "text", "text": "请检查 " },
      { "type": "skill", "name": "code-reviewer" },
      { "type": "file", "path": "src/main.rs", "label": "main" },
      { "type": "text", "text": " 的边界条件" }
    ],
    "attachment_ids": [
      "97f4206d-83b8-4b19-86c6-8cc55936505a"
    ]
  },
  "client_echo_id": "optional-client-id"
}
```

`parts` 支持 `text`、`skill`、`file`、`directory` 和 `subagent`。数组顺序具有语义：服务端按原顺序展开成一条 Provider user message，`label` 仅用于 UI 展示，不能用于推断位置。文件和目录只是项目内的语义引用，不会由协议层自动内嵌内容。

`attachment_ids` 的顺序没有语义，ID 必须非空且不得重复。Core 在进入 Provider 前按 ID 确定性排序，并把图片块追加到所有语义 parts 之后。任何 client 本地路径都不得出现在 wire 请求中。

运行中干预使用同样的 `UserInput`：

```json
{
  "type": "intervene_message",
  "input": { "parts": [{ "type": "text", "text": "先停一下" }] }
}
```

同一线程同时只接受一个运行：已有运行未结束时再次提交返回 HTTP 409 与 `run_busy` 错误码，被拒绝的输入不会保存。运行中干预不受此限制。

主线程的直接异步子 Agent 使用 `POST /v1/projects/{project_id}/threads/{thread_id}/runs/{run_id}/input` 接收同样的结构化输入。这里的 `thread_id` 是主线程，`run_id` 是子任务 ID；请求必须带 `client_echo_id` 和已连接的 `x-omini-client-id`。服务端登记投递并持久化子会话 UI 历史后广播 `agent_task_user_message_queued`；事件携带任务 ID、子线程 ID、原始 `HistoryItem`、客户端 ID 和回显 ID。模型历史等子 Agent 到达安全输入边界才追加，顺序可以与 UI 历史不同。同一客户端、回显 ID 和子任务的重复请求只投递一次；同键不同内容报错，不同客户端的相同正文保留为两条。只能向仍运行的直接子任务发送新输入；终态任务及非直接子任务返回冲突错误，相同来源键的成功重试仍返回成功。

主 Agent 的 `send_message` 使用发送 Run ID、ToolUse ID 和目标 task ID 作为来源键，持久化入队后立即返回。入队时，子任务历史增加 `system_event` 的 `agent_message`，并广播 `agent_task_message_queued`；主时间线以 `↪ 任务标题 · 消息首行` 单行分界展示该调用，正文完整显示在子会话并以 `↳` 前缀标记来源。模型在安全输入边界收到带来源标注的 User 角色消息；UI 历史与模型历史可以有不同顺序。任务取消、异常或服务重启造成待处理消息无法注入时，投递记录标为失败，数量写入任务结果的 `undelivered_messages`，并通过已有任务完成通知的摘要报告。

## 命令分层

`/help` 和 `/exit` 是纯客户端命令；`/compact`、`/rename`、模型选择等调用各自已有的 typed endpoint。未知 slash 输入仍作为普通 `text` 提交。

`/init` 是独占的 run command。客户端只去掉命令名并保留用户原始参数 parts，不展开初始化 prompt：

```json
{
  "type": "execute_command",
  "command": { "type": "init" },
  "input": { "parts": [{ "type": "text", "text": " 额外关注测试规范" }] }
}
```

Core 持有初始化规范 prompt。UI 历史只保存 `/init` 意图和原始 parts；内部 prompt 仅进入 LLM history。无可见文本的首次 `/init` 使用固定标题 `Initialize project`。

`/skill-name prompt` 被编码为 `skill` part，后面跟原始参数中的 `text`、文件、目录或 subagent parts。客户端不下载 `SKILL.md` 正文。Core 使用线程当前的 `SkillRegistry` 校验 skill 存在且 `user-invocable`，然后在该 part 的位置展开，并把展开结果快照到 LLM history。`GET /v1/projects/{project_id}/threads/{thread_id}/skills` 只返回可调用摘要，包括可选的 `argument_hint`。

## 项目引用校验

Server 在接受 run 前同步校验每个文件和目录 part：路径必须是项目内存在的规范相对路径，不能是绝对路径，不能包含 `..`，也不能通过 symlink 逃逸项目根目录。Subagent 和 Skill 必须来自线程当前 registry。所有 parts、附件归属以及模型图片能力都通过校验后，请求才返回 accepted。

## 图片附件

上传接口：

```text
POST /v1/projects/{project_id}/threads/{thread_id}/attachments
Content-Type: multipart/form-data
x-omini-client-id: <controller id>
```

multipart 必须且只能包含一个名为 `file` 的字段。Server 将请求流式写入 thread staging，单文件上限为 20 MiB，并同时校验声明 MIME 与 magic bytes。当前只接受 PNG、JPEG、WebP 和 GIF。成功响应为：

```json
{ "attachment_id": "97f4206d-83b8-4b19-86c6-8cc55936505a" }
```

返回值是 opaque UUID，不暴露哈希或存储路径。上传要求当前 controller；读取为只读操作，不要求 controller：

```text
GET /v1/projects/{project_id}/threads/{thread_id}/attachments/{attachment_id}
```

读取时会严格验证 project/thread 归属，错误归属和不存在都返回 404。成功响应返回原始二进制，并设置 `Content-Type`、`Content-Length`、`Content-Disposition`、`ETag` 与 `X-Content-Type-Options: nosniff`。

附件元数据记录在 SQLite `attachment` 表：`id`、`thread_id`、`original_name`、`mime_type`、`size`、`sha256`、`relative_path`、`created_at`。内容存放在对应 thread 的 `assets/<sha256>.<ext>`；相同内容可以共享文件，但每次上传都有独立 UUID。一个附件可在同一 thread 的多次 run 中复用，不能跨 thread 使用。删除 thread 时，业务层先逐层收集后代线程，再在单个事务内按依赖顺序显式删除全部关联元数据（数据库层不设外键与级联），随后删除整个 thread 目录。当前没有 list/delete API。

## 错误码

输入协议错误使用结构化 `ProtocolError`。主要错误码包括：

| Code | 含义 |
| --- | --- |
| `invalid_input_part` | part、项目路径或附件 ID 集合无效 |
| `invalid_json` | JSON 请求体缺失、语法错误或字段无效 |
| `invalid_path` | 路径参数无法解析 |
| `invalid_query` | 查询参数无法解析 |
| `invalid_websocket_upgrade` | WebSocket 握手参数无效 |
| `route_not_found` | 请求路径不存在 |
| `method_not_allowed` | 路径存在，但不支持请求使用的 HTTP 方法 |
| `skill_not_found` | 当前 registry 中不存在该 Skill |
| `skill_not_invocable` | Skill 不允许用户调用 |
| `subagent_not_found` | 当前 registry 中不存在该 Subagent |
| `attachment_not_found` | 附件不存在或不属于当前 thread |
| `unsupported_input_modality` | 当前模型不支持图片输入 |
| `invalid_attachment_upload` | multipart 形状无效 |
| `attachment_too_large` | 文件超过 20 MiB |
| `unsupported_attachment_type` | MIME 不受支持或与 magic bytes 不匹配 |
| `run_busy` | 线程已有运行在进行中，提交被拒绝且输入不保存（HTTP 409） |

PDF、DOCX、TXT 等文档附件及有序文档 part 尚未开放；未来扩展不需要改变 opaque attachment ID 和元数据映射模型。
