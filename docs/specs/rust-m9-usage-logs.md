# Rust M9：用量与日志保留

依据：`apps/zcode-cli/packages/adapters/src/logging/retention.ts`、`bootstrap/src/log-retention.ts`；架构见 `rust-p0-p1-architecture.md` §5.16–5.17。用量见第 2 节。

## 1. 日志保留（M9.1）

- 目录：`ZCODE_LOG_DIR`，缺省为 `~/.zcode/cli/log`（与写入相同）。
- 文件：只处理本 runtime 的日文件 `zcode-rust-YYYY-MM-DD.jsonl`；名称必须严格为 4-2-2 位数字且是合法日期（例如 `2026-02-30` 不处理）。Node 的 `zcode-YYYY-MM-DD.jsonl` 由 Node 自己清理，Rust 不触碰。
- 规则：保留 7 天。截止日期为本地日期 `今天 - 7 + 1`，日期早于截止日期的文件删除；目录不存在视为完成。
- 时机：app-server 启动 60 秒后执行一次，后台任务，不阻塞请求与退出。
- 日志：调度时 `info`（`log.retention.cleanup.scheduled`），完成时 `debug`（`log.retention.cleanup.completed`，含扫描、删除与失败数量），读取目录或删除失败时 `warn`（`log.retention.cleanup.failed` / `log.retention.delete.failed`，只记录文件名与错误类别，不记录路径以外的内容）。
- 验收：单元测试覆盖截止日期边界、非法与他人文件名、删除失败不影响其余文件。

## 2. 用量（M9.2）

依据：Node `adapters/src/storage/session-store/repositories/usage.ts`（表、upsert、30 天保留、两类查询）、`bootstrap/src/zcode-protocol/usage-stats-builder.ts`（快照）、`server-operations.ts` 的 `getUsageStats` / `getTaskTokenUsage`、`core/src/runtime/methods/usage-observability.ts`、`turn-tool-usage.ts`、`turn.ts` / `compact.ts`（记录时机）。

### 2.1 状态所有者与事件顺序

- 事实由 Engine（唯一所有者）从 run 事件推导：模型请求来自 `ModelStatus`、`Text`、`ModelDone`，工具调用来自 `ToolStart`、`Permission`、`ToolExecuting`、`ToolDone`，轮次来自 run 开始与 `Finished`。run 任务与模型层不接触存储。
- 每个 run 的进行中状态（请求探针、工具探针、轮次累计）挂在 `Active` 上，随 run 结束丢弃。取消后被丢弃的 run 事件仍先经过用量观察，终态才能记为 cancelled。
- 写入：Engine 把事实发给存储 worker（与会话提交同一有序队列，不等待写入结果）。写入失败只记 `warn`（`usage.write.failed`），不影响会话（Node：观测失败不能改变 Agent 业务语义）。
- 查询：Engine 同步校验参数，之后在后台任务中执行。先经过存储 worker 的屏障，保证本进程此前发出的事实已经落库；再用请求级只读连接做 SQL 聚合，按原请求 token 回复。Engine 不因查询阻塞。

```mermaid
sequenceDiagram
  participant Run as run 任务 / 模型层
  participant E as Engine（所有者）
  participant W as 存储 worker（唯一写者）
  participant R as 只读连接（每个查询）
  Run->>E: ModelStatus started（attempt 1）
  Run->>E: Text（首个增量）
  Run->>E: ModelStatus retry / completed
  Run->>E: ModelDone
  E->>W: 模型事实（不等待）
  Run->>E: ToolStart … ToolDone
  E->>W: 工具事实（running，之后终态）
  Run->>E: Finished
  E->>W: 轮次事实
  Note over E: v4/usage/stats 请求
  E->>W: 屏障
  W-->>E: 此前的事实已处理
  E->>R: SQL 聚合（后台任务）
  R-->>E: 聚合行
  E-->>E: 快照构造后回复原 token
```

### 2.2 表与保留

