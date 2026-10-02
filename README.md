<div align="center">

# omini

在终端中阅读代码、修改项目并执行命令的 AI 编程助手。

</div>

Omini 支持多种模型服务、执行前规划、工具权限控制、后台 Agent、MCP 和可复用的 Skills。会话按项目管理，可继续已有工作，也可通过 `AGENTS.md` 约定项目规范。

## 安装

支持 macOS Apple Silicon 和 Linux x86_64：

```bash
curl -fsSL https://github.com/zhangzhenxiang666/omini/releases/latest/download/install.sh | sh
```

安装后请确保 `~/.local/bin` 已加入 `PATH`。安装脚本不会修改 shell 配置。

如需指定版本，将 `OMINI_VERSION` 传给安装脚本：

```bash
curl -fsSL https://github.com/zhangzhenxiang666/omini/releases/latest/download/install.sh | OMINI_VERSION=0.1.0 sh
```

## 开始使用

在项目目录运行：

```bash
omini
```

首次使用时按引导配置模型服务，也可以预先创建 `~/.omini/config.toml`。运行 Agent 前需要至少一个可用的 Provider 和模型，详情见[配置参考](docs/configuration.md)。

直接输入任务即可开始。使用 `/help` 查看命令，使用 `/init` 为项目生成或更新 `AGENTS.md`。更多操作见[使用指南](docs/usage.md)。

## 功能预览

### 项目对话

阅读文件、修改代码、运行命令，并继续已有会话。

![欢迎界面](assets/welcome.png)

### 计划模式

先讨论执行计划，确认后开始修改。

![计划模式](assets/plan.png)

### 权限管理

通过工具权限和 Bash 规则控制哪些操作可直接执行、需要确认或禁止执行。

![权限管理](assets/permissions.png)

### 上下文压缩

支持自动压缩和手动压缩，在长会话中保留后续工作需要的上下文。

![上下文压缩](assets/compact.png)

## 文档

- [使用指南](docs/usage.md)：会话、常用命令、后台任务和停止操作。
- [配置参考](docs/configuration.md)：模型服务、凭据、MCP 和项目配置。
- [权限配置](docs/permissions.md)：工具权限与 Bash 命令规则。
- [Agent 指令](docs/instructions.md)：用 `AGENTS.md` 定义项目规范。
- [Skills](docs/skills.md)：创建与调用可复用技能。
- [开发指南](docs/development.md)：从源码运行、验证和组件预览。
- [客户端协议](docs/protocol.md)：接入本地服务的公开接口。

完整目录见[文档索引](docs/index.md)，代码分层见[架构说明](ARCHITECTURE.md)。

## 许可

MIT
