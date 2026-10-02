# 开发指南

本文面向修改 Omini 源码的开发者。跨 crate 或核心流程修改前，先阅读[架构说明](../ARCHITECTURE.md)和[仓库协作约定](../AGENTS.md)。

## 构建与运行

在仓库根目录构建客户端和服务端：

```bash
cargo build -p omini -p omini-server
```

工具链由 `rust-toolchain.toml` 指定。从源码启动时，使用本次构建的服务端：

```bash
OMINI_SERVER_BIN="$PWD/target/debug/omini-server" cargo run -p omini
```

`OMINI_SERVER_BIN` 必须是已有服务端二进制的绝对路径。运行 Agent 仍需完成[模型配置](configuration.md)。

安装包将用户命令放在 `~/.local/bin/omini`，将 `omini-server` 和内置搜索工具 `rg` 放在 `~/.omini/bin/`。搜索使用内置 `rg`；缺失时服务端尝试下载匹配自身版本的发布资产，并校验 SHA-256。恢复失败时无法启动 Agent 任务。

## 组件预览

修改终端组件时可使用离线预览，无需启动服务或配置模型：

```bash
cargo run -p omini -- tui-debug
```

左右方向键切换场景，上下方向键滚动，Home/End 跳转首末场景，q 退出。预览复用正式组件与布局，适合检查不同终端尺寸下的显示。该子命令由 `#[cfg(debug_assertions)]` 门控，仅 debug 构建注册并编译，release 二进制会拒绝该命令，图鉴代码也不参与编译；`cargo run -p omini-tui --example debug_snapshot` 截图导出同样仅限 debug 构建。

## 验证

按改动范围运行受影响 crate 的测试，例如：

```bash
cargo test -p omini-tui --lib
```

非简单 Rust 修改还需运行：

```bash
cargo fmt --all --check
cargo clippy --workspace
```

纯文档修改无需构建。若修改配置示例，运行文档示例解析测试：

```bash
cargo test -p omini-config --test configuration full_documentation_example_parses
```

## 文档维护

README 说明产品用途和入门路径；使用文档说明如何完成任务、配置选项和必要限制；客户端协议记录公开接口约束；架构说明记录内部职责与不变量。

文档描述当前能力。界面调整只有改变操作方式或可用能力时才需要更新使用说明；组件样式、预览场景和改动过程留在代码、测试或提交记录中。

## 接入本地服务

实现其他客户端时查阅[客户端协议](protocol.md)和服务提供的 OpenAPI 描述；代码内部的执行与持久化边界见[架构说明](../ARCHITECTURE.md)。

[返回文档索引](index.md)