- Rust 库新增 `rust_model_usage`、`rust_turn_usage`、`rust_tool_usage`，列、检查约束、主键与索引同 Node 迁移 `0010_usage_observability`。不加会话外键：Rust 会话以 `(workspace, id)` 为键，且只删除没有历史的草稿，不会产生孤儿用量。
- upsert 规则同 Node：模型请求按 id 整行覆盖；轮次按 `(session_id, turn_id)` 合并（started_at 取小，首个时刻保留，终态字段覆盖）；工具按 id 合并（终态不被 running 覆盖，字节取大，错误字段保留首个非空值）。
- 保留 30 天：按 started_at 删除三表中早于 `now - 30 天` 的行。Node 每次写入后执行；Rust 在写入时最多每分钟执行一次，过期行最多多保留 1 分钟。

### 2.3 记录规则（M9.3 按 Node 实测对齐）

对照依据是 `scripts/zcode-cli-rust-usage-diff.mjs`（§3）：同一本地模型下真实 Node CLI 与 Rust 跑相同场景，三张表的行归一化 id 与时刻后逐字段比较。以下规则都来自 Node 源码并经该脚本核对。

**模型请求（`model_usage`，Node `recordModelUsageFact`）**

- 记录的来源：`main_turn`（根会话的 agent step）、`subagent`（子会话的 step）、`compact`（手动与自动压缩摘要）、`target_completion_verification`、`session_title`、`goal_summary_title`。WebSearch（`web_search_tool`）、WebFetch 处理（`web_fetch_processing`）与连通性测试不记录，Node 同样不记录。
- 一个逻辑请求从 attempt 1 的 `model_request_started` 开始，每个 `model_retry_scheduled` 使 retry_count 加 1。
- 身份：
  - step 请求：`assistant_message_id` 为该步骤的 Node assistant 消息 id，`parent_user_message_id` 为本轮的 user 消息 id，`logical_request_id` 取 assistant 消息 id。
  - 其他来源：`logical_request_id` 取该请求的 span id；标题请求的 `parent_user_message_id` 为触发标题的 user 消息 id，压缩与目标验证为空。
  - `id` 为 `usage_model_<来源>_<logical_request_id>_<attempt_index>`；`attempt_index` 为压缩在 prompt-too-long 之后的重试序号，其余为 0。
  - `span_id`：每个逻辑请求一个 Node 格式的 span id（`randomUUID().slice(0, 16)`）。
- 终态：
  - `model_request_completed`：status 为 completed。token 取 Node `ModelUsage`（`raw_usage_json` 与 `provider_total_tokens` 都用这一归一化形态，标题请求也一样），finish_reason 取归一化的 finishReason，`provider_metadata_json` 为 `{rawFinishReason}`（供应商原始结束原因）。step 请求在随后的 `ModelDone` 落库，tool_call_count 为该 assistant 消息的工具调用数；其他来源立即落库，tool_call_count 为 0。
  - `retryable` 为 false 的 `model_request_failed`：reason 为 `cancelled` 时 status 为 cancelled（cancelled_by_user 为 1），否则为 error。error_type 取 reason，error_code 为空（Node 的模型适配器错误不是 CoreError，没有 code），error_message 为该 reason 的通用失败文案（Node 适配器错误的 message，例如 `Provider rejected the model request.`），不是供应商原文。reason 为 `context_exceeded` 时 context_exceeded 为 1。
  - run 结束时仍未终结的请求：run 被取消时记 cancelled，否则记 error。
- 时刻：started_at 为 Engine 收到 attempt 1 开始状态的时刻。first_token_at 为该请求首个非空文本或推理增量的时刻；只有 step 请求有流式增量，其他来源为空（与 Node 相同）。
- token：computed_total_tokens 为输入侧加 outputTokens，其中输入侧在 inputTokens 大于 0 时取 inputTokens，否则取 cache 读写之和。retryable 为 retry_count 大于 0。
- 归属：provider_id、model_id 取请求开始时的模型快照；variant 为会话当前的推理档位；mode 为会话权限模式；agent 为 `zcode-agent`，子会话为 `zcode-<子代理类型>`（Node 子代理 runtime 的 `agentName`）；task_type 根会话为 `interactive`，子会话为 `subagent_child`；turn_id、trace_id 为 run 的轮次与 trace，标题请求取触发它的轮次。

