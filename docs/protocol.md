# 客户端协议

本文面向接入 Omini 本地服务的客户端开发者。公开接口位于 `/v1`，当前 `protocol_revision` 为 `9`。完整路径、请求类型和响应定义以 `GET /v1/openapi.json` 为准；调试构建还提供 `/docs`。

## 连接与响应

连接前检查 `GET /v1/health` 返回的 `protocol_revision`。通过 `POST /v1/clients` 注册客户端，该请求不定义请求体。

健康响应的 `bundled_rg.state` 可以是 `ready`、`restoring` 或 `unavailable`。未就绪时仍可进行配置和项目管理，但提交运行会返回 `bundled_tool_unavailable`。

| 操作 | 成功响应 |
| --- | --- |
| 注册客户端，创建项目或线程 | `201` |
| 提交异步运行 | `202`，返回 `run_id` |
| 无返回数据的命令 | `204`，空响应 |
| 查询与返回配置快照的接口 | `200` |

单线程状态接口直接返回状态对象。所有 HTTP 错误使用 `{ "code": "...", "message": "..." }`，包括 JSON、路径、查询参数和路由错误。

## 会话历史

线程快照中的 `HistoryItem.type` 区分 `user_input`、`assistant_message` 和 `system_event`。计划、压缩摘要、子 Agent 通知和工具结果通过对应的系统事件提供；工具结果使用 `tool_results` 记录。

`agent_tasks` 包含主线程的直接异步子 Agent，各任务的 `history` 包含初始输入、后续消息和工具结果。同步 `run_agent` 与嵌套任务不提供独立的子会话快照。

客户端应使用会话历史恢复对话；模型上下文与会话历史的用途和顺序不同。

## 提交输入

向 `POST /v1/projects/{project_id}/threads/{thread_id}/runs` 提交普通消息：

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

`parts` 支持 `text`、`skill`、`file`、`directory` 和 `subagent`，数组顺序有语义。`label` 是客户端显示用的标签，不可用于推断输入位置。文件与目录表示项目内的引用，不会自动内嵌文件内容。

`attachment_ids` 是无序集合，ID 必须非空且不得重复；图片在所有语义 parts 之后加入模型输入。请求只能携带附件 ID，不能传客户端本地图片路径。

同一线程同时只接受一个运行。已有运行时再次提交返回 HTTP `409` 与 `run_busy`，被拒绝的输入不会保存。运行中补充要求使用相同接口的 `intervene_message`：

```json
{
  "type": "intervene_message",
  "input": { "parts": [{ "type": "text", "text": "优先检查错误处理" }] }
}
```

## 子 Agent 消息

向运行中的直接异步子 Agent 提交输入：

```text
POST /v1/projects/{project_id}/threads/{thread_id}/runs/{run_id}/input
x-omini-client-id: <已注册并连接的客户端 ID>
```

这里的 `thread_id` 是主线程 ID，`run_id` 是子任务 ID。请求体为 `{ "input": <UserInput>, "client_echo_id": "..." }`，`client_echo_id` 必填。

输入受理后，通过 `agent_task_user_message_queued` 事件提供任务 ID、子线程 ID、原始历史记录、客户端 ID 和回显 ID。受理表示输入已排队，不保证模型立即处理。

同一客户端、回显 ID 和子任务的重复请求只投递一次；同键不同内容报错。终态任务或非直接子任务返回冲突错误；已经受理的相同来源请求可安全重试。

Agent 之间的消息使用 `agent_task_message_queued` 事件，子任务历史中的对应记录为 `system_event.agent_message`。取消、异常或服务重启导致排队消息未被处理时，任务结果的 `undelivered_messages` 和完成通知摘要会说明未投递数量。

## 命令与 Skill

`/help` 和 `/exit` 由客户端处理；压缩、重命名、模型选择等操作调用对应接口。未知 slash 输入作为普通文本提交。

`/init` 使用 `execute_command`，客户端去掉命令名后保留原始参数 parts：

```json
{
  "type": "execute_command",
  "command": { "type": "init" },
  "input": { "parts": [{ "type": "text", "text": " 额外关注测试规范" }] }
}
```

客户端无需生成初始化提示或下载 `SKILL.md` 正文。`/skill-name prompt` 编码为一个 `skill` part，再跟随原始参数 parts；服务端校验并展开技能。可调用技能摘要通过 `GET /v1/projects/{project_id}/threads/{thread_id}/skills` 获取，包含可选的 `argument_hint`。

## 项目引用校验

文件和目录必须使用项目内存在的规范相对路径，不能是绝对路径、包含 `..` 或通过符号链接逃逸项目目录。Subagent 和 Skill 必须属于当前线程可用的目录。输入、附件归属和模型图片能力全部通过校验后，运行请求才会被接受。

## 图片附件

上传接口：

```text
POST /v1/projects/{project_id}/threads/{thread_id}/attachments
Content-Type: multipart/form-data
x-omini-client-id: <controller ID>
```

multipart 必须且只能包含一个名为 `file` 的字段。单文件上限为 20 MiB，支持 PNG、JPEG、WebP 和 GIF；声明 MIME 必须与文件内容相符。成功响应为：

```json
{ "attachment_id": "97f4206d-83b8-4b19-86c6-8cc55936505a" }
```

读取接口：

```text
GET /v1/projects/{project_id}/threads/{thread_id}/attachments/{attachment_id}
```

上传要求当前 controller，读取不要求 controller。附件不存在或不属于指定 project/thread 时返回 `404`。读取响应包含原始二进制，以及 `Content-Type`、`Content-Length`、`Content-Disposition`、`ETag` 和 `X-Content-Type-Options: nosniff`。

附件 ID 是不透明 UUID，不暴露内容哈希或存储路径。附件可在同一 thread 的多次运行中复用，不能跨 thread 使用。删除线程会清除其附件；目前没有独立的附件 list/delete 接口，也不支持 PDF、DOCX 或 TXT 附件。

## 错误码

| Code | 含义 |
| --- | --- |
| `invalid_input_part` | part、项目路径或附件 ID 集合无效 |
| `invalid_json` | JSON 请求体缺失、语法错误或字段无效 |
| `invalid_path` | 路径参数无法解析 |
| `invalid_query` | 查询参数无法解析 |
| `invalid_websocket_upgrade` | WebSocket 握手参数无效 |
| `route_not_found` | 请求路径不存在 |
| `method_not_allowed` | 路径不支持该 HTTP 方法 |
| `skill_not_found` | 当前线程找不到该 Skill |
| `skill_not_invocable` | Skill 不允许用户调用 |
| `subagent_not_found` | 当前线程找不到该 Subagent |
| `attachment_not_found` | 附件不存在或不属于当前 thread |
| `unsupported_input_modality` | 当前模型不支持图片输入 |
| `invalid_attachment_upload` | multipart 字段无效 |
| `attachment_too_large` | 文件超过 20 MiB |
| `unsupported_attachment_type` | MIME 不受支持或与文件内容不符 |
| `run_busy` | 线程已有运行，输入未保存（HTTP 409） |
| `bundled_tool_unavailable` | 内置搜索工具未就绪，运行未启动 |

[返回文档索引](index.md)
