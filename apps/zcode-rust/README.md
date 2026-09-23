# ZCode Rust 核心

无 TUI 的原生 Agent。实现 App 现有 stdio/V4 协议，依赖由 `Cargo.lock` 固定，SQLite 编译进二进制。Node 只用于现有 Electron App 和集成测试，Rust Agent 本身不调用 Node。

## 当前能力

- OpenAI Chat Completions、OpenAI Responses、Anthropic Messages；文本对话、SSE 正文/reasoning、分片 tool call、usage 与各协议推理元数据回传。
- HTTP 连接复用、有界重试、Retry-After、空响应恢复、闲置/显式总超时；App 显示重试等待，stop 可取消请求与退避。
- 首段即时交付，后续 16 ms/8 KiB 合并；reasoning 历史回传；可见输出后断流保留中断内容，不透明重放。
- yolo 自动执行；Read/Write/Edit 使用 TS 标准参数，Glob/Grep 原生搜索；后台 Bash、TaskOutput/TaskStop、输出文件、文件 diff 投影。List 仅兼容旧 native 调用，不再对模型公开。
- AskUserQuestion 复用 App 问答界面；支持单选、多选、自定义/部分回答、跳过、拒绝、自动继续与暂停倒计时。问题和回答先提交再唤醒工具；冷恢复保留已提交回答，未回答的问题标记中断。
- TodoRead/TodoWrite 保存会话任务清单，投影 App 工作计划摘要；支持全量替换/清空、旧 TS 清单导入、冷恢复与压缩后读取。进度提示按当前 TS 的十轮间隔注入，TodoWrite 保持顺序提交屏障。
- 会话创建、重命名、历史读取、FIFO 输入/compact 维护队列、队列编辑、held queue 保留/清空发送、sendQueuedNow、stop 和幂等 ACK 查询。
- 手动 /compact、自动预算压缩、超限后的单次反应式压缩、旧工具结果 microcompact；摘要边界与时间线同事务保存，完整历史保留；每次请求刷新根 AGENTS.md。
- 会话 SQLite 持久化、崩溃中断恢复、workspace identity 隔离、进程 owner 锁、旧 run 事件丢弃。
- V4 conversation/sessions-index/workspace-config 投影、desktop/mobile 独立订阅、分片校验和 snapshot 恢复。
- App 原生存储准备、能力协商和显式 runtime 选择。
- 直接读取 App Provider Registry、个人设置及账号 overlay；热更新、模型/档位切换、每请求 Host 鉴权、连通性测试和 workspace 文本生成/取消。
- 当前 TS SQLite 的只读备份与幂等导入，保留身份、消息/工具、压缩边界和输入处置；附件独立快照及已有分块预览接口。
- 模型/工具结果及后台登记提交屏障；Read/List/Glob/Grep 最多四并发，写入/Shell 顺序执行；存储失败停止执行且禁止收口再次提交失败状态。

尚未替换默认 TypeScript runtime。不支持 MCP/插件、子代理、Plan 执行、context refs、Node REPL 和工作流；未支持的命令明确拒绝。Todo 工作计划不启用 Plan 执行开关。Composer 附件已支持分片上传与文本/图片/PDF 基础链路，剩余媒体和容量边界见[对齐清单](../../docs/specs/rust-parity-remaining.md)。部分高级 UI 入口尚未隐藏。重连通过新 snapshot 恢复，不承诺增量日志 replay。Shell 使用 yolo 权限，不提供 OS sandbox。

## 构建与验证

仓库根目录执行。Rust >= 1.89，当前验证工具链为 1.95.0；Node/pnpm 依 `mise.toml`。

```sh
pnpm build:rust-agent
pnpm check:rust-agent
pnpm test:rust-agent
pnpm typecheck
pnpm lint
```

集成测试启动真实 Rust 二进制、临时 SQLite、临时工作区和本地 HTTP 模型 fixture，使用 App 的实际协议客户端、Host 服务与 V4 schema/assembler。不会请求真实模型或读取个人账号。验证记录见 [spec](../../docs/specs/rust-agent-core.md)。

macOS/Linux 的 TLS 证书失败测试使用本地 `openssl` 生成临时自签名证书，测试后删除；Windows 暂跳过这一用例。Rust 单测及受控存储测试不依赖外部服务。

性能比较使用 release 产物：

```sh
cargo build --locked --release --manifest-path apps/zcode-rust/Cargo.toml
node scripts/bench-rust-agent-suite.mjs /absolute/baseline-binary apps/zcode-rust/target/release/zcode-rust .zcode-runtime/rust-bench/requests 256000 256000
```