**轮次（`turn_usage`，Node `recordTurnUsageFact`）**

- 记录时机（Node `turn.ts`、`compact.ts`）：普通输入与目标续跑的 run 在成功、失败、取消时都记录；UserPromptSubmit hook 拦截的输入按成功记录；手动压缩的 run 在每种结局都记录。
- 字段：
  - `user_message_id` 为本轮 user 消息 id，手动压缩为空。
  - started_at 为 run 开始时刻，completed_at 与 duration_ms 取结束时刻。
  - model_request_count 与 model_retry_count 为本轮事件中的模型请求数与重试数：agent step 与轮内压缩计入，目标验证（自有事件列表）、标题（旁路）与 WebSearch 的内部请求不计入。
  - tool_call_count 为调度的工具调用数。tool_error_count 只数本轮事件里的 ToolCallError：Node 执行器的工具事件不进入轮次事件列表，只有 run 结束时合成的中断调用计入（Rust 为 run 结束时仍未完成的调用），普通失败与拒绝不计。
  - token 为本轮所有 ModelComplete 的 usage 之和：agent step、轮内压缩，以及工具内部请求的嵌套用量（§2.3.1）；目标验证不计入。computed_total_tokens 按 Node `getModelUsageTotalTokens` 累加。
  - 失败信息同 Node `createTurnFailureError`：取消为 `turn_cancelled` / `TURN_CANCELLED`（cancelled_by_user 为 1）；超出上下文为 `model_context_exceeded` / `MODEL_CONTEXT_EXCEEDED`、retryable 为 1、context_exceeded 为 1；其他失败为 `unknown_error` / `UNKNOWN_ERROR`。

**工具（`tool_usage`，Node `recordToolUsageFromEvent` 与 `recordToolUsageFromResult`，经同一 upsert 合并）**

- id 为 `usage_tool_<session>_<callId>`。
- `side_effect_scope`、`read_only`、`destructive` 取 Node 工具注册元数据：Read、Glob、Grep、TaskOutput、TodoRead 为 `none`/只读；Write、Edit 为 `workspace`；Bash 为 `system`；WebFetch、WebSearch 为 `network`/只读；TodoWrite、Skill、Agent 为 `session`/只读；SendMessage、TaskStop、EnterPlanMode、ExitPlanMode 为 `session`；AskUserQuestion 为 `userInteraction`/只读；MCP 工具为 `network`（宿主 `node_repl` 的 `js` 为 `system`），只读与破坏性取 annotations 的 `readOnlyHint === true`、`destructiveHint === true`。
- 状态：调度时 running；成功为 completed，失败与拒绝为 error，run 取消导致的失败为 cancelled（cancelled_by_user 为 1）。run 结束时仍未完成的调用按 run 的结局收口。
- approval_status：权限事件依次写 `requested`、`allowed` / `denied`，但 Node 的结果记录器最后写入 `none`，upsert 的 coalesce 让它覆盖前值；有结果的调用最终都是 `none`（照 Node 保留）。
- 字段：
  - started_at 为调度时刻；duration_ms 为执行时长，被拒绝的调用从调度算起（Node 结果记录器总有时长）。
  - first_output_at 为首次输出时刻：Bash 输出的首个增量，其他工具为完成时刻；time_to_first_output_ms 为完成时刻减执行开始时刻（Node 结果记录器）。
  - exit_code 为 Bash 的退出码，其他工具为 0（Node 结果事件对缺失的退出码写 0）。
  - output_bytes 为结果文本的字节数，失败与拒绝为 0（没有序列化输出）；truncated 为结果是否被预算截断。
  - 失败时 error_type 为 Node 的错误类型（`tool_execution_failed`、`tool_cancelled`、`permission_denied`），error_code 为 CoreError 的大写类型（`TOOL_EXECUTION_FAILED`、`TOOL_CANCELLED`），权限拒绝没有 code；error_message 为错误文本。

