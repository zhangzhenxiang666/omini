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
| `model_tiers` | `ModelTiers` | ❌ | 抽象模型档位到 `provider + model` 的映射，供辅助任务使用 |

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

### `model_tiers` — 模型分级

后台/辅助任务（如未来标题生成、记忆抽取、复杂归纳）通常不需要和主线程使用同一个模型。通过 `model_tiers` 可以把抽象档位映射到任意已配置的 `provider + model`，让用户在保持主线程模型不变的情况下，为辅助任务选择更合适的模型。

Omini 内部使用 provider-neutral 档位名，不会硬编码任何 vendor（Haiku/Sonnet/Opus、mini/nano 等）。

| 档位 | 语义 |
|------|------|
| `small` | 轻量、快速、低成本；适合标题生成、简单抽取等高频辅助任务 |
| `standard` | 质量与成本平衡；适合常规摘要、记忆整理 |
| `large` | 高能力、复杂推理；适合困难归纳、冲突解决 |

每个档位子表包含：

| 字段 | 类型 | 必需 | 说明 |
|------|------|------|------|
| `provider` | `String` | ✅ | 已配置 provider 的 key（`providers.<key>`） |
| `model` | `String` | ✅ | 该 provider 下已声明的 model id |
| `thinking_effort` | `String` | ❌ | 思考力度，遵循与主线程相同的 `thinking` 模型归一化规则 |

完整示例（主线程使用 OpenAI，`small` 复用 Anthropic Haiku，`large` 升级到 Anthropic Opus）：

```toml
[model_tiers.small]
provider = "anthropic"
model = "claude-haiku-4-5"
thinking_effort = "low"

[model_tiers.standard]
provider = "openai"
model = "gpt-5-mini"

[model_tiers.large]
provider = "anthropic"
model = "claude-opus-4-8"
thinking_effort = "high"
```

不同 tier 可以指向不同 provider，由用户自行决定。

#### Fallback 行为

通过 `Settings::resolve_tier(tier)` 解析档位时，命中下列任一条件会 fallback 到当前线程活跃 `(provider, model, thinking_effort)`，并通过 `tracing::warn` 记录原因：

| 触发条件 | `reason` 字段 |
|----------|---------------|
| 整个 `model_tiers` 块缺失或该 slot 未配置 | `tier_not_configured` |
| tier.provider 不在当前 `providers` 表里 | `tier_provider_missing` |
| tier.model 不在该 provider 的 `models` 列表里 | `tier_model_missing` |
| 目标 model 不支持 thinking（`thinking = false`），但 tier 配置了 `thinking_effort` | 归一化为 `None`，不触发 fallback |

失效的 tier 配置是**软失效**：不会阻止 `config.toml` 加载，也不会影响主对话模型选择。未来消费方应统一通过 `resolve_tier` 入口获取结果，避免直接读取配置表。

#### 隐私与可观测性提示

辅助任务可能使用与主线程不同的 provider，对应的请求内容（标题生成会包含首条用户消息、记忆抽取可能包含线程片段）会被发送到该 provider 的服务。配置时需注意这一隐私边界。后续消费方应在日志中记录实际使用的 provider/model 与 fallback 原因，便于追踪成本、延迟与质量问题。

`model_tiers.small` 缺失时会 fallback 到当前主线程模型，可能产生与主对话相同的成本。推荐显式配置 `model_tiers.small` 为低成本模型（例如 `gpt-4o-mini` / `claude-haiku-4-5`），把首条消息触发的后台标题生成开销控制在辅助层。

> **状态**：标题生成已通过 #39 接入，默认消费 `model_tiers.small`。其他消费方（记忆系统、复杂归纳等）由后续 issue 单独接入。

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
| `model_tiers` | 子表整体替换：项目级出现的 `small` / `standard` / `large` slot 会替换对应用户级 slot，未出现的 slot 保留用户级值 |

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

# 项目级覆盖 small 档位，standard/large 保留用户级配置
[model_tiers.small]
provider = "openai"
model = "project-finetuned-v2"
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
