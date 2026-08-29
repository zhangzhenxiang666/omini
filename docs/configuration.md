# 主配置文件（config.toml）

Omini 使用新的、破坏性的配置 schema：没有 `version` 字段，也不支持旧字段名或旧配置布局。

配置有两层：`~/.omini/config.toml` 是全局配置，`<project>/.omini/config.toml` 是可选的项目覆盖层。两者使用同一 schema；项目层只需写要覆盖的字段。

## 最小可运行配置

至少配置一个 provider 和一个 model：

```toml
[providers.openai]
protocol = "openai"
base_url = "https://api.openai.com/v1"
api_key = { env = "OPENAI_API_KEY" }

[providers.openai.models."gpt-5"]
```

`name`、`context_window`、`thinking`、`input`、routing、MCP 和权限均为可选配置。

如果用户级配置缺失，或某个 provider 尚未配置 model，omini 会把对应项目标为
“需要配置”，并在 TUI 中提供服务端驱动的首次引导。配置 TOML 语法错误、provider
缺少 `protocol`/`base_url` 或凭据无法解析时则显示只读诊断，需手动修复文件。
引导表单使用 `Tab`/上下键切换字段；文本字段支持左右键、`Home`、`End`、
`Backspace`、`Delete` 和在当前光标处粘贴，protocol 字段使用左右键切换类型。

## 认证存储（auth.json）

首次引导保存的 API key 不会写入 `config.toml`，而是保存到权限为 `0600` 的
`~/.omini/auth.json`，并由配置引用环境变量名：

```json
{
  "env": {
    "OPENAI_API_KEY": "sk-..."
  }
}
```

```toml
[providers.openai]
api_key = { env = "OPENAI_API_KEY" }
```

daemon 每次打开项目或创建 runtime 都读取最新 `auth.json`；启动 omini-server 的真实
环境变量优先于该文件。`auth.json` 目前只为模型 provider 提供凭据，不会注入 MCP 或其
子进程。

## 内置工具状态

`rg` 是 omini-server 管理的内置依赖，而非 TUI/CLI 的依赖。安装器负责首次放置它；
server 启动时会异步校验并在缺失时下载与自身版本匹配的 Release 资产。`GET /v1/health`
中的 `bundled_rg.state` 为 `ready`、`restoring` 或 `unavailable`，所有客户端（包括未来的
Web）都应消费此状态。daemon 即使 `unavailable` 仍可用于配置和项目管理，但提交新的
agent run 会返回 `bundled_tool_unavailable`，直到恢复成功。

## 完整配置示例

下面的示例覆盖当前 `config.toml` 支持的所有配置段和字段。示例中的值仅用于说明；特别是 request 覆盖和 MCP command 应按实际 provider 与本机环境调整。

<!-- config-example:full:start -->

```toml
[agent]
language = "简体中文"

[providers.openai]
name = "OpenAI"
protocol = "openai"
base_url = "https://api.openai.com/v1"
api_key = { env = "OPENAI_API_KEY" }

[providers.openai.request.headers]
"X-Client" = "omini"

[providers.openai.request.body]
metadata = { source = "omini" }

[providers.openai.models."gpt-5"]
name = "GPT-5"
context_window = 400000
thinking = true
input = ["text", "image"]

[providers.openai.models."gpt-5".request.headers]
"X-Model-Mode" = "reasoning"

[providers.openai.models."gpt-5".request.body]
temperature = 0.2

[providers.openai.models."gpt-5-mini"]
name = "GPT-5 Mini"
context_window = 200000
thinking = true
input = ["text"]

[routing]
small = { provider = "openai", model = "gpt-5-mini", thinking_effort = "low" }
standard = { provider = "openai", model = "gpt-5", thinking_effort = "medium" }
large = { provider = "openai", model = "gpt-5", thinking_effort = "high" }

[context.compaction]
enabled = true
preserve_recent = 6
buffer_tokens = 13000
summary_output_tokens = 20000
max_consecutive_failures = 3

# stdio MCP server
[mcp.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "."]
env = { LOG_LEVEL = "warn" }
cwd = "/path/to/project"
enabled = true
startup_timeout_sec = 30.0
tool_timeout_sec = 60.0
enabled_tools = ["read_file", "list_directory"]
disabled_tools = ["write_file"]

# streamable HTTP MCP server；不能同时设置 command。
[mcp.docs]
url = "https://mcp.example.com/mcp"
bearer_token_env_var = "DOCS_MCP_TOKEN"
http_headers = { "X-Client" = "omini" }
enabled = true
startup_timeout_sec = 30.0
tool_timeout_sec = 60.0

[permissions]
allow = ["read", "search"]
ask = ["write"]
deny = ["read(.env)"]
```

