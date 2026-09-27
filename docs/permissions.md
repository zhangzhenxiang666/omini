# 权限配置

Omini 支持多种方式来配置工具权限和 Bash 命令规则，实现精细化的权限控制。

## 权限配置来源

| 来源 | 路径 | 说明 |
| ------ | ------ | ------ |
| 主配置段 | `~/.omini/config.toml` 的 `[permissions]` 段 | 全局工具权限规则 |
| 项目权限文件 | `<project>/.omini/permissions.toml` | 项目级权限规则 |
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
| `tool` | 对工具的所有操作生效 | `"read"`、`"search"` |
| `tool(path)` | 对特定路径的操作生效 | `"read(./src/**)"`、`"write(/etc/**)"` |

**支持路径限定的工具：**

- `read`、`view_image` — 读取文件
- `search` — 搜索文件
- `edit`、`write` — 编辑/写入文件
- `Agent` — 同时匹配 `spawn_agent` 与 `run_agent`（按 `name` 匹配，例如 `Agent(explorer)`）

**路径语法：**

| 前缀 | 说明 | 示例 |
| ------ | ------ | ------ |
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

`permissions.toml` 是项目级权限配置文件，位于 `<project>/.omini/permissions.toml`。

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

Bash 使用黑名单审批模式：**未命中规则或风险名单的命令默认放行**。未知 CLI、构建、测试、普通包安装、`uv run`、脚本文件和普通下载均无需确认。

决策顺序如下：

1. 内置硬拒绝检查，不可覆盖。
2. 匹配用户和项目 `.rules`，取 `forbidden > prompt > allow`，与文件加载或规则声明顺序无关。
3. 无显式规则匹配时，检查内置风险询问名单。
4. 无匹配时放行。

显式 `allow` 可以覆盖内置风险询问，但不能覆盖用户 `prompt`、`forbidden` 或内置硬拒绝。复合命令的每条可见命令分别检查，整次 Bash 调用取最严格结果；不会先执行其中已经允许的部分。外层包装的 `allow` 不取消内层命令的独立检查。已有 `.rules` 无需迁移，原有显式 `prompt` 继续生效。

### 命令分析与执行边界

命令通过 Bash AST 分析，检查管道、条件、循环、后台分隔符 `&`、函数体、命令替换和子 shell 中的可见命令。循环和条件控制字本身不触发确认。字面量 `sh/bash/zsh -c/-lc`、`eval`、`exec`、`trap`、`env`、`command` 及前置环境赋值中的执行命令也会展开检查；包装展开最多 8 层，解析失败或超限时需要确认。`env -S/--split-string` 的静态内容使用独立分词语法，本期无法完整分析时需要确认。

内置风险检查按可执行文件 basename 识别，所以 `/bin/rm` 也会询问。用户前缀规则保持原始可执行路径和参数匹配：`["rm"]` 不自动匹配 `/bin/rm`，需另外配置对应路径。内置子命令检查识别 `git -C`、`jj -R` 等全局选项；用户规则仍匹配完整 argv。动态参数和空字符串保留参数位置，不会因省略它们而错误匹配后续参数。引号、注释与 heredoc 的普通正文不作为执行命令，但其中实际生效的命令替换会检查。

Omini 的 Bash **没有执行沙箱**。审批只分析输入中可见的命令，不检查 `sh script.sh`、`uv run script.py` 等脚本文件的内容，不推断 `$cmd`、动态 `eval` 或解释器代码的实际行为。这些形式默认放行，其中可见的嵌套命令仍按规则检查。黑名单不提供文件系统或网络隔离，Bash 也不继承 `read/write` 工具的路径规则。

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
| ------ | ------ | ------ | -------- | ------ |
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

**显式放行某类内置询问：**

```bash
# ~/.omini/rules/local-cleanup.rules
# 放行作用域明确的清理命令，根目录或家目录硬拒绝仍生效。
prefix_rule(
  pattern = ["rm", "-rf", "/tmp/my-build-cache"],
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

### 内置 Bash 风险策略

以下命令默认询问；审批抽屉会显示触发原因。用户显式 `allow` 可覆盖这些询问。

| 命令模式 | 默认行为 |
| ---------- | ---------- |
| `rm`、`rmdir`、`unlink`、`shred`、`truncate`、`dd` | 询问删除或数据破坏 |
| `kill`、`pkill`、`killall` | 询问进程终止 |
| `ssh`、`scp`、`rsync` | 询问远程访问或同步 |
| `chmod`、`chown`、`chgrp` | 询问权限或所有权变更 |
| `git push/reset/clean/restore`、`git branch -D` | 询问远端写入或破坏性版本控制操作 |
| `jj git push`、`jj abandon/restore/undo`、`jj operation restore`（含 `op` 别名） | 询问远端写入或历史修改 |
| `jj commit/describe/new/squash/split/absorb/metaedit`（含 `ci/desc` 别名） | 询问提交创建、描述或内容整理 |
| `gh` 的 Issue、PR、Release、Label、Repo 写操作及工作流触发、取消、重跑 | 询问远端修改 |
| `diesel/sqlx/prisma/sea-orm-cli migrate/migration` | 询问数据库迁移 |
| `docker rm`、`docker system prune` | 询问资源删除或清理 |
| `sudo`、`su`、`doas`、`systemctl`、`launchctl` | 询问提权、用户切换或系统服务管理 |
| `fdisk`、`parted`、`sfdisk`、`gdisk`、`sgdisk`、`wipefs`、`mkfs`、`mkfs.*` | 询问磁盘分区或格式化 |

普通 `git commit/pull/merge/rebase/checkout/switch`、`docker run`、包管理器和下载命令默认放行。自定义 CLI 别名和脚本内容不展开推断，可通过 `.rules` 自行增加询问或拒绝。

以下三类保留不可覆盖的硬拒绝，不弹出可批准的询问：

- 递归强制删除根目录、等价根路径或家目录，例如 `rm -rf /`、`rm -rf /tmp/..`、`rm -rf ~`、`rm -rf "$HOME"`。
- fork bomb，例如 `:(){ :|:& };:`，包括字面量 shell 包装中的该模式。
- 下载后直接执行，例如 `curl ... | sh`、`wget ...; bash ...`。沿用保守检查：同次分析的可见命令序列中，下载之后出现 shell 执行即拒绝，不追踪下载文件的数据流。

询问时批准仅执行本次调用，不自动记住后续调用；拒绝或取消不会启动命令。

## 默认行为

如果没有配置任何权限规则，Omini 使用以下默认行为：

| 工具 | 默认决策 | 说明 |
| ------ | ---------- | ------ |
| `read`、`view_image` | 项目内/`/tmp` 内允许，其他需确认 | 读取项目内文件直接允许 |
| `search` | 允许 | 搜索操作直接允许 |
| `edit`、`write` | 需确认 | 写入操作需要用户确认 |
| `todo_write` | 允许 | 创建待办清单直接允许 |
| `ask_user`、`skill`、`spawn_agent`、`run_agent`、`read_task`、`cancel_task` | 允许 | 交互与 Agent task 工具直接允许 |
| `bash` | 默认放行，风险命令询问，极端命令拒绝 | 显式 `.rules` 优先于内置询问，解析失败或包装超限仍询问 |

## 相关文档

- [主配置文件](configuration.md) — `config.toml` 的其他配置项
- [Agent 指令](instructions.md) — `AGENTS.md` 文件的用途与格式
- [Skills 配置](skills.md) — 可复用技能包的创建与管理
- [返回文档索引](index.md)
