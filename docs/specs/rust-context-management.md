# Rust 上下文管理

状态：已实现；验收与性能结果见对应交付报告。属于完整对齐计划的第三包；权限仍仅 yolo，默认 runtime 仍为 TS。

## 规则与所有者

- Session actor 唯一拥有 canonical messages、上下文摘要边界、维护队列与时间线。完整 messages/rows 不被压缩删除；SQLite 元数据存储 `context.offset` 与 `context.summary`，与压缩完成时间线在同一事务提交。
- Agent loop 只使用当前边界之后的 messages，加一条标记为历史摘要的 user 消息及当前 system instructions。摘要不得提升为 system 权限。
- 手动 `compact` 和 `/compact [instructions]` 复用现有维护命令；忙碌或已有 held queue 时进入 FIFO，自动晋级仍由同一 actor 决定。维护输入不伪装成用户消息。重复 commandId 只返回 ACK，不重复压缩。
- 摘要流不投影为 assistant 正文；复用 `control.apiRetry` 显示重试，使用现有 timelineMarker compact running/success/failed/cancelled/noop。摘要失败或取消保留旧边界。存储失败停止执行，后续请求不得越过提交屏障。
- 模型配置新增 contextWindow（缺省 200000）、maxOutputTokens（缺省 32000）、contextBufferTokens（缺省 13000）、autoCompact（缺省 true）。沿用当前 TS preflight 预算：输入阈值 = contextWindow - min(maxOutputTokens,21000) - buffer。参数组合必须留出正输入预算。maxOutputTokens 同步约束请求输出。
- token estimate 由 Session 单一消息追加入口增量维护，冷恢复重建派生缓存；对齐当前 TS UTF-16 字符 / 3，计入 content、reasoning、工具名称和参数；加上工具定义与 system。供应商 prompt usage 在同一 run 的后续步骤作为锚点，加上新增消息估算。估算不是精确 tokenizer。
- threshold 附近先做 request-local microcompact：保留最近五条可清理工具结果；仅清理旧成功结果，保留 tool_call_id、消息结构及错误结果。原始正文留在 canonical 历史。触发阈值 min(autoThreshold\*0.9, autoThreshold-2000)，最少节省256估算token。
- 自动压缩保留最近 assistant 轮次及其后消息，分界不能留下悬空 tool_call；单段不可压缩或摘要后仍超出可用预算时返回可解释错误，不无限循环。context_exceeded 且未输出时允许一次反应式压缩重试，已有输出不重放。
- 每次模型请求前重新读取根目录 AGENTS.md，保持修改立即生效。目录级规则与完整 TS prompt 组装另行对齐。

```mermaid
sequenceDiagram
    participant Input as App / CommandInbox
    participant Owner as Session actor
    participant DB as SessionStore
    participant Loop as Agent loop
    participant Model as ModelPort
    Input->>Owner: compact / sendText
    Owner->>DB: input + ACK + maintenance turn
    DB-->>Owner: committed
    Owner->>Loop: start run (immutable context snapshot)
    Loop->>Model: summary request (no tools; hidden text)
    Model-->>Loop: validated summary
    Loop->>Owner: CompactDone + receipt
    Owner->>DB: boundary + marker + usage
    DB-->>Owner: committed
    Owner-->>Loop: receipt
    Loop->>Model: next request using committed boundary
```

Desktop continuous 与 Web replayable 使用相同 seq / snapshot / marker；摘要不另建协议。迟到事件仍以 sessionId + runId 隔离。重启使用最后提交边界，绝不重跑压缩或工具。

## 验收

- 自动预算/微压缩/中文估算/完整工具轮次分界单测。
- 真实 Rust 子进程 + App client/schema：手动压缩、busy FIFO、重复 ACK、取消和失败回滚、冷恢复边界、完整历史保留、AGENTS 动态更新、自动/反应式压缩。
- 可控 SessionStore fixture：压缩提交前不得发后续请求，事务失败不得采用新边界。
- Rust fmt/Clippy/tests、App integration、typecheck、lint、架构检查；性能基线另行比较。

## 边界

本包不宣称完整 TS context parity：多媒体、目录级 rules、摘要过长的多轮分块、模型长度续写仍需独立验收。App Registry/请求期鉴权遵守原契约，不把账号 Overlay 当作密钥配置。

## 同一维护队列的 App 操作补齐

- `sendQueuedNow` 在 actor 内预留完整 queue item（保留 sourceCommandId/client/model/mode/kind），提交 ACK 后取消当前 run；必须等旧 run Finished 才晋级。前台未收口时不能启动新请求，预留项不能编辑、删除或二次预留。显式 stop 释放预留，保留排队输入。
- held queue 支持 `keepQueueAndSend` / `clearQueueAndSend`；如提供 expectedHeldQueueItemIds，则校验无重复且集合完全相同，过期确认返回 guard.heldQueueConfirmationStale。未选择返回 heldQueueDispositionRequired。
- clear 的原输入 ACK 改为 failed/guard.queueDeleted，与新输入及其 ACK 在同一 SessionStore transaction 提交。失败立即结束 runtime，不执行新输入。
- compact 与普通输入共用 queue，compact 不允许通过 editQueueItem 改写成普通文本。所有变更继续投影给 desktop/mobile，迟到 stop 仍按 foregroundExecutionId 校验。
