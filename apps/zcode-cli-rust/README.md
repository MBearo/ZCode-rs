# zcode-cli-rust

Cargo workspace 提供 App Server 与 Rust TUI 共用的 Session 核心。App Server 继续实现现有 stdio/V4 协议，TUI 通过 `zcode-cli-core-api` 读取同一运行时；依赖由 `Cargo.lock` 固定，SQLite 编译进二进制。Node 只用于现有 Electron App 和集成测试，zcode-cli-rust 本身不调用 Node。

crate 边界见 [架构规格](../../docs/specs/rust-cli-architecture.md)：`protocol`/`domain`/`core-api`/`core` 提供稳定核心，`state`、`model`、`tools`、`host` 是 adapter，`app-server` 和 `tui` 只负责前端输入、传输与投影。

## 当前能力

- OpenAI Chat Completions、OpenAI Responses、Anthropic Messages；文本对话、SSE 正文/reasoning、分片 tool call、usage 与各协议推理元数据回传。
- HTTP 连接复用、有界重试、Retry-After、空响应恢复、闲置/显式总超时；App 显示重试等待，stop 可取消请求与退避。
- 首段即时交付，后续 16 ms/8 KiB 合并；reasoning 历史回传；可见输出后断流保留中断内容，不透明重放。
- 权限模式 build/edit/plan/yolo/auto、Plan 模式与 hooks；Read/Write/Edit 使用 TS 标准参数（Edit 与 Node 相同的宽松匹配），Glob/Grep 原生搜索；后台 Bash、TaskOutput/TaskStop、输出文件、文件 diff 投影。List 仅兼容旧 native 调用，不再对模型公开。
- MCP（stdio、Streamable HTTP、legacy SSE，插件 options 与官方鉴权）、Skill、插件市场与安装/更新/卸载/恢复内置、子代理（Agent/SendMessage/TaskOutput/TaskStop）、Goal（续跑与验证）、分叉与选区侧聊、编辑/重试/回退与文件恢复、分享上下文导入、自动标题、`-p` 无头模式。
- AskUserQuestion 复用 App 问答界面；支持单选、多选、自定义/部分回答、跳过、拒绝、自动继续与暂停倒计时。问题和回答先提交再唤醒工具；冷恢复保留已提交回答，未回答的问题标记中断。
- TodoRead/TodoWrite 保存会话任务清单，投影 App 工作计划摘要；支持全量替换/清空、冷恢复与压缩后读取（与 Node 共用 `todo` 表）。进度提示按当前 TS 的十轮间隔注入，TodoWrite 保持顺序提交屏障。
- 会话创建、重命名、历史读取、FIFO 输入/compact 维护队列、队列编辑、held queue 保留/清空发送、sendQueuedNow、stop 和幂等 ACK 查询。
- 手动 /compact、自动预算压缩、超限后的单次反应式压缩、旧工具结果 microcompact；摘要边界与时间线同事务保存，完整历史保留；每次请求刷新根 AGENTS.md。
- 会话只存 Node 的 `db.sqlite`（记录格式与 Node 逐字节一致）；崩溃中断恢复、workspace identity 隔离、进程 owner 锁、旧 run 事件丢弃。
- V4 conversation/sessions-index/workspace-config 投影、desktop/mobile 独立订阅、分片校验、有界增量回放、snapshot 恢复和流控。
- App 原生存储准备、能力协商和显式 runtime 选择。
- 直接读取 App Provider Registry、个人设置及账号 overlay；热更新、模型/档位切换、每请求 Host 鉴权、连通性测试和 workspace 文本生成/取消。
- 与 Node 共用会话库：Node 写入的会话直接冷加载并可继续，Rust 写入的会话 Node 同样可读，两个进程可交替续写同一会话、同时写不同会话；附件分块预览读取 Node 产物。
- 提示附件按 Node 规则解析：图片按 Node Jimp 规则缩放（长边 2000、5 MiB），本地视频上限 30 MiB；每个模型请求的媒体按 Node 的 40 MiB 预算保留最新输入与较新的历史媒体。
- 模型/工具结果及后台登记提交屏障；Read/List/Glob/Grep 最多四并发，写入/Shell 顺序执行；存储失败停止执行且禁止收口再次提交失败状态。

