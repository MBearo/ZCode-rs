# Rust 会话 Todo 与 App 计划摘要

2026-09-22。对齐当前 `contracts/src/tools/todo.ts`、`core/src/tool/handlers/todo.ts` 和 V4 plan 投影。权限仅 yolo；Todo 进度清单与 `independentPlanState` 控制的 Plan 执行开关分开，后者保持不支持。

## 规则与接口

- TodoRead 接受严格空对象，返回 `{todos}`。TodoWrite 接受严格 `{todos}`，全量替换，返回 `{oldTodos,todos,summary}`，summary 包含 total/pending/inProgress/completed。
- 每项 content 是非空字符串，status 为 pending/in_progress/completed，priority 为 high/medium/low。对齐 TS：不 trim 原始 content，过滤 item 未声明字段，允许多个 in_progress；空数组清空清单。顶层未知字段、缺失或 null 参数失败，失败不修改旧清单。
- 使用生成的 TS schema/description；TodoRead 可进入四只读并发组，TodoWrite 是顺序屏障，无文件或进程副作用。模型输出最多 100000 UTF-8 字节，截断不损坏持久化清单或 App plan。
- Session 持有一份 canonical todos 和更新时间，V4 plan 与旧 `session/read.todos` 从它派生。plan 项保留顺序，id 为 trim 后的 content，in_progress 投影 inProgress；空清单投影 null，不能留下旧计划。纯空白 content 按 TS 接受，但不产生无效空标题项。

## 所有权与时序

```mermaid
sequenceDiagram
    participant Loop as Agent loop
    participant Owner as Session actor
    participant Store as SQLite adapter
    participant App as App / mobile subscription
    Loop->>Owner: ModelDone
    Owner->>Store: 提交模型工具调用
    Store-->>Loop: durable receipt
    Loop->>Owner: Todo 工具事件（session/run/call）
    Owner->>Store: 原子提交 todos + 已完成工具行
    Store-->>Owner: success
    Owner-->>App: 同一 owner 的 plan / row 投影
    Owner-->>Loop: 真实结果
    Loop->>Owner: ToolDone，按调用顺序提交 canonical
    Owner->>Store: 提交 canonical tool result
    Store-->>Loop: receipt，允许下次模型请求
```

Todo 工具事件经过现有 run generation / cancel 检查。Store 失败立即终止 runtime，不能继续工具或模型请求；提交后、canonical 结果前崩溃时，从已提交工具行恢复真实结果，不重新执行。普通停止、关闭、EOF 和新 run 不清空已提交 Todo。桌面 continuous 与手机 replayable 复用同一事实，断开一个订阅不改变清单。

清单提示沿用 TS 规则：距最后 TodoWrite 调用与距最后提醒均至少 10 个 assistant 消息才追加一次 todo_reminder；输出上限续写期间不追加。提醒包含当时 authoritative todos，作为隐藏 synthetic user 消息先提交再请求模型。压缩后的计数以当前有效历史为准；清单独立于压缩结果存储，TodoRead 始终读取 owner 状态。

## 数据兼容

新增原生元数据 `todos`、`todosUpdatedAt`，老原生会话缺省空清单。TS 新导入读取备份中 todo 表，按 position 排序。已经完成导入但缺少 todos 字段的旧 Rust 数据从已提交 TS 备份一次性补齐，不能重新读取变化后的生产库、重新复制数据库或覆盖 Rust 已有清单（包括显式空数组）。迁移与导入同样响应取消，失败原子回滚。缺失旧测试库 todo 表视为空；真实表存在但字段/值非法则失败，不能静默丢数据。

## 验收

- TS handler/schema 差分验证初始读取、替换、旧值、统计、空数组、多个 in_progress、未知 item 字段、Unicode 与无效输入。
- 真 Rust 子进程 + 现有 App client/schema：同批 write/read/write 顺序，失败保留状态，会话隔离，desktop/mobile 双订阅，冷恢复与关闭后读取；模型内容与 App 投影一致。
- 受控 Store 验证 Todo 状态及 canonical 提交屏障，模拟状态成功提交后的崩溃窗口，迟到旧 run 不修改清单。
- TS 新导入和旧已提交备份补齐：源文件不变、无需新备份、幂等、保留 Rust 覆盖/清空、非法值回滚。
- 提醒阈值/去重/压缩与冷恢复验证；真实 App 创建、更新、清空和重启后的计划显示。
- Rust tests/fmt/Clippy、App tests、typecheck、lint、fmt、架构检查；缓存继续使用 CARGO_INCREMENTAL=0。

Goal iteration、子代理 Todo 镜像和 Plan 工具属于对应后续包，不能因此宣称完整扩展能力完成。
