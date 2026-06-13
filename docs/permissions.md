# 权限配置

Omini 支持多种方式来配置工具权限和 Bash 命令规则，实现精细化的权限控制。

## 权限配置来源

| 来源 | 路径 | 说明 |
|------|------|------|
| 主配置段 | `~/.omini/config.toml` 的 `[permissions]` 段 | 全局工具权限规则 |
| 项目权限文件 | `<project>/.omini/permissions.toml` | 项目级权限规则（兼容入口） |
| 用户 Bash 规则 | `~/.omini/rules/*.rules` | 全局 Bash 命令规则 |
| 项目 Bash 规则 | `<project>/.omini/rules/*.rules` | 项目级 Bash 命令规则 |

## 工具权限配置

### 配置格式

在 `config.toml` 中使用 `[permissions]` 段：

```toml
[permissions]
allow = [
  "read",                    # 允许读取任意文件
  "read(./src/**)",          # 允许读取 src 目录下的文件
  "search",                  # 允许搜索
  "todo_write",              # 允许创建待办清单
]
ask = [
  "write",                   # 写入文件前需要确认
  "edit",                    # 编辑文件前需要确认
]
deny = [
  "read(~/.ssh/**)",         # 禁止读取 SSH 密钥
  "write(~/.bashrc)",        # 禁止修改 bashrc
]
```

### 规则语法

每个规则可以是简单的工具名，也可以带路径限定：

| 格式 | 说明 | 示例 |
|------|------|------|
| `tool` | 对工具的所有操作生效 | `"read"`、`"bash"` |
| `tool(path)` | 对特定路径的操作生效 | `"read(./src/**)"`、`"write(/etc/**)"` |

**支持路径限定的工具：**

- `read`、`view_image` — 读取文件
- `search` — 搜索文件
- `edit`、`write` — 编辑/写入文件
- `subagent` — 子 Agent（按 name 匹配）

**路径语法：**

| 前缀 | 说明 | 示例 |
|------|------|------|
| `./` | 相对于项目根目录 | `"read(./src/**)"` |
| `/` | 绝对路径 | `"write(/etc/**)"` |
| `~/` | 相对于用户主目录 | `"read(~/.ssh/**)"` |
| `**` | 递归匹配子目录 | `"read(./**/*.rs)"` |

### 决策优先级

当多个规则匹配同一个操作时，遵循以下优先级：

```
deny > ask > allow
```

即：

- 如果有任何 `deny` 规则匹配，操作被拒绝
- 如果没有 `deny` 但有 `ask` 规则匹配，需要用户确认
- 如果只有 `allow` 规则匹配，操作直接执行

## permissions.toml 文件

`permissions.toml` 是项目级的权限配置兼容入口，位于 `<project>/.omini/permissions.toml`。

> **注意**：这个文件主要用于向后兼容。新项目应使用 `config.toml` 中的 `[permissions]` 段。

### 格式

```toml
# <project>/.omini/permissions.toml

allow = ["read", "search"]
ask = ["write"]
deny = ["read(.env)"]
```

字段说明与 `config.toml` 中的 `[permissions]` 段相同。

### 合并行为

当同时存在 `config.toml` 的 `[permissions]` 段和 `permissions.toml` 时：

- 两者作为独立来源加载
- 最终决策遵循 "更严格优先" 原则（deny > ask > allow）

## Bash 命令规则

Bash 命令的权限控制需要使用专门的 `.rules` 文件，不能使用 `[permissions]` 段中的规则（在 `[permissions]` 中写 `bash` 规则会被忽略并产生警告）。

### 文件路径

- 用户级：`~/.omini/rules/*.rules`
- 项目级：`<project>/.omini/rules/*.rules`

所有 `.rules` 文件按文件名排序后依次加载。

### 规则语法

`.rules` 文件使用自定义 DSL 语法，每条规则以 `prefix_rule()` 包裹：

```bash
# <project>/.omini/rules/default.rules

# 允许 git status 命令
prefix_rule(
  pattern = ["git", "status"],
  decision = "allow",
)

# 允许 cargo test 和 cargo check
prefix_rule(
  pattern = ["cargo", ["test", "check", "clippy"]],
  decision = "allow",
)

# 禁止 rm -rf 命令
prefix_rule(
  pattern = ["rm", "-rf"],
  decision = "forbidden",
  justification = "禁止使用 rm -rf 删除文件",
)

# 执行 docker run 前需要确认
prefix_rule(
  pattern = ["docker", "run"],
  decision = "prompt",
)
```

### 字段说明