<!-- config-example:full:end -->

## 字段参考

### Agent 与上下文

| 段/字段 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `[agent].language` | 否 | 无 | Agent 使用的语言偏好。 |
| `[context.compaction].enabled` | 否 | `true` | 是否启用自动上下文压缩。 |
| `[context.compaction].preserve_recent` | 否 | `6` | 压缩时保留的最近消息数。 |
| `[context.compaction].buffer_tokens` | 否 | `13000` | 为压缩与后续生成预留的 token 数。 |
| `[context.compaction].summary_output_tokens` | 否 | `20000` | 压缩摘要的最大输出 token 数。 |
| `[context.compaction].max_consecutive_failures` | 否 | `3` | 连续压缩失败后停止自动重试的次数。 |

### Provider 与模型

`providers.<id>` 是连接配置，`providers.<id>.models.<id>` 是该 provider 可选择的模型目录。provider ID 与 `protocol` 独立：例如 OpenRouter 可以使用 `protocol = "openai"`。

| 段/字段 | 必填 | 默认值 | 说明 |
| --- | --- | --- | --- |
| `providers.<id>.name` | 否 | provider ID | 显示名。 |
| `providers.<id>.protocol` | 是 | — | 当前为 `"openai"` 或 `"anthropic"`。 |
| `providers.<id>.base_url` | 是 | — | 完整 API base URL。 |
| `providers.<id>.api_key` | 否 | 无 | 明文字符串，或 `{ env = "NAME" }`；指定 env 时环境变量必须存在。 |
| `models.<id>.name` | 否 | model ID | 显示名。 |
| `models.<id>.context_window` | 否 | `256000` | 必须大于零。 |
| `models.<id>.thinking` | 否 | `false` | 是否允许该模型选择推理强度；为真但未选择时默认 `medium`。 |
| `models.<id>.input` | 否 | 空列表 | 支持的输入类型，例如 `"text"`、`"image"`。 |

Provider 和模型都可设置 `[...request.headers]` 与 `[...request.body]`。同名 provider/model 在项目层合并；headers 按 key 覆盖，body 为浅层 key 覆盖，模型覆盖 provider。

这些固定覆盖会在每次请求中使用，且允许覆盖任意 header 或 body 字段，包括认证和协议字段。配置文件应只来自你信任的来源。

### Routing

`[routing]` 的 `small`、`standard`、`large` 都是 `{ provider, model, thinking_effort? }`。provider 和 model 必须引用已配置的目录；未设置的 slot 使用当前线程模型。

`thinking_effort` 可取 `none`、`low`、`medium`、`high`、`xhigh`、`max`。不支持 thinking 的模型会忽略该值。

### MCP

每个 `[mcp.<name>]` 必须二选一：

| Transport | 必填字段 | 可选字段 |
| --- | --- | --- |
| stdio | `command` | `args`、`env`、`cwd`、`enabled`、`startup_timeout_sec`、`tool_timeout_sec`、`enabled_tools`、`disabled_tools` |
| streamable HTTP | `url` | `bearer_token_env_var`、`http_headers`、`enabled`、`startup_timeout_sec`、`tool_timeout_sec`、`enabled_tools`、`disabled_tools` |

`command` 与 `url` 不能同时设置。`enabled` 默认 `true`；其余可选字段缺失时不设置专门值。项目层中同名 MCP server 会整体替换全局值，因此覆盖时需要写出完整的该 server 配置。

### 权限

`[permissions]` 包含 `allow`、`ask`、`deny` 三个可选规则列表。具体工具规则、`permissions.toml` 与 Bash `.rules` DSL 请参阅[权限配置](permissions.md)。

## 项目覆盖示例

项目配置只写变动部分。例如 `<project>/.omini/config.toml` 可以只替换一个模型的能力、request 覆盖和 routing：

```toml
[providers.openai.models."gpt-5"]
thinking = false

[providers.openai.models."gpt-5".request.body]
temperature = 0.5

[routing]
standard = { provider = "openai", model = "gpt-5-mini" }
```

项目层可覆盖 provider、模型、routing、context、MCP 与 permissions。目前它与全局配置有同等能力；不要把不信任仓库中的 `.omini/config.toml` 当作安全边界。项目配置的信任确认将在后续单独设计。
