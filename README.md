# ZCode CLI Rust

这是将 ZCode CLI 从 TypeScript 迁移到 Rust 的工作仓库。Rust 实现位于 [`apps/zcode-cli-rust/`](apps/zcode-cli-rust/)，以 Cargo workspace 组织协议、Session 核心、模型适配、工具执行、App Server 和 TUI。当前 Rust runtime 通过显式命令启动，现有 TypeScript runtime 仍是默认实现。

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

## GitHub Actions 构建 Electron 安装包

在 GitHub 仓库的 **Actions → Build Electron App → Run workflow** 中选择分支运行；推送 `v*` tag 也会触发构建。完成后从该次运行的 **Artifacts** 下载：

- `zcode-macos-arm64`：Apple Silicon 的 `.dmg` 和 `SHA256SUMS.txt`。
- `zcode-windows-x64`：Intel/AMD 64 位 Windows 的 `.exe` 和 `SHA256SUMS.txt`。

只构建这两个目标，产物保留 14 天。使用当前默认的 TypeScript Agent，不切换 Rust runtime，也不自动创建 GitHub Release。

Windows 先生成未签名测试包。macOS 未配置 Apple Secrets 时生成带 ad-hoc 临时签名的测试包，首次打开可能需要用户在“隐私与安全”中放行；这不等于 Developer ID 发布签名。

要启用 macOS 正式签名和公证，在 **Settings → Secrets and variables → Actions → New repository secret** 添加以下五项，名称与旧私有 CI 保持一致：

| Secret                                        | 内容                                                          |
| --------------------------------------------- | ------------------------------------------------------------- |
| `APPLE_DEVELOPER_ID_APPLICATION_P12_BASE64`   | 含 Developer ID Application 证书和私钥的 `.p12` 文件的 Base64 |
| `APPLE_DEVELOPER_ID_APPLICATION_P12_PASSWORD` | 上述 `.p12` 的导出密码                                        |
| `APPLE_ID`                                    | Apple 开发者账号邮箱                                          |
| `APPLE_PASSWORD`                              | 该账号生成的 Apple 专用密码，不是登录密码                     |
| `APPLE_TEAM_ID`                               | Apple Developer Team ID                                       |

五项齐全时自动签名、公证并验证 DMG，全部未配置时生成测试包，只配置部分则明确失败。签名身份从证书自动解析，不用另配 `APPLE_SIGNING_IDENTITY`。证书、私钥和密码不要提交到 Git。

Workflow 位于 [`.github/workflows/build-desktop.yml`](.github/workflows/build-desktop.yml)，构建边界和验收规则见 [`docs/specs/electron-github-build.md`](docs/specs/electron-github-build.md)。

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