| 字段 | 类型 | 必需 | 默认值 | 说明 |
|------|------|------|--------|------|
| `pattern` | `[[String]]` | ✅ | — | 命令前缀匹配模式 |
| `decision` | `String` | ❌ | `"allow"` | 决策：`"allow"` / `"prompt"` / `"forbidden"` |
| `justification` | `String` | ❌ | — | 拒绝时的原因说明 |

### Pattern 匹配

`pattern` 是一个二维数组，每个内层数组表示该位置可以接受的多个值：

```bash
# 简单模式：匹配 "git status"
prefix_rule(
  pattern = ["git", "status"],
  decision = "allow",
)

# 多选项模式：匹配 "git status" 或 "git s" 或 "git st"
prefix_rule(
  pattern = [["git"], ["status", "s", "st"]],
  decision = "allow",
)

# 匹配 "cargo test" 或 "cargo check" 或 "cargo clippy"
prefix_rule(
  pattern = ["cargo", ["test", "check", "clippy"]],
  decision = "allow",
)

# 匹配任意以 "npm" 开头的命令
prefix_rule(
  pattern = ["npm"],
  decision = "prompt",
)
```

**匹配规则：**

- `pattern` 中的每个元素按顺序匹配命令的每个参数
- 如果元素是字符串，必须精确匹配
- 如果元素是数组，匹配数组中的任意一个值
- `pattern` 的长度可以小于命令参数数量（前缀匹配）

### 更多示例

**允许常见的只读命令：**

```bash
# ~/.omini/rules/safe-commands.rules

prefix_rule(
  pattern = ["ls"],
  decision = "allow",
)

prefix_rule(
  pattern = ["cat"],
  decision = "allow",
)

prefix_rule(
  pattern = ["head"],
  decision = "allow",
)

prefix_rule(
  pattern = ["tail"],
  decision = "allow",
)

prefix_rule(
  pattern = ["wc"],
  decision = "allow",
)

prefix_rule(
  pattern = ["grep"],
  decision = "allow",
)
```

**禁止危险操作：**

```bash
# ~/.omini/rules/dangerous.rules

prefix_rule(
  pattern = ["rm", "-rf", "/"],
  decision = "forbidden",
  justification = "禁止删除根目录",
)

prefix_rule(
  pattern = ["rm", "-rf", "~"],
  decision = "forbidden",
  justification = "禁止删除主目录",
)

prefix_rule(
  pattern = ["mkfs"],
  decision = "forbidden",
  justification = "禁止格式化磁盘",
)

prefix_rule(
  pattern = ["dd"],
  decision = "forbidden",
  justification = "禁止使用 dd 命令",
)
```

**项目特定的规则：**

```bash
# <project>/.omini/rules/project.rules

# 允许运行项目的测试套件
prefix_rule(
  pattern = ["cargo", "test"],
  decision = "allow",
)

# 允许运行项目的构建
prefix_rule(
  pattern = ["cargo", "build"],
  decision = "allow",
)

# 允许运行项目特定的脚本
prefix_rule(
  pattern = ["./scripts/test.sh"],
  decision = "allow",
)

# 禁止直接操作生产数据库
prefix_rule(
  pattern = ["psql", "-h", "prod-db"],
  decision = "forbidden",
  justification = "禁止直接连接生产数据库",
)
```

### 内置 Bash 安全策略

除了用户配置的规则外，Omini 还有内置的 Bash 安全策略：

| 命令模式 | 默认决策 | 说明 |
|----------|----------|------|
| `sudo ...` | `prompt` | 需要确认 |
| `su ...` | `prompt` | 需要确认 |
| `chmod 777 ...` | `prompt` | 需要确认 |
| `curl ... \| sh` | `forbidden` | 禁止管道执行 |
| `wget ... \| sh` | `forbidden` | 禁止管道执行 |

这些内置策略无法通过配置覆盖。

## 默认行为

如果没有配置任何权限规则，Omini 使用以下默认行为：

| 工具 | 默认决策 | 说明 |
|------|----------|------|
| `read`、`view_image` | 项目内/`/tmp` 内允许，其他需确认 | 读取项目内文件直接允许 |
| `search` | 允许 | 搜索操作直接允许 |
| `edit`、`write` | 需确认 | 写入操作需要用户确认 |
| `todo_write` | 允许 | 创建待办清单直接允许 |
| `ask_user`、`skill`、`subagent` | 允许 | 交互类工具直接允许 |
| `bash` | 按规则判断 | 根据 Bash 规则和内置策略决定 |

## 相关文档

- [主配置文件](configuration.md) — `config.toml` 的其他配置项
- [Agent 指令](instructions.md) — `AGENTS.md` 文件的用途与格式
- [Skills 配置](skills.md) — 可复用技能包的创建与管理
- [返回文档索引](index.md)
