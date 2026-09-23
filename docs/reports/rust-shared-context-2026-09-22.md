# Rust shared context 与 App handover 验收

2026-09-22，macOS arm64，当前工作区 Rust debug 产物。完成当前 App handover 的导入、引用发送与状态持久化；默认 runtime 仍为 TS，Rust 显式选择，权限只支持 yolo。规格见 [rust-shared-context.md](../specs/rust-shared-context.md)。

## 已实现

- 既有 `session/create(importedHistory.source=sharedContext)` 接入真实 Rust stdio，严格校验 workspace、provenance、摘要及参数。无模型/账号也可先导入，不触发模型调用。同来源同 session 重试幂等，冲突不覆盖历史。
- Session actor 唯一持有 pending/reserved/attached/discarded。共享正文存入不可变字节，metadata 只持有描述符，避免每次 checkpoint 重写候选正文。已有 20 MiB 存储上界与 stdio 帧限制保持有效。
- `sendText.context_refs` 只引用本会话的已验证候选。隐藏上下文、真实输入、attached 与 ACK 同事务提交，之后才启动模型。pending/discarded 不进入模型或压缩，已附加上下文不会在后续输入/压缩/冷恢复时重复注入。
- busy queue/guide/startNow 预约候选；编辑/重排保留引用，删除/清空/close/restart 释放预约。Guide 通过同一个 StepBoundary 提交回执按顺序返回 hidden context 与 steer，没有第二条历史写入路径。
- `discardSharedContext` 持久化状态和幂等 ACK；无聊天行的导入候选也即时持久化。无模型候选省略未绑定的 modelSelection，避免 V4 schema 收到空 ID。
- TS 历史导入区分四种状态与旧无状态上下文；reserved 冷恢复为 pending，损坏关联/摘要使导入事务回滚。备份和源库不改写。
- 实机发现 Composer 允许空正文只发送共享上下文，补充失败回归后已对齐；完全空输入仍拒绝。

新增 domain/shared_context、domain/shared_import、app/shared_context、adapters/legacy_shared_context 四个 Rust 文件共 530 行；每个源文件不超过 400 行。IO 仍通过 Store port，App/Host/Renderer 不新增状态副本，wire 版本不变。

## 测试与故障验证

新增 15 个真实 Rust 子进程 + 既有 App SDK/schema 测试：导入/冲突/无模型、错误引用/摘要/身份、双连接、首次与后续发送、空正文引用、队列编辑/重排/删除/清空、直接 startNow 与 sendQueuedNow、guide、关闭/重启、TS 五类历史、压缩、数据库提交失败。

SQLite trigger fixture 分别令导入和输入提交失败，验证没有成功发布、没有 provider 请求，进程停止；重启状态保留 missing/pending。TS fixture 使用真实 commitSharedContextImportBundle。首次测试错误地向 Store 传入两层 modelSelection；按其逻辑接口改为平铺 selection 后，迁移回归通过，没有修改生产 importer 来适配错误 fixture。

| 检查                                        | 结果                                                                                       |
| ------------------------------------------- | ------------------------------------------------------------------------------------------ |
| `CARGO_INCREMENTAL=0 pnpm test:rust-agent`  | Rust 50 / App 154 全通过，0 跳过；包含测试 TypeScript 编译及生成 prompt/tool schema 一致性 |
| `CARGO_INCREMENTAL=0 pnpm check:rust-agent` | Rust boundary / fmt / Clippy `-D warnings` 通过                                            |
| `pnpm typecheck`                            | 通过                                                                                       |
| `pnpm lint`                                 | 0 errors，70 个既有 warnings                                                               |
| `pnpm fmt:check`                            | 通过                                                                                       |
| `pnpm architecture:check --changed`         | 0 violations / 0 new                                                                       |

## 真实 App 与真实模型

通过现有 TS Store 和 formatSharedContextV1 构造隔离的小型历史库，包含只出现在隐藏上下文中的校验码。启动隔离 Electron profile、真实 Host 与 Rust CLI，保留正式 ZCode 进程。

1. App 任务列表显示已导入任务；Composer 自动从 snapshot 携带 `context_refs`。通过 UI 提问后，Rust 状态由 pending 变为 attached，GLM-5.3 回复 `RUST_SHARED_CONTEXT_PROVIDER_OK`；数据库只有一条隐藏共享消息，界面不泄露候选正文。
2. 关闭并重新启动整个测试 App，从任务列表打开该历史，再经 Composer 续聊。真实 stdio 续聊不再携带引用，GLM-5.3 再次返回相同校验码，历史内共享正文仍只有一份，源 TS 数据库 SHA-256 不变。
3. 第二个候选在空输入框直接点击 Send。真实 stdio 为 `text: ""` 加一个 `context_refs`；模型成功读取并确认校验码，状态 completedSuccess/attached，没有重复注入。该 fixture 原名含 discard，实际用于 context-only 验收；不是 UI 丢弃验收。

当前源码已移除旧共享候选 chip；其缺席不算 Rust 回归。离线只读分享块依赖 Host 安装的 shared-conversation.json，本 fixture 未安装该文件。本轮覆盖 TS fixture 迁移 → App Composer → Rust → 真实模型，`session/create` 则由真实 App SDK 子进程测试覆盖；不声称验证了公网分享下载或 artifact 安装。

冷启动进程：Electron 37028、Host 37652、测试 workspace Rust 37720。最后验证二进制 SHA-256：`970717123fba8b29b97e47d2b0c5c82ddbe0838d25f02314eef08d94adf2edad`。两次 Renderer errors 均为空。已有 workflowRuns 未实现仍在剩余清单。

证据位于 `.zcode-runtime/rust-e2e/20260922/`：`shared-context-evidence.json`、`shared-context-{app,cold-app}.log`、`shared-context-cache-cleanup.json`、`screenshots/43-shared-context-attach.png`、`44-shared-context-cold.png`、`45-shared-context-only.png` 和 `checks/shared-context/`。日志与本地账号配置不纳入提交。

两次停止测试 App 后共清理约 108 MiB 可再生成的 Chromium 缓存，保留会话/备份/附件/截图。构建始终 `CARGO_INCREMENTAL=0`，incremental 为 0 B，完成时磁盘约 11 GiB 可用。

## 保留的缺口

- 旧 Rust 版本已经错误注入 pending/discarded 的 native 历史尚需安全修复；本轮修正新导入，不凭缺失来源映射删除已有消息。
- 公网下载、签名与 artifact 安装、离线分享只读块、手机与远端恢复矩阵尚未实机覆盖。无 UI 丢弃入口的路径仅由协议测试覆盖。
- 事务前写入的未引用字节仍待统一 GC；没有把 SQLite 提交与文件写入称为同一原子事务。
- 其他 importedHistory 来源（如 claudeCode）、fork/edit/retry、工作流和扩展工具继续对齐。
- 本轮没有完成 TS/Rust release 五轮性能对照；不可把 debug 功能验证称为全量替换或性能验收。