#### 2.3.1 工具内部请求的嵌套用量（Node `appendNestedToolModelUsage`）

- 工具结果带 `modelUsage` 时（WebSearch 的内部模型请求），Node 在本轮追加一条 `ModelComplete {stopReason: "tool_internal"}`。Rust 在工具结果提交时把这份用量计入：
  - 本轮 `turn_usage` 的 token（不计入 model_request_count）；
  - 旧协议 `turn.completed` 的 usage，其中 `webSearchRequests`、`webFetchRequests` 累加 `serverToolUse`；
  - 目标的 `tokensUsed`（Node `accountTargetTurnCompletion` 取本轮 usage 的 totalTokens）；
  - 子代理返回给父会话的用量（Node `aggregateModelUsage` 汇总子会话本次运行的全部 ModelComplete）。
- 不计入 `model_usage`（所以不进入 `v4/conversation/usage` 与 `v4/usage/stats`），也不计入 v4 `usage` 状态与上下文用量，与 Node 相同。
- WebSearch 输出的 `modelUsage` 为内部请求的 Node `ModelUsage`（input/output/total、cache 读写、reasoning、`serverToolUse.webSearchRequests/webFetchRequests`），并带顶层 `webSearchRequests`。

#### 2.3.2 标题的时机

- app-server（Node 协议模式）：Node 的 `providerRuntimeHeadersPort.shouldRefreshBeforeModelRequest` 恒为 true，首条输入的标题总是推迟到该轮成功结束后生成；失败或取消的首轮不生成，推迟的标题也随之丢弃，之后第一条成功的轮次生成。Rust 原先只对账号类供应商推迟，现与 Node 相同。
- `turnNumber === 0` 的判定：本次激活之前已存的输入（加载时的历史）加上本次激活成功结束的 run（压缩除外）为 0；本次激活开始的输入与 `/goal` 目标不计入（原先按消息里的输入条数判断，失败的首轮会让之后永远不生成标题）。
- 标题用量的 turn_id、trace_id 为触发它的 run。

### 2.4 查询

**`v4/usage/stats` 与旧 `usage/stats`（同一实现）**

- 参数：严格对象 `{range: "all" | "7d" | "30d", timeZone?: string}`，缺省 params 视为 `{}`。校验失败为 Node `parseParams` 的 -32602，包括消息与 ZodError data。
- 时间范围：until 为当前时刻；`7d`、`30d` 的 since 为 until 减去对应天数，`all` 为 0。
- 时区：缺省为 `UTC`。IANA 名称不区分大小写，含别名，在 until 时刻按秒精度计算偏移；`±HH:MM` 为固定偏移。无法解析时偏移为 0（Node `resolveTzOffsetMs` 捕获异常后回退 0）。
- 聚合与快照：
  - SQL 聚合同 Node `queryAppUsage`：总计、轮次总计、最长会话、工具总计、模型排行、工具排行、按日的 token / 轮次 / 工具数，以及按日的模型用量。
  - 快照构造同 Node `buildAppUsageSnapshot`：缓存命中率的分母、错误率、活跃天数、当前与最长连续天数、热力图 5 级与按 7 天切周、每日模型趋势、模型份额与最常用模型。
  - 平均值与比例按 JS number 输出。
- 范围：查询覆盖整个 Rust 库（所有 workspace），与 Node 用任意 workspace client 读取全局库一致。

**`v4/conversation/usage` 与旧 `session/usage`**

- 参数：严格对象 `{sessionId}`，两个方法都按 Node 服务端实际使用的 `zcodeTaskTokenUsageParamsSchema` 去空白后非空。
- 计算：按 started_at、id 升序读取该会话的模型请求，规则同 Node `queryTaskUsage`：
  - main_turn、subagent、workflow_child 三个来源的输入侧按各自基线取增量，压缩后基线随之下降；其他来源全额计入。
  - 输入侧口径同 Node `inputSideTokensFromStoredUsage`。
  - cache 读写只累加非基线来源，并返回 `inputBaselineBySource`。