尚未替换默认 TypeScript runtime。MCP OAuth、自动任务、Cron/OffPeak、浏览器/CUA、Node REPL 和主代理记忆尚未实现；动态工作流不在当前迁移范围，TUI 只保留子命令入口；未支持的命令明确拒绝。与 Node 的刻意差异和照 Node 保留的缺陷见 [M11 spec](../../docs/specs/rust-m11-node-storage.md) §2.5、§5.3，其余未完成项见[对齐清单](../../docs/specs/rust-parity-remaining.md)。订阅重连在 epoch 与日志水位有效时增量恢复，日志淘汰或 epoch 变化时回退 snapshot；协议测试覆盖两种 profile，真实手机远控完整验收仍待完成，见 [M8 spec](../../docs/specs/rust-m8-delivery.md)。Shell 不提供 OS sandbox。

## 构建与验证

仓库根目录执行。Rust >= 1.89，当前验证工具链为 1.95.0；Node/pnpm 依 `mise.toml`。

```sh
pnpm build:zcode-cli-rust
pnpm check:zcode-cli-rust
pnpm test:zcode-cli-rust
pnpm typecheck
pnpm lint
```

集成测试启动真实 Rust 二进制、临时 SQLite、临时工作区和本地 HTTP 模型 fixture，使用 App 的实际协议客户端、Host 服务与 V4 schema/assembler。不会请求真实模型或读取个人账号。验证记录见 [spec](../../docs/specs/rust-cli-core.md)。

与 Node 存储对齐的检查（均在仓库根目录，`TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.zcode-cli-rust.json node --import tsx <脚本>`；先 `cargo build --locked --release --manifest-path apps/zcode-cli-rust/Cargo.toml --bin zcode-cli-rust --example node_read`，互操作脚本还需要已构建的 `apps/zcode-cli/packages/cli/dist/zcode.cjs`）：

| 脚本                                            | 内容                                                                                                                 |
| ----------------------------------------------- | -------------------------------------------------------------------------------------------------------------------- |
| `scripts/zcode-cli-rust-node-storage-check.mjs` | Rust 写入的各类会话由 Node 仓储、history hydrator 与冷投影读回，与 Rust 冷读取逐项相等                               |
| `scripts/zcode-cli-rust-real-db-check.mjs`      | 用户现有库的在线备份：Rust 启动不改迁移与表结构，逐会话比对两端冷读取，Rust 续写后 Node 读回；只打印计数，结束删副本 |
| `scripts/zcode-cli-rust-node-interop.mjs`       | 真实 Node CLI（`zcode.cjs`）与 Rust：交替续写（含工具）、流式/工具执行中强杀后由对方恢复、两进程并发写               |

2026-09-25 的结果：用户库 5,397 个会话全部一致；交替、崩溃恢复与并发场景全部通过（spec §10 M11.6）。

macOS/Linux 的 TLS 证书失败测试使用本地 `openssl` 生成临时自签名证书，测试后删除；Windows 暂跳过这一用例。Rust 单测及受控存储测试不依赖外部服务。

性能比较使用 release 产物：

```sh
cargo build --locked --release --manifest-path apps/zcode-cli-rust/Cargo.toml
node scripts/bench-zcode-cli-rust-suite.mjs /absolute/baseline-binary apps/zcode-cli-rust/target/release/zcode-cli-rust .zcode-runtime/rust-bench/requests 256000 256000
node scripts/bench-zcode-cli-node-rust.mjs apps/zcode-cli/packages/cli/dist/zcode.cjs apps/zcode-cli-rust/target/release/zcode-cli-rust .zcode-runtime/node-rust-bench 5
TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.zcode-cli-rust.json node --import tsx scripts/bench-zcode-cli-rust-session-memory.mjs /absolute/a /absolute/b .zcode-runtime/rust-bench/memory
```

