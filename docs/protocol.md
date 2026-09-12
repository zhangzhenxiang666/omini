# Client / Server Protocol

Omini 的公开 client/server 协议位于 `/v1`，当前 `protocol_revision` 为 `2`。这是一次破坏性版本：旧的 `text + context_refs + attachments/local_path` 请求不会被兼容解析。客户端必须在连接前检查 `GET /v1/health` 返回的 `protocol_revision`。

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

附件元数据记录在 SQLite `attachment` 表：`id`、`thread_id`、`original_name`、`mime_type`、`size`、`sha256`、`relative_path`、`created_at`。内容存放在对应 thread 的 `assets/<sha256>.<ext>`；相同内容可以共享文件，但每次上传都有独立 UUID。一个附件可在同一 thread 的多次 run 中复用，不能跨 thread 使用。删除 thread 会通过外键级联删除元数据并删除整个 thread 目录。当前没有 list/delete API。

## 错误码

输入协议错误使用结构化 `ProtocolError`。主要错误码包括：

| Code | 含义 |
| --- | --- |
| `invalid_input_part` | part、项目路径或附件 ID 集合无效 |
| `skill_not_found` | 当前 registry 中不存在该 Skill |
| `skill_not_invocable` | Skill 不允许用户调用 |
| `subagent_not_found` | 当前 registry 中不存在该 Subagent |
| `attachment_not_found` | 附件不存在或不属于当前 thread |
| `unsupported_input_modality` | 当前模型不支持图片输入 |
| `invalid_attachment_upload` | multipart 形状无效 |
| `attachment_too_large` | 文件超过 20 MiB |
| `unsupported_attachment_type` | MIME 不受支持或与 magic bytes 不匹配 |

PDF、DOCX、TXT 等文档附件及有序文档 part 尚未开放；未来扩展不需要改变 opaque attachment ID 和元数据映射模型。
