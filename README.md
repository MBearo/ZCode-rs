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

截至 2026-09-28，以下能力已有实现及自动化或实机验收记录；勾选只表示对应范围完成，不代表全部平台、供应商和交互组合均已验收：

- [x] Session、队列、冷恢复、compact 和 workspace identity 隔离；与 Node 共用会话库，支持交替续聊、崩溃恢复与不同会话并发写入
- [x] OpenAI Chat Completions、OpenAI Responses、Anthropic Messages
- [x] Provider Registry、模型选择、账号 overlay、每请求 Host 鉴权
- [x] build/edit/yolo/auto 权限模式、独立 Plan 状态、审批规则、hooks 与工作区信任
- [x] Read/Write/Edit/Glob/Grep/Bash、Edit 宽松匹配、后台任务、WebFetch/WebSearch、AskUserQuestion、Todo
- [x] MCP、Skill、插件管理与官方 MCP 鉴权；子代理、Goal、retry/edit、fork、选区侧聊和文件回退核心链路
- [x] 文本、图片、PDF、视频附件；图片缩放、媒体 Read 和请求媒体预算
- [x] `-p` 无头模式、自动标题、用量统计、实时遥测和本地首字时间
- [x] V4 桌面/手机分 profile 投递、有界增量回放、snapshot 恢复与流控（协议集成测试）
- [x] macOS ARM64、Windows x64 npm 平台包发布；macOS 原版 App 启动、切回 Node 与再切回 Rust 的基础验收

剩余 TODO：

- [ ] MCP OAuth、legacy SSE 自动重连、真实服务器兼容矩阵与插件商店 UI 全流程
- [ ] 主代理记忆、目录级规则、完整 prompt/output style，以及附件和工具的剩余兼容差分
- [ ] 自动任务、Cron/OffPeak、浏览器/CUA、Node REPL；子代理 transcript artifact 与子树预算聚合
- [ ] 超大会话按需投影、产物回收与文件恢复边界；跨窗口、多会话、远端/手机和真实供应商完整验收
- [ ] Windows 桌面端到端、Linux/macOS x64/Windows ARM64 原生验收与发行、SSH/WSL 部署、完整升级回退和性能发布门槛

动态工作流不在当前迁移范围，TUI 只保留子命令入口。分项证据与剩余边界见[对齐清单](docs/specs/rust-parity-remaining.md)、[共用会话库规格](docs/specs/rust-m11-node-storage.md)和 [npm 启动器验收记录](docs/reports/rust-npm-app-launcher-2026-09-28.md)。默认 runtime 仍为 TypeScript。

2026-09-25 在同一台 Apple M1 Max 上使用本地 SSE fixture、8 回合单 Session、5 次交错测量的结果如下。Node.js 使用当次 CLI bundle，Rust 使用切换到 Node 会话库后的 release 二进制：

| 指标             | Node.js |     Rust |
| ---------------- | ------: | -------: |
| 空闲 RSS         | 416 MiB | 17.8 MiB |
| 负载采样峰值 RSS | 520 MiB | 28.7 MiB |
| 累计 CPU time    |  0.92 s |   0.05 s |
| 8 回合耗时       |  1.50 s |   0.26 s |

这组数据只代表当次本地 fixture 和构建形态，测量时机器存在其他高负载进程。大历史冷读取与驻留另有专项数据；真实供应商、Electron/Host 总进程、MCP 工具负载、Windows/Linux 和移动远控的性能仍需单独验收。完整口径见[性能报告](docs/reports/rust-performance-2026-09-25.md)；[2026-09-23 报告](docs/reports/rust-node-resource-parity-2026-09-23.md)保留为历史基线。

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

App Server 的 stdout 只输出协议帧，诊断信息写入 stderr。`--cwd` 指定工作区；`--data-dir` 只放工作区 owner 锁与 Rust 工具缓存，默认 `~/.zcode/rust`。会话与 Node 共用数据库，默认 `~/.zcode/cli/db/db.sqlite`，路径可由存储配置覆盖；仅设置 `--data-dir` 不会隔离会话库。也可以直接使用 Cargo：

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

完整能力、配置项、共用会话库和已知限制见 [`apps/zcode-cli-rust/README.md`](apps/zcode-cli-rust/README.md)。架构边界见[架构规格](docs/specs/rust-cli-architecture.md)，功能 TODO 见[对齐清单](docs/specs/rust-parity-remaining.md)，资源对比见 [2026-09-25 性能报告](docs/reports/rust-performance-2026-09-25.md)。