第一个脚本串行、交替比较两个版本，在固定流式、100 轮历史和四会话场景各重复五次；记录原始样本及 summary.json。RSS 是定时采样值；存储开销同时记录运行中 SQLite/SHM/WAL 占用和正常 EOF 后持久文件大小，不等于累计物理写入字节。第二个脚本用同一本地模型比较 Node CLI 与 Rust；第三个测大历史冷读取、空闲驻留（macOS 另记 `footprint`，`ps` RSS 含分配器可复用页）与分页。

2026-09-25（M1 Max，机器高负载，中位数，详见[报告](../../docs/reports/rust-performance-2026-09-25.md)）：

| 场景                         | 结果                                                                                                     |
| ---------------------------- | -------------------------------------------------------------------------------------------------------- |
| 切换到 Node 库前后           | 对话路径变化在 4% 以内；流式场景存储 6.15 → 3.18 MiB；空目录首次启动多一次建库迁移（约 +9 ms、+4.3 MiB） |
| 与 Node CLI（8 轮 × 256 块） | 启动 786 → 20.6 ms，空闲 RSS 416 → 17.8 MiB，峰值 520 → 28.7 MiB，CPU 0.92 → 0.05 s                      |
| 大历史冷读取                 | 4 MiB 会话 p95 21.6 ms，footprint 18 MiB；20,000 条消息的压测会话约 4.4 s（Node 4.1 s）                  |

`requestTimeoutSeconds` 仅在显式配置时限制单次 HTTP 尝试总时长；省略表示不设固定总上限。`streamIdleTimeoutMs` 默认 600000，重试每次增加 30000；设为 0 可禁用闲置超时。重试配置可通过可选 `retry` 对象设置 `maxRetries`、`baseDelayMs`、`backoffFactor`、`maxDelayMs`、`jitter`；未提供的字段沿用 `ZCODE_MODEL_RETRY_*` 环境变量及 CLI 默认值（10 次、2 秒、因子 2、60 秒、启用 jitter）。该预算仅允许在尚未交付可见输出时重试，空响应最多重试一次。

## 接入 App

已安装的正式版 **ZCode 3.14.3** 可通过 [npm 启动器](npm/README.md) 使用 Rust；它以原产品 tag 为兼容基线，从专用启动目录运行原 App。下面的开发命令使用当前仓库的 Desktop，不作为已发布 App 的兼容依据。

通常直接使用现有 App 设置，无需 Rust 模型 JSON：

```sh
pnpm dev:desktop:zcode-cli-rust
```

默认构建并启动 release 二进制，关闭 incremental；需要调试符号时显式加 `--debug`。

此命令显式选择 Rust，保留 App 配置/账号和任务索引。Host 注入既有 builtin/personal 文件路径；账号仅提供权益 overlay，每次网络请求的临时鉴权由原 Host 服务解析。使用 yolo 并关闭 Plan。普通 `pnpm dev:desktop` 仍运行 TS。

`--data-dir` 可隔离 App/Electron/Agent 数据，不会把普通 App 设置复制到实验环境。独立部署和 fixture 仍可使用显式单模型配置：

```json
{
  "apiType": "openai-chat-completions",
  "providerId": "my-provider",
  "modelId": "my-model",
  "reasoningLevel": "none",
  "reasoningParameters": { "reasoning_effort": "none" },
  "baseUrl": "https://provider.example/v1",
  "apiKeyEnv": "ZCODE_MODEL_API_KEY",
  "requestTimeoutSeconds": 180
}
```

在启动环境中设置 `ZCODE_MODEL_API_KEY`，然后执行：

```sh
pnpm dev:desktop:zcode-cli-rust --config /absolute/path/model.json --data-dir /absolute/path/rust-experiment
```

带 `--config` 时使用显式单模型模式，App 需有对应选型供其就绪门禁使用。执行配置以 JSON 为准；该模式不消费账号 overlay。

