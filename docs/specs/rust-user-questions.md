# Rust AskUserQuestion 与 App 问答

2026-09-22。对齐当前 TS contracts/tools/ask-user-question.ts、core handler、interaction-broker 与 V4InteractionRegistry，权限仍仅 yolo。AskUserQuestion 是澄清交互，不能被 yolo 自动批准。

## 行为与边界

- 暴露当前 TS 生成的工具 schema 和描述；严格校验 1–4 题、每题 2–4 个唯一选项、唯一问题、禁止显式 Other、预览 HTML fragment 约束。兼容省略 multiSelect=false。模型传入 answers 不等于用户已回答。
- 使用既有 `pendingInteractions.kind=userInput`、toolCall pendingApproval、resolveInteraction、snoozeInteractionAutoResolution 与 workspace/updateInteractionPreferences；无新协议版本，无 App 内部状态旁路。sessions-index 分别计数并携带轻量工具身份。
- 接受多题/多选、自由文本、部分回答、显式空 answers 跳过、预览/备注，以及既有 answer_N/单题 answer/freeText 兼容路径。仅标准化的已知问题答案传给模型；未知字段不进入工具输入。decline/cancel 是工具拒绝；stop/startNow/EOF/close 是执行取消，不能误当全部跳过。
- Session actor 是交互、计时资格、答案和 ACK 的唯一所有者；loop 只等待 run-scoped 回执。不另建 accepted queue。最多四个明确安全工具并发，交互按注册顺序投影，每个 session 只有队首倒计时，结果仍按模型 call 顺序提交。
- 默认自动继续启用：队首先隐藏 60 秒，再可见倒计时，到注册激活后 300 秒以空 answers 继续。首次有效操作永久 snooze；关闭设置使当前计时持久化为 snoozed、所有已注册问题失去计时资格；重新开启只影响后续新问题。截止由 actor 时钟和下一个 deadline 驱动，无逐题轮询任务。测试缩放仅在 ZCODE_ENV=test 下沿用 TS 的环境变量和 1–1000 校验。
- 注册、计时阶段、答案和 ACK 都在发布/唤醒前提交。答案写入当前工具行的规范化输入和输出，之后 loop 按 call 顺序提交 canonical tool 结果并等待回执；下一模型请求不能越过任何提交失败。若崩溃发生在答案提交后、canonical tool 提交前，冷恢复从已提交的问题工具行补齐真实结果，不能丢失答案或重新提问。未回答问题恢复为中断，不恢复活 waiter/计时器或重跑工具。
- 多端先到先得；重复/迟到/旧 run 应答无害 noop，不影响新交互；已关闭 session 的迟到 interaction 命令也无害。未知 interaction 不能唤醒其他 session 的工具。问题状态随 existing continuous/replayable snapshot/delta 路由；断开单个订阅不取消 owner 问题。

```mermaid
sequenceDiagram
    participant Loop
    participant Actor as Session actor
    participant Store
    participant App
    Loop->>Actor: validated Question(run, call, reply)
    Actor->>Store: commit tool pending + interaction
    Actor-->>App: existing userInput projection
    App->>Actor: resolveInteraction / snooze
    Actor->>Store: commit normalized answer or timer + ACK
    Actor-->>App: ACK + projection
    Actor-->>Loop: committed answer
    Loop->>Actor: ToolDone in call order
    Actor->>Store: canonical tool result
    Actor-->>Loop: commit receipt
    Loop->>Loop: next model request
```

## 验收

TS schema/格式差分与真实 Rust 子进程覆盖：输入拒绝、多题/多选/自定义/预览注解、部分和零回答、拒绝、旧路径、无回答不得调用模型、同批并发题顺序、双连接快照/断开、重复及迟到应答、stop/startNow/EOF/close/冷恢复、自动继续/队首/永久 snooze/关闭重开。可控 Store 验证注册、答案、timer、工具结果提交失败以及答案提交后的崩溃窗口。使用真实 App 提问、回答、取消和恢复，不只跑 schema fixture。必须通过 Rust/App tests、fmt/Clippy、typecheck/lint 和架构检查，单列已有警告，继续关闭增量缓存。

Todo、Plan、MCP elicitation、子代理/工作流交互不以本包完成为完成，继续保留在剩余清单。