- 未知会话返回全 0。

### 2.5 与 Node 共用的用量表

- 用量写入 Node 库的 `model_usage`、`turn_usage`、`tool_usage`（spec rust-m11-node-storage §2.1），Node 与 Rust 写入的行在同一统计中；`queryAppUsage` 与 Node 一样不按工作区过滤。

### 2.6 与 Node 的差异

- 保留清理最多每分钟一次（见 2.2）。
- `workspace/generateText` 不记录用量：Node 记到工作区的活动会话，没有活动会话时因会话外键写入失败；Rust 不把 workspace 级请求归到会话。连通性测试两边都不记录。
- stdout / stderr 字节只在 Node 的 Bash 进度事件里有值，Rust 不单独统计，记 0。
- 标题请求 Node 走非流式 `generateText`，`provider_metadata_json` 是 AI SDK 的供应商元数据（例如 `{anthropic: {usage, cacheCreationInputTokens, …}}`）；Rust 以流式请求生成标题，记 `{rawFinishReason}`。
- 时刻取 Engine 收到事件的时间，Node 取事件对象的时间戳；两者只差进程内的通道延迟。

### 2.7 修复

- 目标完成验证原先复用压缩的隐藏请求，请求来源为 `compact`；现改为 Node 的 `target_completion_verification`，网络状态、`session/debug` 与用量的归属都与 Node 一致。
- 隐藏请求（压缩、目标验证、WebSearch）在请求结束时直接返回，通道里尚未转发的 completed 状态被丢弃，压缩用量因此记为 error、token 为 0，轮次 token 缺少压缩部分。现在请求结束后先转发剩余状态。

### 2.8 验收

- 单元测试：
  - 快照构造：连续天数、热力图级别与补齐、时区偏移、份额。
  - 会话用量的基线计算。
  - 事实推导：重试、失败、取消，step 请求在 `ModelDone` 落库。
  - 表的 upsert 与查询。
- 集成测试（`zcode-cli-rust-usage.test.ts`）：
  - 一轮带工具调用后，`v4/conversation/usage` 与 `session/usage` 的 token。
  - `v4/usage/stats` 的 summary、模型与工具排行、热力图。
  - 参数错误。
  - Node 写入的用量行出现在统计中。

## 3. 与 Node 的差分检查（M9.3）

`TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.zcode-cli-rust.json node --import tsx scripts/zcode-cli-rust-usage-diff.mjs [zcode.cjs] [rust] [--only=…] [--show=…]`：

- 本地 Anthropic Messages 模型（流式与非流式、cache 读写与 `server_tool_use` 用量）；共用 Provider Registry 形态的 HOME（`scripts/zcode-cli-rust-interop-runtime.mjs`）。
- 场景：普通轮、Bash 工具轮、WebSearch（内部搜索请求）、模型 400 失败、流式中停止、手动压缩、子代理。每个场景一个新会话，Node 与 Rust 各用一个临时库。
- 比较：三张表的行按记录对象（来源或工具名）排序后逐字段比较；会话、轮次、消息、trace、span、调用 id 按首次出现替换为占位，时刻与时长只比较是否为空，`*_json` 解析后比较。
- 同时收集两边的 `v4/telemetry/event`（§4 对齐前只列出种类）。
- 已知差异（脚本列出、不计数）：标题的 `provider_metadata_json`（§2.6）；工具失败文本里的工作目录（Rust 把工作区 realpath 化，单列对齐项）。
- 场景：普通轮、Bash、WebSearch、模型失败、停止、手动压缩、子代理、工具失败（Read 不存在的文件）、build 模式下拒绝权限。2026-09-25 的结果：9 个场景三张表逐字段一致（差异 0）。