静态模式的 `reasoningParameters` 按协议配置：Chat 接受 reasoning_effort/thinking/enable_thinking，Responses 接受 reasoning 或 reasoning_effort，Anthropic 接受 thinking。Registry 模式自动使用 App option maps。模型切换先提交，下一个模型步骤生效；排队输入保留接收时的选型。默认选型来自 App `defaultModelSelection`，恢复会话保留自身选择。跨 provider/model 不回放私有推理签名，正文和工具关联保留。

手工集成可设置 `ZCODE_AGENT_SERVER_RUNTIME=zcode-cli-rust`、`ZCODE_AGENT_SERVER_COMMAND=<binary>`、`ZCODE_AGENT_SERVER_ARGS_JSON=["app-server","--stdio","--data-dir","...","--config","..."]`。Host 自动补充 `--cwd` 与 `--surface desktop`；不要在 args 中另传 `--cwd`。不设置这些变量仍使用原有 TS runtime。

直接使用协议入口：

```sh
apps/zcode-cli-rust/target/debug/zcode-cli-rust app-server --stdio \
  --cwd /absolute/workspace --data-dir /absolute/isolated-data \
  --config /absolute/path/model.json
```

stdout 仅输出 NDJSON；stderr 为诊断。`--prepare-storage` 使用原 Host 握手，不调用模型。未提供 Registry 文件路径或显式 config 时，只读原生历史。运行队列不跨进程恢复，command query 明确标记未执行输入被丢弃。

会话存储与 Node 共用同一个库（默认 `~/.zcode/cli/db/db.sqlite`，按 Node 的分层配置 `storage.dir`、`storage.sessionDbPath` 与 `ZCODE_SESSION_DB_PATH` 解析），产物（附件快照、工具结果媒体、checkpoint）在 `<storage.dir>/cli/artifacts`。Node 与 Rust 可交替打开同一会话继续对话，不做导入、备份或反向同步；`--data-dir`（默认 `~/.zcode/rust`）只放工作区 owner 锁与 Rust 工具缓存。记录格式、冷加载规则与已知差异见 [rust-m11-node-storage](../../docs/specs/rust-m11-node-storage.md)。

切回 Node 时取消 runtime override 即可，Node 直接读取 Rust 写入的会话。P0 实现与验收见 [spec](../../docs/specs/rust-app-p0.md) 和 [报告](../../docs/reports/rust-app-p0.md)。

App 集成验证主要在 macOS 完成；真实 GLM-5.3 与 Electron Renderer 的基础对话、工具、输入、附件和问答已有[实机证据](../../docs/specs/rust-parity-remaining.md)。截至 2026-09-28，macOS ARM64 与 Windows x64 npm 平台包已发布；Windows 已完成原生构建、公开 npm 安装、exe 执行与启动器 fixture 验证，macOS 原版 App 已完成 Rust → Node → Rust 的基础切换验收，见[npm 启动器报告](../../docs/reports/rust-npm-app-launcher-2026-09-28.md)。完整供应商/交互矩阵、Windows 桌面端到端、其余平台与 SSH/WSL 部署仍待验证。

## Coding 工具与后台执行

权限模式与 Plan 规则见 [M2 spec](../../docs/specs/rust-m2-permissions-plan-hooks.md)。后台任务绑定 session，正常前台结束后继续运行；TaskOutput 可查询或等待，TaskStop 和 App cancelBackgroundWork 可停止。stop 和 EOF 收回任务进程树。后台 Shell 任务与 Node 一样不落库，重启后不再出现，不重启未知副作用。

Read 使用 file_path/offset/limit；Write 使用 file_path/content；Edit 使用 file_path/old_string/new_string/replace_all。相对路径按 cwd 解析，也允许显式访问工作区外路径（与 TS yolo 一致）。已有文件必须先 Read，外部变化会要求重读；Write 要求完整读取，Edit 支持新鲜的部分读取。Edit 匹配与 Node 相同：精确匹配后依次尝试弯引号、Read 行号前缀、转义、Unicode 转义、首尾空白、缩进与首尾行锚点（中间行相似度 ≥ 0.8），CRLF 按多数行尾写回。读取观察缓存只在当前进程、当前 session 有效，冷恢复需重新读取。

