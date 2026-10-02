# Skills

Skills 用于复用代码审查、文档编写等工作流程。每个 Skill 是一个包含 `SKILL.md` 的目录，可以附带模板、脚本和示例。

## 创建 Skill

在 `~/.omini/skills/code-reviewer/SKILL.md` 或 `<project>/.omini/skills/code-reviewer/SKILL.md` 中写入：

```markdown
---
name: code-reviewer
description: 审查代码变更，提供改进建议
argument-hint: "[目标或额外要求]"
user-invocable: true
---

检查指定范围的代码变更：

1. 理解变更目的与相关行为
2. 查找正确性问题和缺失的边界处理
3. 按影响程度说明问题，并给出验证建议
```

随后在项目会话中调用：

```text
/code-reviewer 检查当前变更的错误处理
```

命令后的文本、文件引用和其他输入作为本次调用的补充要求。调用 Skill 不会改变工具权限。

## 文件位置与同名覆盖

| 来源 | 位置 | 生效范围 |
| --- | --- | --- |
| 内置 | Omini 自带，例如 `skill-creator` | 所有项目 |
| 用户级 | `~/.omini/skills/<name>/SKILL.md` | 所有项目 |
| 项目级 | `<project>/.omini/skills/<name>/SKILL.md` | 当前项目 |

同名 Skill 按项目级、用户级、内置的顺序选择，优先使用更具体的定义。

## 元数据参考

`SKILL.md` 以 YAML frontmatter 开头，之后是非空的 Markdown 正文。

| 字段 | 类型 | 必填 | 默认值 | 用途 |
| --- | --- | --- | --- | --- |
| `name` | 字符串 | 是 | — | 调用时使用的 Skill 名称 |
| `description` | 字符串 | 是 | — | 说明 Skill 适合处理什么任务 |
| `short-description` | 字符串 | 否 | 无 | 简短说明 |
| `argument-hint` | 字符串 | 否 | 无 | 提示调用时可补充的参数 |
| `disable-model-invocation` | 布尔值 | 否 | `false` | 为 `true` 时不向 Agent 提供该 Skill 的自动发现摘要 |
| `user-invocable` | 布尔值 | 否 | `true` | 是否允许通过 `/<skill-name>` 调用 |

`disable-model-invocation` 控制自动发现，不限制工具执行权限；用户命令是否可用由 `user-invocable` 决定。

## 附带资源

```text
my-skill/
├── SKILL.md
├── templates/
│   └── report.md
└── examples/
    └── sample.toml
```

在正文中说明资源的用途和使用时机，并用相对 Skill 目录的路径引用。Agent 加载技能时可以取得该目录的位置。

[返回文档索引](index.md)
