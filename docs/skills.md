# Skills 配置

Skills 是可复用的 Agent 技能包，每个 skill 是一个包含 `SKILL.md` 的目录。

## 文件路径

| 来源 | 路径 | 说明 |
|------|------|------|
| 内置 | Omini 自带 | `commit-message`、`skill-creator` 等 |
| 用户级 | `~/.omini/skills/<skill-name>/SKILL.md` | 全局 skills，对所有项目生效 |
| 项目级 | `<project>/.omini/skills/<skill-name>/SKILL.md` | 项目级 skills，仅对当前项目生效 |

## 加载优先级

Skills 按以下顺序加载，后加载的覆盖先加载的同名 skill：

1. 内置 skills 最先加载
2. 用户级 skills 覆盖同名内置 skills
3. 项目级 skills 覆盖同名用户级 skills

## SKILL.md 格式

`SKILL.md` 在文件头部使用 YAML frontmatter 定义元数据，后面是 skill 的主体内容（Markdown 格式）：

```markdown
---
name: code-reviewer
description: 审查代码变更，提供改进建议
inject: true
user-invocable: true
---

你是一个代码审查专家。当被调用时：

1. 检查变更的代码质量
2. 识别潜在的问题和 bug
3. 提供具体的改进建议
```

## Frontmatter 字段

| 字段 | 类型 | 必需 | 默认值 | 说明 |
|------|------|------|--------|------|
| `name` | `String` | ✅ | — | Skill 的唯一标识名，用于调用 |
| `description` | `String` | ✅ | — | Skill 的简短描述，显示在系统提示中 |
| `inject` | `bool` | ❌ | `true` | 是否注入到系统提示的 skill 列表中 |
| `user-invocable` | `bool` | ❌ | `true` | 用户是否可以通过 `/skill` 命令调用 |

### inject 字段

- `inject: true`（默认）：skill 的 name 和 description 会出现在系统提示中，Agent 知道这个 skill 的存在
- `inject: false`：skill 不会出现在系统提示中，但仍然可以通过 `/skill` 命令调用

### user-invocable 字段

- `user-invocable: true`（默认）：用户可以通过 `/skill <name>` 命令调用
- `user-invocable: false`：只有 Agent 可以调用，用户不能直接调用

## 目录结构

Skill 目录可以包含额外的资源文件，这些文件的路径会通过 `<skill_directory>` 标签传递给 Agent：

```
~/.omini/skills/my-skill/
├── SKILL.md          # 必需：skill 定义
├── templates/        # 可选：模板文件
│   └── report.md
└── examples/         # 可选：示例文件
    └── sample.toml
```

## 相关文档

- [主配置文件](configuration.md) — `config.toml` 的其他配置项
- [权限配置](permissions.md) — 工具和 Bash 命令的权限控制
- [Agent 指令](instructions.md) — `AGENTS.md` 文件的用途与格式
- [返回文档索引](index.md)