Read 展示最多 64 KiB；修改文件最多 8 MiB；Grep 单文件最多 16 MiB，输出最多 20 KB；Glob 最多 100 项。搜索支持 ignore、正则、glob/type、大小写、上下文、多行与分页，耗时最多 30 秒。大文件/复杂方言的全量 TS 对齐尚未宣称完成。

Shell 每个流内联最多 24 KiB，完整输出落 tool-results；单流最多 16 MiB，超限停止进程。每会话最多 16 个运行后台任务、128 条进程内记录。前台默认 120 秒、显式 timeout 最多 600 秒；后台未指定 timeout 时运行至结束、显式停止或 Agent 退出。此包将任务状态附在下一次输入的模型上下文，不因任务完成自动发起新回合。Shell 输出文件在 `--data-dir` 的 `tool-results`。

工具 schema 从 TS 契约生成并有一致性测试；变更契约后运行 `node --import tsx scripts/generate-zcode-cli-rust-tool-schemas.mjs`。`examples/tool_fixture.rs` 仅供差分测试，不是发布入口。第二包 spec 为 `docs/specs/rust-coding-tools.md`。

## 上下文与维护队列

`contextWindow` 默认 200000，`maxOutputTokens` 默认 32000，`contextBufferTokens` 默认 13000，`autoCompact` 默认 true。输入阈值按当前 TS preflight 规则计算：窗口减去 min(输出上限,21000)，再减 buffer。配置应符合供应商的实际模型能力；不按模型名称猜测窗口。请求通过协议及 option map 携带输出上限；Registry 沿用 App 的正整数上限，不额外要求它小于上下文窗口。

App compact 与 `/compact [摘要侧重点]` 使用同一维护队列。普通聊天历史保留在 SQLite；模型只读取已提交摘要边界之后的历史。摘要流不会显示为正文。自动压缩保留最近完整工具轮次；失败、取消或摘要无法缩短上下文时明确停止，避免重复压缩。微压缩只清理请求投影中的旧成功工具结果，保留最近五项、错误及完整调用结构。

暂停队列时必须选择保留或清空再发；带 expectedHeldQueueItemIds 的旧确认会被拒绝。sendQueuedNow 保留原队列来源，提交预留后先取消并等旧 run 收口，再执行指定输入。显式 stop 释放预留。队列仍不跨进程恢复。

第三包规则见 `docs/specs/rust-context-management.md`，压缩与 Node 对齐的规则见 `docs/specs/rust-m7-compact.md`。目录级规则与摘要过长的分块压缩尚未对齐。

## 模型协议

`apiType` 省略时仍为 `openai-chat-completions`，也可选 `openai-responses` 或 `anthropic-messages`。Chat/Responses 的 baseUrl 分别追加 `/chat/completions`、`/responses`。Anthropic 与 TS adapter 一致：网关根地址先补 `/v1`（已有则保留），再追加 `/messages`；配置密钥时同时发送 x-api-key 和 Bearer Authorization，版本头为 2023-06-01。

Responses 使用 store=false，回传加密 reasoning items；Anthropic 回传 thinking 签名及 redacted_thinking，工具失败映射 is_error。这些元数据只保存在 canonical 历史并由对应协议使用，不进入 App 正文。所有协议复用原有取消、重试和提交屏障；截断/缺失终态的工具不会执行。

协议规则见 `docs/specs/rust-model-protocols.md`。动态 Registry、账号鉴权和模型切换已在 P0 补齐；provider 原生工具与 Node 一样只编码 Anthropic 原生 WebSearch；WebSocket 尚未实现。

输出达到模型上限时，三种协议均先提交已有 assistant 内容，再按 TS 规则最多续写三次；持续截断会返回 model_output_limit_exceeded。Continue 提示只用于当前执行，重启不自动续写，也不会作为用户输入保存。截断工具不执行，截断摘要不提交；续写继续经过上下文预算与压缩。规则见 `docs/specs/rust-output-continuation.md`。
