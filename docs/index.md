# Omini 配置文档

Omini 使用多种配置文件来控制行为，支持 **用户级** 和 **项目级** 两层配置。

## 配置文件概览

| 类型 | 用户级路径 | 项目级路径 | 文档 |
|------|-----------|-----------|------|
| 主配置 | `~/.omini/config.toml` | `<project>/.omini/config.toml` | [配置参考](configuration.md) |
| 权限配置 | — | `<project>/.omini/permissions.toml` | [权限配置](permissions.md) |
| Bash 规则 | `~/.omini/rules/*.rules` | `<project>/.omini/rules/*.rules` | [权限配置](permissions.md) |
| Agent 指令 | `~/.omini/AGENTS.md` | `<project>/AGENTS.md` | [Agent 指令](instructions.md) |
| Skills | `~/.omini/skills/*/SKILL.md` | `<project>/.omini/skills/*/SKILL.md` | [Skills 配置](skills.md) |

## 文档目录

### [配置参考](configuration.md)

主配置文件 `config.toml` 的详细说明，包括：

- 最小可用配置
- Provider 和 Model 配置
- 自动压缩配置
- MCP Server 配置
- 项目级配置合并规则

### [权限配置](permissions.md)

工具权限和 Bash 命令规则的配置，包括：

- `[permissions]` 段的格式和语法
- `permissions.toml` 项目权限配置
- `rules/*.rules` DSL 语法
- Pattern 匹配规则
- 大量实际示例

### [Agent 指令](instructions.md)

`AGENTS.md` 文件的用途与格式。

### [Skills 配置](skills.md)

可复用技能包的创建与管理。

## 快速开始

### 1. 创建主配置文件

```bash
mkdir -p ~/.omini
cat > ~/.omini/config.toml << 'EOF'
[providers.openai]
endpoint = "openai"
base_url = "https://api.openai.com/v1"
api_key = "sk-..."

[providers.openai.models."gpt-4o"]
EOF
```

### 2. 配置权限规则（可选）

```bash
cat > ~/.omini/rules/safe-commands.rules << 'EOF'
prefix_rule(
  pattern = ["git", "status"],
  decision = "allow",
)

prefix_rule(
  pattern = ["cargo", ["test", "check", "clippy"]],
  decision = "allow",
)
EOF
```

### 3. 设置项目级指令（可选）

```bash
cat > AGENTS.md << 'EOF'
# 项目规范

- 使用 `cargo fmt` 格式化代码
- 使用 `cargo clippy` 检查代码
- 禁止使用 `unwrap()`
EOF
```

### 4. 创建自定义 Skill（可选）

```bash
mkdir -p ~/.omini/skills/code-reviewer
cat > ~/.omini/skills/code-reviewer/SKILL.md << 'EOF'
---
name: code-reviewer
description: 审查代码变更，提供改进建议
---

# 代码审查流程

1. 查看变更文件列表
2. 检查每个文件的代码质量
3. 总结问题和改进建议
EOF
```

## 配置加载顺序

Omini 启动时按以下顺序加载配置：

1. **主配置**：`~/.omini/config.toml`
2. **项目配置**：`<project>/.omini/config.toml`（如果存在，合并到主配置）
3. **权限配置**：`<project>/.omini/permissions.toml`（如果存在）
4. **Bash 规则**：`~/.omini/rules/*.rules` 和 `<project>/.omini/rules/*.rules`
5. **Agent 指令**：`~/.omini/AGENTS.md` 和 `<project>/AGENTS.md`
6. **Skills**：内置 skills、`~/.omini/skills/*`、`<project>/.omini/skills/*`

所有配置都是可选的，Omini 可以在没有任何配置文件的情况下运行（使用默认值）。
