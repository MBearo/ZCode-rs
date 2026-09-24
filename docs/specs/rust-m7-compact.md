# Rust M7：压缩质量

依据：Node `core/src/compact/{prompt,manual,policy,rounds}.ts`、`runtime/methods/{compact-active,compact,compact-active-helpers}.ts`、`runtime/helpers/{compact,compact-selection,compact-post-reminders}.ts`、`turn-loop.ts` / `turn-loop-state.ts`（快速回填）、`turn-model-step.ts`（反应式压缩）。架构见 `rust-p0-p1-architecture.md` §5.14；既有行为见 `rust-context-management.md`。

## 1. 所有者与边界

- 会话 Engine 拥有规范消息、摘要边界（`context.offset` / `context.summary`）、压缩时间线行，以及本进程内的“连续压缩失败次数”。
- run 任务在自己的历史副本上选轮、请求摘要，结果经 `CompactDone` / `CompactFailed` 交给 Engine 提交；提交回执之前不切换边界、不发下一次请求（沿用既有提交屏障）。
- 摘要文本存在 `context.summary`，请求时作为历史首条 user 消息发送。以 `This session is being continued from a previous conversation` 开头的摘要按原文发送（新摘要，以及从 TS 导入的摘要）；旧 Rust 摘要仍加原有前缀。

## 2. 摘要请求

- 消息：
  1. 与 agent step 相同的前缀，即 system prompt、AGENTS 指令、skills 与 profile 提醒、目标状态。前缀相同时 provider 缓存可以复用。
  2. 已有摘要（如果有）。
  3. 本次要总结的历史。
  4. 一条 user 消息，内容为 Node `buildCompactPrompt(customInstructions)` 的全文（NO_TOOLS 前导、九节结构说明、`Additional Instructions`、NO_TOOLS 结尾）。
- 工具：本 run 提供给模型的工具定义；超过 100 个时不发送工具（Node `COMPACT_TOOL_KEEP_MAX_COUNT`）。
- 输出上限：`min(模型常规输出上限, 20000)`（Node `MAX_OUTPUT_TOKENS_FOR_SUMMARY`）。
- 结果：
  - 返回工具调用时失败，文本为 `Tool use is not allowed during compaction`，不重试。
  - 文本按 Node `formatCompactSummary` 处理：去掉第一个 `<analysis>…</analysis>`，把第一个 `<summary>…</summary>` 换成 `Summary:\n<内容>`，连续空行合并，首尾去空白。
  - 处理后为空时失败，文本为 `Failed to generate compact summary`，可重试。
  - 输出因长度截断且文本为空，按上下文超限处理：失败文本为 `Conversation too long to compact automatically. Try /compact again after narrowing the active context.`，不重试。输出被截断但有文本时照常采用。
  - 差异：Rust 的模型输出不区分 `length` 与 Anthropic 的 `model_context_window_exceeded`，两者都按长度截断处理；Node 把后者当作上下文超限。
- 摘要消息：Node `buildCompactSummaryMessage(summary, {suppressFollowup: true})`：

  ```text
  This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.

  <处理后的摘要>
  Continue the conversation from where it left off without asking the user any further questions. Resume directly — do not acknowledge the summary, do not recap what was happening, do not preface with "I'll continue" or similar. Pick up the last task as if the break never happened.
  ```

## 3. 选轮

- 分组：按“assistant 开始新一组”划分（Node `groupByAssistantStartedRounds`）。已有摘要视为首条 user 消息，参与分组。
- 自动与反应式压缩保留最后一组，其余全部总结；手动压缩全部总结。
- 可压缩条件（Node `hasEnoughRuntimeEntriesToCompact`）：要总结的部分至少两组，且含 assistant 消息。
  - 手动压缩不满足时，时间线标记为 `noop`，边界不变。
  - 自动压缩不满足时不压缩，也不产生标记（Node 的决策 reason 为 `not_enough_messages`）。

## 4. 自动与反应式压缩

- 触发：
  - 自动压缩：每个 agent step 请求前，估算 token 不小于阈值时触发（阈值不变）。
  - 反应式压缩：请求以 `context_exceeded` 失败、且尚未流出输出时触发。每个模型步骤最多一次，工具批次完成后重新允许（Node `reactiveCompactAttemptedInCurrentModelStep`）。
- 同一次自动压缩内，可重试的失败最多尝试 3 次，时间线标记在重试期间保持 `running`。
- 失败不中断 run：
  - 自动压缩失败时，标记改为 `failed`，连续失败次数加一，本次请求照常发送。
  - 反应式压缩失败或不可压缩时，原来的 `context_exceeded` 失败照常上报。
  - 修复：旧实现把自动压缩失败直接作为 run 失败，压缩后仍超阈值时也会报错；Node 两种情况都继续发送请求。
