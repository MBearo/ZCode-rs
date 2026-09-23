# Rust shared context handover

2026-09-22。对齐 App 已有 `session/create(importedHistory.source=sharedContext)`、V4 `sendText.context_refs` 和 `discardSharedContext`，不新增 wire 版本、不由 Renderer 传入共享正文。

## 合同与边界

- Host 的 conversation-share service 继续负责下载、签名/摘要与 artifact 安装；Rust 接收其严格 importedHistory/provenance，校验 markdown SHA-256，按当前 workspace identity 和 session ID 原子创建本地候选。导入不调用模型，允许尚未绑定模型。
- 同 session ID 的同一来源重试幂等；来源/内容冲突不能覆盖已有历史。保留标题、创建时间、share/context ID、规范 share URL、三个摘要、formatterVersion 和 installedArtifacts。旧无 URL provenance 仍用 legacy title-only 投影；不伪造链接。
- 正文复用 SessionStore 不可变字节存储，Session 仅保存私有引用和 provenance，避免每个流式 checkpoint 重写正文。字节先持久化，随后 metadata 单事务提交；失败不得发布会话成功。与现有附件一样，事务前写出的未引用字节仍受后续统一 GC 范围约束。
- `context_refs` 为零或一个严格对象，只接受当前 Session 的已存 context ID，trim ID；未知字段、任意正文、其他会话或 workspace 的 ID、重复附加均拒绝。与当前 Composer 一致，允许仅有效共享引用的空正文输入；无正文、附件或共享引用仍拒绝。引用本身不授予文件路径读取权限。候选正文沿用不可变附件存储的 20 MiB 上界，stdio 请求仍受现有帧大小限制。
- pending 候选不进入模型历史。输入即时开始时，读取候选正文，在同一事务写入隐藏 canonical user 消息、真实用户输入、attached 状态和 ACK，提交后才调用模型。导入正文不生成聊天气泡。
- busy queue/guide/抢占输入提交时 pending -> reserved，sourceId 绑定唯一 queue item；其他输入不能抢用。提升时 reserved -> attached，与输入持久化同事务。Guide 的 hidden context 与 steer 按顺序经同一 StepBoundary 提交回执交给 loop。
- 删除/清空排队输入、关闭会话释放对应 reservation；进程重启丢弃旧 queue ACK 并将 reserved 恢复为 pending。编辑、重排不丢引用。用户取消已开始输入不撤销 attached，也不重复附加。
- `discardSharedContext` 只允许 pending -> discarded，并持久化命令幂等 ACK；reserved/attached/discarded 拒绝。状态通过已有 snapshot/patch 发布给 desktop-continuous 与 web-remote-replayable。
- 压缩只处理已附加的 canonical 历史，不读 pending/discarded 正文；attached 内容压缩后不会因后续输入再次注入。关闭只释放 runtime，候选与历史保留。

```mermaid
sequenceDiagram
    participant Host as Host Share service
    participant Actor as Session actor
    participant Store as SessionStore
    participant Loop as Agent loop
    Host->>Actor: session/create(importedHistory)
    Actor->>Store: immutable bytes + atomic metadata
    Store-->>Actor: committed
    Actor-->>Host: snapshot (pending)
    Host->>Actor: sendText(context_refs)
    alt busy
        Actor->>Store: reserved + sourceId + ACK
        Note over Actor: queue delete/close/restart releases reservation
    end
    Actor->>Store: hidden context + user input + attached + ACK
    Store-->>Actor: committed
    Actor->>Loop: start / StepBoundary receipt
    Loop->>Loop: model request
```

Session actor 是状态、reservation 和 canonical 历史唯一 owner；blob IO 在现有 Store adapter。Host/Renderer 只读投影，不维护第二份已接受的上下文状态。run ID 与旧 ACK 防重规则不变。

## 旧 TS 导入

读取已提交备份内 shared_context message 与 v4/shared_context_import provenance，保留来源和生命周期。pending/reserved/discarded 不进入 canonical；reserved 在恢复时释放。attached 与无状态的 legacy shared_context 按 TS hydrator 保留在隐藏模型历史。正文和 provenance 必须关联同一 context ID；损坏、缺失、摘要不一致明确失败并回滚导入。已由旧 Rust 版本错误注入 pending/discarded 的历史需要单列修复，不能通过悄悄过滤当前用户输入来掩盖。

## 验收

- 真实 Rust stdio + App schemas：无模型导入、双身份/同 share 多 session、幂等/冲突、严格引用及摘要校验、隐藏 UI 正文、首次附加与后续无重复、丢弃、同会话队列竞争、编辑/重排/删除/清空/抢占/guide、冷恢复与压缩。
- 使用真实 TS commitSharedContextImportBundle/transitionSharedContextImport/formatter 建立差分 fixture；覆盖 pending/reserved/attached/discarded 和 legacy。生产源文件、备份和 installedArtifacts 不改写。
- 可控制的 Store 提交故障证明：导入失败没有成功回复；输入提交失败没有模型请求或可见 attached；迟到事件不复活旧 reservation。
- App 现有 handover 的状态/首发与冷恢复；当前 Composer 已移除旧候选 chip，首发仍从 snapshot 自动携带引用。离线只读块由 Host 已安装的 shared-conversation.json 提供，本地 fixture 未安装该文件时不应伪造块。没有执行的公网分享下载或手机/远端验收单列。
- Rust tests/fmt/Clippy、App tests/typecheck、仓库 typecheck/lint/fmt/architecture。构建 CARGO_INCREMENTAL=0；清理限于已确认可重建缓存。
