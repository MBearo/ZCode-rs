# Rust 会话关闭与草稿回收

## 现有契约

以 TS `commands/handlers/session-mgmt.ts` 的 `deleteSession`、`zcode-protocol/v4-bridge.ts` 的 closeSession 及 `v4-gateway.ts` 的 disposeSession 为准：删除命令关闭 runtime、释放订阅和上传状态、通知 sessions-index 移除，已持久化历史不删除。再次 conversation subscribe 从持久层冷恢复。空预热草稿不产生历史记录。此语义不等同于归档，也不引入永久删除功能。

## 所有者与顺序

Session actor 独占 admission、运行代际、队列、ACK 和订阅。Store 端口提供单会话加载和无历史草稿的事务回收；adapter 执行 SQLite IO。ToolPort 的关闭钩子释放该会话后台任务资源，不影响其他会话。

```mermaid
sequenceDiagram
  participant UI as App / Host
  participant A as Session actor
  participant R as Model / tools
  participant S as Store
  UI->>A: deleteSession + commandId + optional CAS
  A->>A: check idempotency/CAS; pause queue
  A->>R: cancel foreground/background and pending interaction
  R-->>A: terminal events (runId checked)
  A->>S: commit interrupted state, queue disposition and close ACK
  Note over A,S: draft without history: reclaim metadata and commit ACK atomically
  A->>A: remove session, uploads and both delivery subscriptions
  A-->>UI: accepted ACK; sessions-index session.removed
  UI->>A: new conversation subscribe
  A->>S: load this persisted session only
  A-->>UI: new epoch + snapshot; old subscriptions stay invalid
```

- 命令只接受空 payload；重复 commandId 返回 duplicate，CAS 冲突不取消运行。会话不存在且命令未命中既有 ACK 时明确失败。
- 关闭先停止队列提升，取消前台和所有后台工作、权限与反向鉴权等待；等待真实终态，不以固定超时冒充完成。运行期间已显示的文本和工具历史保留，未完成工具按现有 interrupted 恢复规则补齐 canonical 结果，不能重放副作用。
- 后台登记已经进入 owner、尚未收到提交回执时被取消，也必须投递相同 task/run 的终态；不能留下没有实际进程的 running 工作而使关闭永远等待。关闭释放该会话文件读取新鲜度记录，重开后修改既有文件须重新读取；其他会话记录保留。
- 未执行的 queued 输入转为 failed / `fault.input.discardedOnClose`，结果为 inputDisposition，可通过 commands/query 查询；与关闭 ACK 和会话终态同事务提交。关闭后的旧事件不能恢复会话、推进队列或污染重新打开的 generation。
- 已持久化会话从运行注册表释放，历史、附件快照和累计使用量保留。只回收没有任何 row/message 历史的草稿元数据；Store 再次核查以防误删已提升会话。未引用文件回收仍由附件 GC 后续任务处理。
- 普通预热草稿从未落盘，其 close ACK 仅在当前进程幂等缓存中保留，不为频繁切换页面额外写 SQLite。仅当确有旧的无历史持久记录时，回收与 ACK 才进入同一事务；进程重启后的未持久化 create/delete 命令不保证可查询。
- 关闭成功后移除全部 connection 的 conversation 订阅及 session 上传暂存，sessions-index 所有者更新后再推移除事件。暂停/背压订阅恢复时也不得重新看到关闭的内存实体；其他会话不受影响。
- conversation subscribe、历史/附件读取可按 TS 对应入口冷恢复持久历史；旧 subscription 的 resync 不能恢复关闭实体。legacy session/read 保持 existing-only，不偷偷激活历史。冷恢复生成新 epoch，不触发模型或工具，不自动执行旧队列。
- 任何存储失败必须停止 actor；不得返回 accepted 或发布未提交的关闭/移除事实。关闭前取消运行属于清理动作，失败时不恢复执行。

## 验收

真实 Rust 子进程 + App client/schema 覆盖空草稿及有上传草稿回收、同 ID 重试、stale CAS、双 delivery 订阅清理、backpressure/index、运行中取消、队列 disposition、后台 Shell 退出、其他会话隔离、重开与进程重启保留历史/附件、迟到事件和存储失败提交屏障。真实 App 从草稿切换到任务不再出现清理预热会话被拒警告。默认 runtime 与协议版本保持不变。