该脚本串行、交替比较两个版本，在固定流式、100 轮历史和四会话场景各重复五次；记录原始样本及 summary.json。RSS 是定时采样值；存储开销同时记录运行中 SQLite/SHM/WAL 占用和正常 EOF 后持久文件大小，不等于累计物理写入字节。结果目录默认 `.zcode-runtime/rust-bench/requests`。

`requestTimeoutSeconds` 仅在显式配置时限制单次 HTTP 尝试总时长；省略表示不设固定总上限。`streamIdleTimeoutMs` 默认 600000，重试每次增加 30000；设为 0 可禁用闲置超时。重试配置可通过可选 `retry` 对象设置 `maxRetries`、`baseDelayMs`、`backoffFactor`、`maxDelayMs`、`jitter`；未提供的字段沿用 `ZCODE_MODEL_RETRY_*` 环境变量及 CLI 默认值（10 次、2 秒、因子 2、60 秒、启用 jitter）。该预算仅允许在尚未交付可见输出时重试，空响应最多重试一次。

## 接入 App

通常直接使用现有 App 设置，无需 Rust 模型 JSON：

```sh
pnpm dev:desktop:rust
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
pnpm dev:desktop:rust --config /absolute/path/model.json --data-dir /absolute/path/rust-experiment
```

带 `--config` 时使用显式单模型模式，App 需有对应选型供其就绪门禁使用。执行配置以 JSON 为准；该模式不消费账号 overlay。

静态模式的 `reasoningParameters` 按协议配置：Chat 接受 reasoning_effort/thinking/enable_thinking，Responses 接受 reasoning 或 reasoning_effort，Anthropic 接受 thinking。Registry 模式自动使用 App option maps。模型切换先提交，下一个模型步骤生效；排队输入保留接收时的选型。默认选型来自 App `defaultModelSelection`，恢复会话保留自身选择。跨 provider/model 不回放私有推理签名，正文和工具关联保留。

手工集成可设置 `ZCODE_AGENT_SERVER_RUNTIME=rust-core`、`ZCODE_AGENT_SERVER_COMMAND=<binary>`、`ZCODE_AGENT_SERVER_ARGS_JSON=["app-server","--stdio","--data-dir","...","--config","..."]`。Host 自动补充 `--cwd` 与 `--surface desktop`；不要在 args 中另传 `--cwd`。不设置这些变量仍使用原有 TS runtime。

直接使用协议入口：

```sh
apps/zcode-rust/target/debug/zcode-rust app-server --stdio \
  --cwd /absolute/workspace --data-dir /absolute/isolated-data \
  --config /absolute/path/model.json
```

stdout 仅输出 NDJSON；stderr 为诊断。`--prepare-storage` 使用原 Host 握手，不调用模型。未提供 Registry 文件路径或显式 config 时，只读原生历史。运行队列不跨进程恢复，command query 明确标记未执行输入被丢弃。

Rust 默认写入 `~/.zcode/rust/rust-sessions.sqlite`，可用 `ZCODE_RUST_DATA_DIR` 或 `--data-dir` 修改。首次打开 workspace 时只读导入当前 TS 库；源路径按 `--import-ts-db`、既有 SESSION_DB 环境、用户/项目 storage 配置、默认路径解析。显式不存在的源会失败；旧 schema 须先经 TS 自身迁移，Rust 不原地升级它。备份为 `ts-backup-*.sqlite`，新导入附件快照位于 `ts-import-<UUID>/imported-attachments`，旧 `imported-attachments` 继续可读。工作区导入与完成标记原子提交；失败/取消回收本次文件，重启回收无提交引用的中断目录，成功备份保留。启动导入支持 SIGTERM、Ctrl-C、EOF 和已检测到的 stdout 断管取消，详见 `docs/specs/rust-import-lifecycle.md`。未知语义、缺失本地附件和不支持的 MIME 明确报错；远端 URL 保留，本包不离线下载远端资源。

回退时退出 Rust 并取消 runtime override，TS 继续读原库。Rust 新增历史保留在独立库，不反向同步。P0 实现与验收见 [spec](../../docs/specs/rust-app-p0.md) 和 [报告](../../docs/reports/rust-app-p0.md)。

当前集成验证在 macOS 完成；真实 GLM-5.3 与 Electron Renderer 的基础对话、工具、输入、附件和问答已有[实机证据](../../docs/specs/rust-parity-remaining.md)。完整供应商/交互矩阵、Windows/Linux 实机及发布打包仍待验证。

## Coding 工具与后台执行

