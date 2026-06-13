# 主配置文件 (config.toml)

Omini 的主配置文件使用 TOML 格式，支持 **用户级** 和 **项目级** 两层配置，项目级配置会与用户级配置自动合并。

## 配置文件路径

| 层级 | 路径 | 说明 |
|------|------|------|
| 用户级 | `~/.omini/config.toml` | 全局配置，对所有项目生效 |
| 项目级 | `<project>/.omini/config.toml` | 项目级配置，仅对当前工作目录生效，可选 |

启动时，Omini 先加载用户级配置，再加载当前工作目录下的项目级配置（如果存在），并将二者合并。合并规则见 [项目级配置](#项目级配置)。

## 最小可用配置

```toml
# ~/.omini/config.toml

[providers.openai]
endpoint = "openai"
base_url = "https://api.openai.com/v1"
api_key = "sk-..."

[providers.openai.models."gpt-4o"]
```

> **注意**：模型 ID 如果包含点号（如 `gpt-4.1`），必须使用 TOML 引号键，例如 `[providers.openai.models."gpt-4.1"]`，否则 TOML 会将其解析为嵌套表导致解析错误。

## 完整配置示例

```toml
# ~/.omini/config.toml

# 语言偏好（影响系统提示中的语言指令）
language = "简体中文"

# ── Provider 配置 ──────────────────────────────────────────

[providers.openai]
name = "OpenAI"                  # 可选，默认使用 provider key
endpoint = "openai"
base_url = "https://api.openai.com/v1"
api_key = "sk-..."

[providers.openai.models."gpt-4o"]
name = "GPT-4o"
limit = 128000                   # 上下文窗口大小（token），默认 256000
thinking = false
input_modalities = ["text", "image"]

[providers.openai.models."o3-mini"]
name = "o3-mini"
limit = 200000
thinking = true                  # 标记为支持 thinking 的模型

# ── Anthropic Provider ─────────────────────────────────────

[providers.anthropic]
endpoint = "anthropic"
base_url = "https://api.anthropic.com"
api_key = "sk-ant-..."

[providers.anthropic.models."claude-sonnet-4-20250514"]
name = "Claude Sonnet 4"
limit = 200000
thinking = true

# ── 自动压缩 ────────────────────────────────────────────────

[compact]
enabled = true                 # 启用对话自动压缩
preserve_recent = 6            # 压缩时保留最近的对话轮数
buffer_tokens = 13000          # 触发压缩的 token 缓冲阈值
summary_output_tokens = 20000  # 压缩摘要的最大输出 token 数
max_consecutive_failures = 3   # 压缩连续失败的最大次数

# ── MCP Servers ─────────────────────────────────────────────

# 通过 stdio 启动的本地 MCP server
[mcp_servers.docs]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-docs"]
enabled = true
startup_timeout_sec = 10.0
tool_timeout_sec = 30.0
enabled_tools = ["search", "read"]

# 通过 Streamable HTTP 连接的远程 MCP server
[mcp_servers.remote-search]
url = "https://mcp.example.com/search"
bearer_token_env_var = "MCP_TOKEN"   # 从环境变量读取 Bearer Token
http_headers = { "X-Custom" = "value" }
```

## 配置字段参考

### 顶层字段

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `providers` | `HashMap<String, ProviderConfig>` | ✅ | LLM 提供商配置，至少一个 |
| `language` | `String` | ❌ | 语言偏好，用于系统提示中的语言指令 |
| `compact` | `CompactConfig` | ❌ | 对话自动压缩配置 |
| `mcp_servers` | `HashMap<String, McpServerConfig>` | ❌ | MCP server 配置 |

> **注意**：权限配置（`permissions`）已移至 [权限配置](permissions.md) 文档。

### `providers` — 提供商配置

每个 provider 以 key 作为唯一标识（如 `openai`、`anthropic`）。

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `name` | `String` | ❌ | 显示名称，默认使用 provider key |
| `endpoint` | `"openai"` \| `"anthropic"` | ✅ | API 协议类型 |
| `base_url` | `String` | ✅ | API 基础 URL |
| `api_key` | `String` | ✅ | API 密钥 |
| `models` | `HashMap<String, ModelEntry>` | ❌ | 该 provider 下可用的模型列表 |

### `models` — 模型配置

每个 model 以模型 ID 作为 key。

| 字段 | 类型 | 必需 | 默认值 | 说明 |
|------|------|------|--------|------|
| `name` | `String` | ❌ | 模型 ID | 显示名称 |
| `limit` | `u32` | ❌ | `256000` | 上下文窗口大小（token） |
| `thinking` | `bool` | ❌ | `false` | 是否支持 thinking（扩展推理） |
| `input_modalities` | `Vec<String>` | ❌ | — | 支持的输入类型：`"text"` \| `"image"` |
| `headers` | `HashMap<String, String>` | ❌ | — | 该模型专属的额外 HTTP 请求头 |
| `body` | `HashMap<String, Value>` | ❌ | — | 该模型专属的额外请求体字段 |

#### 模型专属 headers 和 body

某些 API 兼容服务要求特定模型携带额外的 HTTP header 或请求体参数。可以通过 `headers` 和 `body` 字段为单个模型配置：

```toml
[providers.example.models."some-model"]
name = "Some Model"
limit = 128000
thinking = false

[providers.example.models."some-model".headers]
"x-provider-feature" = "enabled"

[providers.example.models."some-model".body]
some_option = true
routing_mode = "fast"
```

**注意事项：**

- 未配置 `headers` / `body` 时，现有行为保持不变。
- 仅当选中该模型时，请求才会附带这些额外参数；同 provider 下的其他模型不受影响。
- extra headers 会覆盖默认 headers（如 `Authorization`），extra body 字段会覆盖请求体中的同名字段（如 `messages`、`model`）。当前未对此进行拦截，请谨慎配置。

### Thinking（扩展推理）

对于 `thinking = true` 的模型，可在 TUI 中通过 `/thinking` 命令设置思考力度，可选值：

| 值 | 说明 |
|----|------|
| `none` | 不使用 thinking |
| `low` | 低思考量 |
| `medium` | 中等思考量（默认） |
| `high` | 高思考量 |
| `xhigh` | 极高思考量 |
| `max` | 最大思考量 |

切换到不支持 thinking 的模型时，思考力度会自动清除。

### `language` — 语言偏好

设置后会影响系统提示中的语言指令，使 Agent 倾向于使用指定语言回复。值可以是任意语言名称字符串，例如 `"简体中文"`、`"English"`、`"日本語"`。

### `compact` — 自动压缩

当对话接近模型上下文限制时，Omini 会自动压缩早期对话以腾出空间。

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `enabled` | `bool` | `true` | 是否启用自动压缩 |
| `preserve_recent` | `usize` | `6` | 压缩时保留最近多少轮对话不被压缩 |
| `buffer_tokens` | `usize` | `13000` | 距离上下文限制还剩多少 token 时触发压缩 |
| `summary_output_tokens` | `usize` | `20000` | 压缩生成的摘要最大 token 数 |
| `max_consecutive_failures` | `usize` | `3` | 压缩连续失败的最大次数，超过后停止尝试 |

### `mcp_servers` — MCP Server 配置

每个 MCP server 以 key 作为唯一标识。支持两种传输方式：**stdio**（本地进程）和 **streamable HTTP**（远程服务）。

**通用字段：**

| 字段 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| `enabled` | `bool` | `true` | 是否启用该 server |
| `startup_timeout_sec` | `f64` | — | 启动超时时间（秒） |
| `tool_timeout_sec` | `f64` | — | 工具调用超时时间（秒） |
| `enabled_tools` | `Vec<String>` | — | 仅启用指定的工具（白名单） |
| `disabled_tools` | `Vec<String>` | — | 禁用指定的工具（黑名单） |

**stdio 传输（本地进程）：**

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `command` | `String` | ✅ | 启动命令 |
| `args` | `Vec<String>` | ❌ | 命令参数 |
| `env` | `HashMap<String, String>` | ❌ | 环境变量 |
| `cwd` | `String` | ❌ | 工作目录 |

```toml
[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/user/projects"]
startup_timeout_sec = 15.0
```

**streamable HTTP 传输（远程服务）：**

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `url` | `String` | ✅ | 远程 MCP server URL |
| `bearer_token_env_var` | `String` | ❌ | Bearer Token 所使用的环境变量名 |
| `http_headers` | `HashMap<String, String>` | ❌ | 额外的 HTTP 请求头 |

```toml
[mcp_servers.web-search]
url = "https://mcp.example.com/search"
bearer_token_env_var = "SEARCH_API_TOKEN"
```

> **注意**：`command` 和 `url` 必须二选一，不能同时设置。同时设置会导致配置解析错误。此外，stdio 类型不支持 `bearer_token_env_var` 和 `http_headers`，HTTP 类型不支持 `args`、`env`、`cwd`。

## 项目级配置

项目级配置文件位于 `<project>/.omini/config.toml`，用于为特定项目定制配置。

### 合并规则

项目级配置会覆盖用户级配置，合并规则如下：

| 配置区域 | 合并策略 |
|----------|----------|
| `language` | 项目级直接覆盖 |
| `providers` | 按 provider key 合并：同名 provider 按字段覆盖，模型按 model ID 合并 |
| `compact` | 按字段覆盖，未设置的字段保留用户级值 |
| `mcp_servers` | 同名 server 整体替换（不做字段级合并） |

> **注意**：权限配置的合并规则详见 [权限配置](permissions.md)。

### 项目级新增 Provider

如果项目级配置要新增一个用户级配置中不存在的 provider，必须提供所有必需字段（`endpoint`、`base_url`、`api_key`），否则启动会报错。

### 项目级配置示例

```toml
# <project>/.omini/config.toml

# 项目专属语言设置
language = "English"

# 覆盖用户级的 base_url（如使用项目级代理）
[providers.openai]
base_url = "https://project-proxy.example.com/v1"

# 新增项目专属模型
[providers.openai.models."project-finetuned-v2"]
name = "Project Finetuned V2"
limit = 64000

# 为该项目禁用压缩
[compact]
enabled = false

# 项目专属 MCP server
[mcp_servers.project-db]
command = "docker"
args = ["run", "-i", "--rm", "mcp/postgres", "postgresql://localhost/projectdb"]
```

## 校验规则

- 必须至少配置一个 provider。
- 每个 provider 必须至少有一个 model。
- MCP server 的 `command` 和 `url` 必须二选一。
- 项目级新增 provider 时必须提供 `endpoint`、`base_url`、`api_key`。

## 相关文档

- [权限配置](permissions.md) — `[permissions]` 段、`permissions.toml`、`rules/*.rules` DSL 语法
- [Agent 指令](instructions.md) — `AGENTS.md` 文件的用途与格式
- [Skills 配置](skills.md) — 可复用技能包的创建与管理
- [返回文档索引](index.md)
