# ZCode CLI Rust

这是将 ZCode CLI 从 TypeScript 迁移到 Rust 的工作仓库。Rust 实现位于 [`apps/zcode-cli-rust/`](apps/zcode-cli-rust/)，以 Cargo workspace 组织协议、Session 核心、模型适配、工具执行、App Server 和 TUI。当前 Rust runtime 通过显式命令启动，现有 TypeScript runtime 仍是默认实现。

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

| 目录 | 作用 |
| --- | --- |
| `apps/zcode-cli-rust/src` | Rust CLI 组合根和启动参数 |
| `apps/zcode-cli-rust/crates/protocol` | App stdio/V4 协议 |
| `apps/zcode-cli-rust/crates/core`、`core-api`、`domain` | Session 核心及稳定接口 |
| `apps/zcode-cli-rust/crates/model`、`tools`、`state`、`host` | 模型、工具、存储和宿主适配器 |
| `apps/zcode-cli-rust/crates/app-server`、`tui` | App Server 与终端前端 |
| `docs/specs/` | 架构、迁移和验收规格 |

完整能力、配置项、数据导入和已知限制见 [`apps/zcode-cli-rust/README.md`](apps/zcode-cli-rust/README.md)。架构边界见 [`docs/specs/rust-cli-architecture.md`](docs/specs/rust-cli-architecture.md)。