新会话 mode=yolo；原生旧数据缺少 mode 时仍视为 build，历史可读，通过现有 switchCollaborationMode(yolo) 显式切换后才能继续。后台任务绑定 session，正常前台结束后继续运行；TaskOutput 可查询或等待，TaskStop 和 App cancelBackgroundWork 可停止。stop 和 EOF 收回任务进程树。冷恢复保留任务记录/输出文件、标记中断，不重启未知副作用。

Read 使用 file_path/offset/limit；Write 使用 file_path/content；Edit 使用 file_path/old_string/new_string/replace_all。相对路径按 cwd 解析，也允许显式访问工作区外路径（与 TS yolo 一致）。已有文件必须先 Read，外部变化会要求重读；Write 要求完整读取，Edit 支持新鲜的部分读取。当前编辑匹配为精确匹配及 CRLF 归一化，TS 其它宽松匹配策略仍待补齐。读取观察缓存只在当前进程、当前 session 有效，冷恢复需重新读取。

Read 展示最多 64 KiB；修改文件最多 8 MiB；Grep 单文件最多 16 MiB，输出最多 20 KB；Glob 最多 100 项。搜索支持 ignore、正则、glob/type、大小写、上下文、多行与分页，耗时最多 30 秒。大文件/复杂方言的全量 TS 对齐尚未宣称完成。

Shell 每个流内联最多 24 KiB，完整输出落 tool-results；单流最多 16 MiB，超限停止进程。每会话最多 16 个运行后台任务、128 条进程内记录。前台默认 120 秒、显式 timeout 最多 600 秒；后台未指定 timeout 时运行至结束、显式停止或 Agent 退出。此包将任务状态附在下一次输入的模型上下文，不因任务完成自动发起新回合。输出产物保留在独立 Rust 数据目录。

工具 schema 从 TS 契约生成并有一致性测试；变更契约后运行 `node --import tsx scripts/generate-rust-tool-schemas.mjs`。`examples/tool_fixture.rs` 仅供差分测试，不是发布入口。第二包 spec 为 `docs/specs/rust-coding-tools.md`。

## 上下文与维护队列

`contextWindow` 默认 200000，`maxOutputTokens` 默认 32000，`contextBufferTokens` 默认 13000，`autoCompact` 默认 true。输入阈值按当前 TS preflight 规则计算：窗口减去 min(输出上限,21000)，再减 buffer。配置应符合供应商的实际模型能力；不按模型名称猜测窗口。请求通过协议及 option map 携带输出上限；Registry 沿用 App 的正整数上限，不额外要求它小于上下文窗口。

App compact 与 `/compact [摘要侧重点]` 使用同一维护队列。普通聊天历史保留在 SQLite；模型只读取已提交摘要边界之后的历史。摘要流不会显示为正文。自动压缩保留最近完整工具轮次；失败、取消或摘要无法缩短上下文时明确停止，避免重复压缩。微压缩只清理请求投影中的旧成功工具结果，保留最近五项、错误及完整调用结构。

暂停队列时必须选择保留或清空再发；带 expectedHeldQueueItemIds 的旧确认会被拒绝。sendQueuedNow 保留原队列来源，提交预留后先取消并等旧 run 收口，再执行指定输入。显式 stop 释放预留。队列仍不跨进程恢复。

第三包规则见 `docs/specs/rust-context-management.md`。目录级规则、新多媒体输入、摘要过长的分块压缩尚未对齐；P0 导入支持旧文本、图片和 PDF 附件。

## 模型协议

`apiType` 省略时仍为 `openai-chat-completions`，也可选 `openai-responses` 或 `anthropic-messages`。Chat/Responses 的 baseUrl 分别追加 `/chat/completions`、`/responses`。Anthropic 与 TS adapter 一致：网关根地址先补 `/v1`（已有则保留），再追加 `/messages`；配置密钥时同时发送 x-api-key 和 Bearer Authorization，版本头为 2023-06-01。

Responses 使用 store=false，回传加密 reasoning items；Anthropic 回传 thinking 签名及 redacted_thinking，工具失败映射 is_error。这些元数据只保存在 canonical 历史并由对应协议使用，不进入 App 正文。所有协议复用原有取消、重试和提交屏障；截断/缺失终态的工具不会执行。

协议规则见 `docs/specs/rust-model-protocols.md`。动态 Registry、账号鉴权和模型切换已在 P0 补齐；provider hosted tools、新媒体输入和 WebSocket 尚未实现。

输出达到模型上限时，三种协议均先提交已有 assistant 内容，再按 TS 规则最多续写三次；持续截断会返回 model_output_limit_exceeded。Continue 提示只用于当前执行，重启不自动续写，也不会作为用户输入保存。截断工具不执行，截断摘要不提交；续写继续经过上下文预算与压缩。规则见 `docs/specs/rust-output-continuation.md`。
