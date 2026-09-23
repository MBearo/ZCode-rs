# zcode-cli-rust 多 crate 架构

## 目标

将现有单 crate 实现重构为 `apps/zcode-cli-rust` Cargo workspace，形成可复用的 zcode-cli-rust 核心，并由 App Server 与 Rust TUI 复用。当前 V4/App stdio wire contract 保持兼容；本规格只定义模块边界、状态所有权、端口和迁移顺序。

## Workspace 与依赖方向

目标 workspace 包含以下 crate：

- `zcode-cli-protocol`：V4 RPC、command、ACK、snapshot、event、capability 和错误 DTO；只包含 schema/type conversion。
- `zcode-cli-domain`：Session、history、queue、context、goal、todo、attachment、subagent 和 workspace identity 的纯领域模型。
- `zcode-cli-core-api`：`SessionRuntime`、`SessionStore`、`ModelPort`、`ToolPort`、`ContextPort`、`AuthPort` 和 `Clock` 公共契约。
- `zcode-cli-core`：Session actor、Agent loop、command admission、commit barrier、recovery 和 projection。
- `zcode-cli-state`：SQLite、session index/history、ACK、TS 只读导入、附件快照和 recovery。
- `zcode-cli-model`：OpenAI/Anthropic provider、SSE、retry、registry 和请求期鉴权。
- `zcode-cli-tools`：文件工具、Shell、后台任务、MCP、Skills、checkpoint/rewind 和 process cleanup。
- `zcode-cli-host`：workspace 路径/身份、配置、context source、平台路径和 runtime 环境组合。
- `zcode-cli-app-server`：stdio framing、RPC 路由、startup/storage handshake、snapshot/resync 和 App projection。
- `zcode-cli-tui`：只负责 TUI 输入和渲染，通过 `core-api` 工作。
- `zcode-cli-rust`：唯一二进制和 composition root，负责 clap、依赖注入、信号和前端选择。

依赖方向固定为：

```text
protocol/domain → core-api → core
                         ↑
                state/model/tools/host
                         ↓
              app-server / tui → cli binary
```

`domain` 不依赖 tokio、reqwest、rusqlite、文件系统或具体 adapter；`core` 不依赖 state/model/tools；TUI 不直接访问持久化或 provider。

## 状态所有权与事件顺序

```text
CLI/TUI/AppServer input
        │
        ▼
SessionRuntime / SessionActor（唯一业务状态 owner）
        │ command admission
        ▼
state transition → durable commit receipt → event projection
        │                                  │
        ├─ Agent loop → ModelPort/ToolPort ┘
        └─ snapshot/query
```

- accepted input 只能进入 Core owner 一次；前端只保留 draft 和 pending optimistic overlay。
- 模型结果、工具结果、问答、todo、goal 和 file checkpoint 必须先完成 durable commit，再继续模型请求或发布下一步事件。
- 旧 run 事件按 `sessionId + runId + generation` 丢弃。
- 所有任务携带 `traceId`；`sessionId`、`turnId`、`messageId`、`toolCallId` 和 `spanId` 是其结构化子标识。
- `workspaceIdentity?.trim() || workspacePath` 用于隔离、去重和关联；`workspacePath` 只用于文件操作、cwd 和展示。
- desktop 使用 `desktop-continuous` 连续流；mobile 使用 `web-remote-replayable` snapshot/resync 恢复；两者共享同一 Session owner。

## 公开端口

- `SessionRuntime::dispatch` 返回带幂等状态和 stale revision 的 `CommandAck`。
- `SessionRuntime::query` 只读，不激活 Session、不触发模型或工具。
- `SessionRuntime::subscribe` 返回带 trace/session/run/turn/sequence 的 typed event。
- `SessionStore::commit_receipt` 是唯一持久化写入入口，事务成功后返回 durable receipt；底层 `commit` 只作为 adapter 的实现钩子。
- `ModelPort::complete` 返回 canonical `ModelOutput` 或结构化 `ModelFailure`。
- `ToolPort::execute_scoped` 返回 model content、display projection、artifact reference 和 failure 状态。
- `AuthPort` 只提供请求范围凭据，凭据不进入 session/history/queue。

## 改名规则

本仓库尚未发布，不保留旧入口。旧单 crate 目录、旧包名、旧 Rust 标识符、旧数据目录变量和旧 runtime 值都已删除；唯一有效名称是：

- workspace：`apps/zcode-cli-rust`
- package/binary：`zcode-cli-rust`
- Rust 标识符：`zcode_cli_rust`
- 数据目录变量：`ZCODE_CLI_RUST_DATA_DIR`
- runtime 选择：`ZCODE_AGENT_SERVER_RUNTIME=zcode-cli-rust`

V4 wire method、已有 SQLite schema 和 TS 只读导入语义不在本任务中改动。

## 迁移顺序与验收

1. 先完成规格和全量改名，确保新名称可独立构建、启动和被测试发现。
2. 建立 workspace、`protocol`、`domain`、`core-api` 和 Rust dependency-direction checker。
3. 迁移 `domain` 与 `core`，保持 Session owner、commit barrier、recovery 和旧 run 防护。
4. 按能力拆分 state/model/tools/host adapters。
5. 重建 app-server 和 CLI composition root。
6. 接入最小 Rust TUI：创建/恢复、发送、流式事件、stop、queue、错误和 snapshot 恢复。

每阶段必须执行 `pnpm typecheck`、`pnpm lint`、`pnpm fmt:check`、`pnpm architecture:check --changed`、`pnpm check:zcode-cli-rust` 和 `pnpm test:zcode-cli-rust`。默认 TypeScript runtime、数据库 schema redesign 和完整 TUI parity 作为后续 gate。
