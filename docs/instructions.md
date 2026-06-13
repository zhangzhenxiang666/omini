# Agent Instructions (AGENTS.md)

`AGENTS.md` 文件用于给 Agent 提供行为指令，会被注入到系统提示中。

## 文件路径

| 层级 | 路径 | 说明 |
|------|------|------|
| 用户级 | `~/.omini/AGENTS.md` | 全局指令，对所有项目生效 |
| 项目级 | `<project>/AGENTS.md` | 项目级指令，仅对当前项目生效 |

> **注意**：项目级 `AGENTS.md` 位于项目**根目录**，而不是 `.omini/` 目录下。

## 优先级

当同时存在用户级和项目级 `AGENTS.md` 时：

- **项目级优先**：项目级指令会覆盖用户级指令
- Agent 会被告知两个文件的来源路径，并在冲突时优先遵循项目级指令
- 如果项目级指令与用户的最新请求冲突，Agent 会在执行前解释冲突

## 文件格式

`AGENTS.md` 是纯 Markdown 文件，内容直接作为 Agent 的行为指令。没有特殊的元数据或 frontmatter，直接写 Markdown 即可。

## 常见用途

- **项目规范**：定义编码规范、工作流要求
- **工作流约束**：限制 Agent 的工作方式（如修改代码前先阅读、提交前运行检查等）
- **项目上下文**：提供架构说明、关键模块、开发环境等信息
- **全局偏好**：用户级的语言、风格、工具偏好

## 示例

```markdown
# 项目规范

## 代码风格

- 遵循 Rust 惯用写法
- 所有 public API 必须有文档注释
- 使用 `cargo fmt` 格式化代码
- 使用 `cargo clippy` 检查代码质量

## 错误处理

- 禁止使用 `unwrap()`，必须处理错误
- 使用 `?` 运算符传播错误

## 工作流

### 修改代码前

- 先阅读相关代码，理解现有实现
- 不要修改无关的代码

### 提交前

- 运行 `cargo fmt --all`
- 运行 `cargo clippy --workspace -- -D warnings`
- 运行 `cargo test --workspace`
```

## 与其他配置的交互

- **权限规则**：`AGENTS.md` 中的工作流约束不会覆盖权限配置，两者独立生效
- **Skills**：`AGENTS.md` 可以提到使用特定 skills，但 skill 的调用仍需通过 `/skill` 命令
- **配置合并**：项目级 `AGENTS.md` 完全覆盖用户级，不会合并内容

## 相关文档

- [主配置文件](configuration.md) — `config.toml` 的其他配置项
- [权限配置](permissions.md) — 工具和 Bash 命令的权限控制
- [Skills 配置](skills.md) — 可复用技能包的创建与管理
- [返回文档索引](index.md)
