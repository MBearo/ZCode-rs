# Rust 运行中输入：guide 与 startNow

## 产品合同与所有者

以当前 TS `core/src/runtime/methods/turn-guide-drain.ts`、`turn-stop.ts`、`steering.ts` 与 bootstrap `commands/handlers/session-flow.ts` 为准。Session actor 是 accepted input、队列、模型选择、canonical history 和 run generation 的唯一所有者。沿用现有 stdio V4，不增加协议版本；权限仍只支持 yolo。

- `setFollowupMode(queue|guide)` 改变会话默认 busy 路由，持久化并投影 `config.followupMode`、`inputRouting` 与 availability；显式单条 delivery 优先。空闲输入仍新建一轮。
- busy guide 不取消当前请求或工具。仅在完整 assistant 提交以及全部 tool results 提交后，每个模型 step 最多消费一条 guide，以真实 user 消息加入同一个 product turn。普通 queue 不阻塞 guide 子序列；guide 之间保持 admission FIFO。text-only 正常完成同样是可继续边界。
- guide 的冻结 model selection 在消费边界应用，下一步使用新选择；未消费前不得改写当前请求。正文使用 TS user_steer 提示，App user row 保持原文并带 `guided: true`、原始 sourceCommandId/clientId。
- guide 附件回退普通 queue，标记 `guide.attachmentsUnsupported`；中断、失败或无法继续时，未消费 guide 原地回退，保留顺序与来源，分别标记 `guide.turnInterrupted` / `guide.noToolBoundary`。正常 queue 不在 loop 内消费。guide admission ACK 保持 TS 的 queued admission (`delivery: queue`)。
- busy startNow 复用当前队列的唯一 queued-now reservation。先原子提交输入 ACK/预留，再取消旧 foreground；旧 run 的 Finished 提交后才提升新输入。ACK delivery 为 startNow。预留阶段不作为普通队列输入展示；拒绝第二个前台抢占，保留其他队列原顺序及 autoDrain。新 run 不能在旧工具退出前执行；旧 generation 的任何迟到事件均无效。
- stop 撤销尚未提升的 reservation，并让其成为可见 held queue；EOF/close 不能在收口时启动已预留输入。held queue 仍使用既有确认与 CAS，不因 startNow 绕过。

## 事件与失败顺序

```mermaid
sequenceDiagram
    participant App
    participant Owner as Session actor
    participant Store
    participant Loop as Agent loop
    App->>Owner: sendText / commandId
    Owner->>Store: accepted input ACK + session commit
    Store-->>Owner: committed
    Owner-->>App: ACK + projection
    alt guide
        Loop->>Owner: model + all tool result commit receipts
        Loop->>Owner: StepBoundary(runId)
        Owner->>Store: consume one guide + user row/history + selection
        Store-->>Owner: committed
        Owner-->>Loop: committed user message (or none)
        Loop->>Loop: next bound model request
    else startNow
        Owner->>Loop: cancel old run after reservation commit
        Loop-->>Owner: Finished after foreground tools exit
        Owner->>Store: interrupted turn commit
        Owner->>Store: promote reserved input commit
        Owner->>Loop: start new runId
    end
```

Store 任一失败立即终止 actor；不得回复消费 receipt、执行下一工具或发下一模型请求。边界握手不扫描/复制整个历史，只返回新增 user 消息；无 guide 时无额外数据库提交。同一 commandId 不重复消费。已接纳但尚未转录的输入仍 runtime-local；重启清空队列并把 ACK 标记 discardedOnRestart，不能把 startNow reservation 或 guide 当成已执行。已转录的 guide 通过持久 user row 证明消费，无自动重放。

Desktop continuous 与 Web replayable 复用同一 owner/deltas；本包通过双连接协议 fixture 验证投影，真实 App 验证默认引导与立即发送入口。完整手机/远程环境矩阵仍在后续清单。

## 验收

1. guide 在 text-only 和多工具完整批次后，同轮续跑；普通队列不挡 guide，连续 guide 一步一条，失败工具结果仍完整匹配。
2. guide 冻结选型、模式持久化、附件回退、stop 后 held queue、重复 commandId、冷恢复原始来源。
3. startNow 在流式正文及 Shell 期间抢占，旧内容保留、新旧工具不重叠，原 FIFO 随后继续；重复/竞争抢占、stop/EOF 与迟到事件不重复执行。
4. 可控制 Store fixture 验证 admission、guide 消费、旧轮终态和提升提交失败时无越界执行。
5. 真实 Rust 子进程经过现有 App client/schema；Rust tests/fmt/Clippy、App tests、typecheck、lint、fmt 和架构检查。保持 `CARGO_INCREMENTAL=0`，清理仅本次可再生缓存，保留导入备份与用户历史。
