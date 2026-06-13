<div align="center">

# omini

**一个用于探索 Vibe Coding Agent 实现方式的学习项目**

</div>

## 关于

`omini` 是一个个人学习项目，旨在深入探索 OpenCode、Claude Code这类 **Vibe Coding Agent**
背后的实现原理与核心机制。

通过这个项目的构建过程，将逐步理解：

- Tool Calling 与 Agent Loop 的设计与编排
- Codebase 上下文的理解与检索（索引、搜索、代码分析）
- 文件编辑与代码生成的最佳实践
- 多客户端交互与 Vibe Coding 体验的搭建

> ⚠️ **注意**: 这是一个学习性质的项目，并非生产级工具。

## 功能预览

### 主界面

![欢迎界面](assets/welcome.png)

### 计划模式

支持在执行前进行任务规划，先展示计划再执行：

![计划模式](assets/plan.png)

### 权限管理

内置权限系统，支持工具调用和 Bash 命令的细粒度权限控制：

![权限管理](assets/permissions.png)

### 上下文压缩

自动/手动压缩历史对话，保持上下文窗口在限制范围内：

![上下文压缩](assets/compact.png)

## 构建

```bash
cargo build -p omini -p omini-server
```

## 安装布局

发布包固定包含：

```text
bin/omini
bin/omini-server
bin/rg
```

未来安装流程会把用户命令 `omini` 安装到 `~/.local/bin/omini`，把内部依赖
`omini-server` 和 `rg` 安装到 `~/.omini/bin/`。运行时 `omini` 从 `OMINI_SERVER_BIN`
或 `~/.omini/bin/omini-server` 启动 daemon，search 工具固定调用 `~/.omini/bin/rg`，
不依赖用户 `PATH` 中的 `rg`。

## 配置

Omini 支持用户级和项目级两层配置：

- **用户级配置**：`~/.omini/config.toml`（全局生效）
- **项目级配置**：`<project>/.omini/config.toml`（可选，仅当前项目生效）

项目级配置会与用户级配置自动合并，支持为特定项目定制 provider、模型、MCP server 等。

详细配置说明请参考 [配置文档](docs/index.md)，包括：

- [配置参考](docs/configuration.md) — 主配置文件
- [权限配置](docs/permissions.md) — 权限规则和 Bash 规则 DSL
- [Agent 指令](docs/instructions.md) — AGENTS.md 文件格式
- [Skills 配置](docs/skills.md) — 可复用技能包

## 许可

MIT