- 熔断：会话在本进程内连续失败 3 次后跳过自动压缩（Node `MAX_CONSECUTIVE_AUTOCOMPACT_FAILURES`）。自动或反应式压缩成功后清零；手动压缩不影响计数，与 Node 相同。
- 快速回填（Node `evaluateRapidRefill`，按轮）：
  - 本轮成功压缩过之后，每个完成的工具批次计一轮。
  - 再次压缩时，如果距上次压缩不足 3 个工具轮，快速回填次数加一，否则清零。
  - 快速回填次数达到 3 时 run 失败，文本为 `Autocompact stopped because the context refilled within fewer than 3 tool turns after compaction 3 times in a row. A file or tool output may be too large. Read it in smaller chunks, or start a new session.`。

## 5. microcompact

- 与 Node 一致，默认关闭。Node 只能通过内部 runtime 配置开启，没有用户配置入口，因此 Rust 不再在阈值附近清理旧工具结果。
- 修复：旧实现默认开启，请求中的旧工具结果会被替换为 `[Old tool result content cleared]`，Node 不会这样做。

## 6. 摘要请求超长与压缩后提醒

### 6.1 摘要请求超长（Node `compact-selection.ts`）

- 摘要请求以 `context_exceeded` 失败时，从 provider 错误文本中解析超出的 token 数（`N tokens > M` 取 `N − M`），并按触发方式处理：
  - 自动与反应式压缩重新选轮：把更新的组移入保留区，直到被移出的组的估算 token 覆盖超出量；无法解析时移一组。移动的组数达到可总结组数减一时，改为移一半（至少一组）。移动后仍需满足可压缩条件。
  - 手动压缩截断：从最旧的组开始丢弃，直到丢弃的估算 token 覆盖超出量；无法解析时丢弃 20%（至少一组），至少保留一组。剩余部分以 assistant 开头时，前面加一条 user 消息 `[earlier conversation truncated for compaction retry]`。截断最多 3 次（与重选合计）。
- 都无法继续时失败，文本为 §2 的 `Conversation too long to compact automatically…`，不重试。
- 反应式压缩的首次选轮使用原请求的超出量：组数大于 3 且能解析超出量时，在最后一组之外再保留足以覆盖“超出量减去最后一组估算”的更新组（Node `selectCompactEntriesForInitialPromptTooLong`）。
- 同一次自动压缩的外层重试从初始选轮重新开始。

### 6.2 压缩后重新附带读取过的文件（Node `compact-post-reminders.ts`）

- 读取状态由工具按会话记录，Node 的 `readFileState` 每个 `(路径, offset, limit)` 一条：
  - 文本 Read 记录读到的原始内容与请求的 offset / limit。
  - Edit / Write 以 `(路径, 1, 无 limit)` 记录为非 Read 来源。
- 压缩成功后，按读取时间从新到旧挑选 Read 来源的记录：
  - 跳过路径含 `/.git/` 的记录，以及保留区里有 Read 调用读过同一 `file_path` 的记录。
  - 最多 5 条。单条估算（UTF-16 长度 / 3，向上取整）超过 5000，或累计会超过 50000 时，改为引用说明：`Note: <path> was read before the last conversation was summarized, but the contents are too large to include. Use Read tool if you need to access it.`。
  - 否则写入 `Called the Read tool with the following input: <{file_path, offset?, limit?}>\nResult of calling the Read tool:\n<带行号的内容>`。行号从 offset 开始（offset 为 0 时从 0，未给出或为 1 时从 1），格式为 `行号\t内容`。
- 这些提醒以 `<system-reminder>` 用户消息追加在保留区与计划文件提醒之后，写入会话消息（Node 的持久提醒来源 `resume_referenced_session_context`）。
- 之后清空该会话的读取状态（Node `readFileState.clear()`）：再次 Edit 前需要重新 Read。

## 7. 验收

- 单元测试：
  - 提示词全文与自定义指令。
  - `formatCompactSummary` 的各种输入。
  - 摘要消息。
  - 分组与选轮（含已有摘要）、可压缩条件。
  - 快速回填计数。
  - 旧摘要与新摘要的渲染。
- 集成测试（`zcode-cli-rust-context.test.ts`）：
  - 手动压缩的请求带 system 前缀、工具定义与 Node 提示词，下一轮历史首条为 Node 摘要消息。
  - 自动压缩保留最后一组。
  - 自动压缩失败后请求照常发送，标记为 `failed`。
  - 连续 3 次失败后不再尝试。
  - 摘要返回工具调用时失败。
  - microcompact 的代码已删除，由请求形状的断言覆盖（摘要请求与后续请求保留原始工具结果）。
  - 反应式压缩的摘要请求超长时，保留更多近期组后成功。
  - 手动压缩超长时截断最旧的组并加标记。
  - 压缩后下一次请求带最近 Read 的文件内容提醒，之后 Edit 需要重新 Read。
- 单元测试补充：超出量解析、重选与截断的组数、读取提醒的挑选与格式。
