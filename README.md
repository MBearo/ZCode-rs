# ZCode CLI Rust

这是将 ZCode CLI 从 TypeScript 迁移到 Rust 的工作仓库。Rust 实现位于 [`apps/zcode-cli-rust/`](apps/zcode-cli-rust/)，以 Cargo workspace 组织协议、Session 核心、模型适配、工具执行、App Server 和 TUI。当前 Rust runtime 通过显式命令启动，现有 TypeScript runtime 仍是默认实现。

## 通过 npm 使用 Rust runtime

通过 [`@mbears/zcode-rs`](https://www.npmjs.com/package/@mbears/zcode-rs) 启动已安装的 ZCode，无需克隆仓库或自行编译 Rust。

- 已安装 **ZCode 3.14.3**；启动器会拒绝其他 App 版本。
- 已安装 **Node.js 22.16.0 或更高版本**（含 npm / npx）。
- 当前 npm 版本为 **0.1.0**，已发布 **macOS Apple Silicon（ARM64）** 和 **Windows x64** 平台包。

先完整退出 ZCode；Windows 若有托盘图标，也需要从托盘退出。然后在终端或 PowerShell 中执行：

```sh
# 检查 App 版本和当前平台的 Rust 包
npx @mbears/zcode-rs@0.1.0 app doctor

# 使用 Rust runtime 启动 ZCode
npx @mbears/zcode-rs@0.1.0 app launch
```

npm 会自动选择对应平台的预编译包。`app launch` 使用已安装 App 的界面，并为这次启动选择本地 Rust runtime；远端 SSH/WSL runtime 不随之切换。

**恢复原版：**完整退出这次启动的 ZCode，再从原来的应用图标、桌面或开始菜单快捷方式打开，即使用原版内置 CLI。启动器没有覆盖 App 安装文件或修改原快捷方式，无需重装或卸载 npm 包；回退不会撤销使用期间产生的会话和文件改动。

当前为测试版，Rust 功能范围和已知限制见下文及 [启动器说明](apps/zcode-cli-rust/npm/README.md)。Windows x64 已通过原生编译、公开 npm 安装、exe 执行和启动器检查，尚未覆盖真实 Windows 桌面 UI 端到端测试；详见 [验收记录](docs/reports/rust-npm-app-launcher-2026-09-28.md)。

## 核心目标

本仓库的核心任务是把现有 Node.js/TypeScript zcode CLI runtime 迁移到 Rust，在保持 App stdio/V4 协议、Session 状态、模型请求、工具执行、持久化和桌面接入语义的前提下，逐步用 Rust runtime 替换 Node.js runtime。只有功能对齐、性能、数据迁移、跨平台和发布回退完成验收后，才切换默认 runtime。

当前迁移已经覆盖 Rust App Server 的主要使用链路：

- [x] Session、SQLite、队列、冷恢复、compact 和 workspace identity 隔离
- [x] OpenAI Chat Completions、OpenAI Responses、Anthropic Messages
- [x] Provider Registry、模型选择、账号 overlay、yolo 能力协商
- [x] Read/Write/Edit/Glob/Grep/Bash、后台任务、AskUserQuestion、Todo
- [x] MCP、Skill、子代理、Goal、retry/edit、fork 和文件回退核心链路
- [x] 文本、图片、PDF 基础附件链路
- [ ] 权限模式、MCP OAuth、完整记忆和目录规则、高级媒体与工具能力
- [ ] 工作流、自动任务、浏览器/CUA、远端/手机恢复和跨平台发行

同一台 Apple M2 Pro 上使用本地 SSE fixture、8 回合单 Session、5 次交错测量的结果如下。Node.js 使用当前 CLI bundle，Rust 使用 release 二进制：

| 指标             |   Node.js |     Rust |
| ---------------- | --------: | -------: |
| 空闲 RSS         | 408.8 MiB | 11.2 MiB |
| 负载采样峰值 RSS | 522.2 MiB | 23.4 MiB |
| 累计 CPU time    |    0.78 s |   0.10 s |
| 8 回合耗时       |   1.289 s |  0.147 s |

这组数据只代表当前本地 fixture 和构建形态；真实供应商、Electron/Host 总进程、大历史、MCP、Windows/Linux 和移动远控仍需单独验收。完整测量方法、功能差异和 TODO 见 [`docs/reports/rust-node-resource-parity-2026-09-23.md`](docs/reports/rust-node-resource-parity-2026-09-23.md)。

## 上游同步记录

- **v3.14.3**：同步上游 ZCode v3.14.3 更新，保留本仓库的 Rust CLI 迁移实现。

## 环境要求

- Rust `1.89` 或更高版本
- Node.js 和 pnpm（桌面联调、集成测试及仓库脚本需要；版本以 [`mise.toml`](mise.toml) 为准）
- macOS、Linux 或 Windows

首次准备仓库依赖：

```sh
pnpm install
```

## 启动 Rust runtime

### 直接启动 App Server

先构建 Rust 二进制：

```sh
pnpm build:zcode-cli-rust
```

然后通过 stdio 启动 App Server：

```sh
apps/zcode-cli-rust/target/debug/zcode-cli-rust app-server --stdio \
  --cwd "$PWD" \
  --data-dir "$PWD/.zcode-runtime/rust"
```

App Server 的 stdout 只输出协议帧，诊断信息写入 stderr。`--cwd` 指定工作区，`--data-dir` 指定 Rust 独立数据目录；未指定数据目录时默认使用 `~/.zcode/rust`。也可以直接使用 Cargo：

```sh
cargo run --locked --manifest-path apps/zcode-cli-rust/Cargo.toml -- \
  app-server --stdio --cwd "$PWD" --data-dir "$PWD/.zcode-runtime/rust"
```

直接运行时可以通过 `--config /absolute/path/model.json` 指定单模型配置，并在环境变量中提供配置所需的 API key。通常接入桌面 App 时不需要手写模型配置，App 会提供现有的 Provider Registry 和账号设置。

### 接入桌面 App

使用仓库脚本构建并启动 Electron，同时显式选择 Rust runtime：

```sh
pnpm dev:desktop:zcode-cli-rust
```

默认构建 release 二进制。调试 Rust 代码时使用 debug 构建：

```sh
pnpm dev:desktop:zcode-cli-rust --debug
```

需要隔离实验数据或使用 fixture 模型时：

```sh
pnpm dev:desktop:zcode-cli-rust \
  --config /absolute/path/model.json \
  --data-dir /absolute/path/rust-experiment
```

该脚本会设置 `ZCODE_AGENT_SERVER_RUNTIME=zcode-cli-rust`，由 Host 启动 Rust App Server；未执行该脚本或未设置 runtime override 时，桌面 App 仍使用 TypeScript runtime。

## 构建、检查与测试

```sh
pnpm build:zcode-cli-rust  # 构建 debug 二进制
pnpm check:zcode-cli-rust  # 边界检查、格式检查和 Clippy
pnpm test:zcode-cli-rust   # Rust 单测及 App 集成测试
```

需要 release 产物时：

```sh
cargo build --locked --release --manifest-path apps/zcode-cli-rust/Cargo.toml
```

## 目录说明

| 目录                                                         | 作用                         |
| ------------------------------------------------------------ | ---------------------------- |
| `apps/zcode-cli-rust/src`                                    | Rust CLI 组合根和启动参数    |
| `apps/zcode-cli-rust/crates/protocol`                        | App stdio/V4 协议            |
| `apps/zcode-cli-rust/crates/core`、`core-api`、`domain`      | Session 核心及稳定接口       |
| `apps/zcode-cli-rust/crates/model`、`tools`、`state`、`host` | 模型、工具、存储和宿主适配器 |
| `apps/zcode-cli-rust/crates/app-server`、`tui`               | App Server 与终端前端        |
| `docs/specs/`                                                | 架构、迁移和验收规格         |

完整能力、配置项、数据导入和已知限制见 [`apps/zcode-cli-rust/README.md`](apps/zcode-cli-rust/README.md)。架构边界见 [`docs/specs/rust-cli-architecture.md`](docs/specs/rust-cli-architecture.md)。 Node.js 与 Rust 的资源对比和功能 TODO 见 [`docs/reports/rust-node-resource-parity-2026-09-23.md`](docs/reports/rust-node-resource-parity-2026-09-23.md)。
