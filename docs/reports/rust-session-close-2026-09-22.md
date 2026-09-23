# Rust 会话关闭与草稿清理对齐

2026-09-22，`main` / `872ad96` 的未提交工作区。修复真实 App 切走预热草稿时 `deleteSession` 返回 `guard.capabilityUnsupported` 的缺口。依据当前 TS handler、closeSession binder 和 gateway dispose 的实际语义：关闭 runtime、保留持久历史，不做永久删除。

## 行为与边界

- Session actor 先暂停队列，取消前台、后台、权限及反向鉴权等待，收齐执行终态；关闭前不能只发送 cancel 后立即假定 Shell 已退出。
- 工具 adapter 释放后台任务 handle 和当前会话文件读取新鲜度记录；其他会话不受影响。后台登记已投递但未获得提交回执时取消，也会投递 task/run 对应终态，避免关闭等待不存在的进程。
- 未执行 queued 输入得到可查询的 `fault.input.discardedOnClose`；持久会话终态、队列 disposition 和关闭 ACK 在现有事务中提交，存储失败停止 actor，不发布成功移除事实。
- 清理当前会话全部连接的上传事务及 conversation 订阅，再更新 sessions-index。暂停连接恢复不会把已关闭 runtime 重新放回索引；旧 subscriptionId 不能 resync 复活会话。
- 历史和附件字节保留。重新订阅通过单会话 Store 查询加载，以新 epoch 发 snapshot，不触发模型和工具；legacy session/read 仍为 existing-only。未落盘草稿不会因关闭而新增永久 ACK/WAL 写入，旧无历史记录的回收另有事务保护，不能误删已提升会话。

改动仍在 zcode-rust 的 app/domain/adapters 端口边界内；共享协议版本、默认 TS runtime 未变。受控架构上下文和改动前后的检查均为 0 violations / 0 baseline / 0 new。规格见 [会话关闭 spec](../specs/rust-session-close.md)。

## 验证

新增子进程测试先在旧二进制复现三类关闭均被拒，再验证空草稿、重复命令、CAS、两个 deliveryKind、索引背压恢复、运行中 Shell、队列 disposition、后台 Shell、跨会话隔离、附件保留、冷重开、重启、迟到模型流和数据库故障。存储测试验证草稿回收事务 rollback、拒绝回收有 canonical 消息的记录，以及单会话读取不解析其他历史。

取消登记竞态的原生测试先失败：取消后没有终态事件；修复后通过。另一个既有后台 EOF 测试在完整并发回归中暴露验证竞态：前台 completed 不代表后台 Shell 已写出 PID。测试改为等待实际 PID 文件后再关闭和检查进程退出，没有放宽退出断言。

| 检查                                | 结果                                                  |
| ----------------------------------- | ----------------------------------------------------- |
| `pnpm test:rust-agent`              | 32 Rust / 91 App 集成测试通过                         |
| `pnpm check:rust-agent`             | native 边界、fmt、Clippy 全 target `-D warnings` 通过 |
| `pnpm typecheck`                    | 通过                                                  |
| `pnpm lint`                         | 0 errors，70 条既有 warnings                          |
| `pnpm fmt:check`                    | 通过                                                  |
| `pnpm architecture:check --changed` | violations / baseline / new 均为 0                    |

真实 App 以隔离测试 profile 重启，在已有任务与 New task 草稿之间切换。首次关闭实现的三个真实 `deleteSession` 请求均 accepted；加入纯内存草稿不写永久 ACK 的优化后，再用最终 debug 产物复验两次，均 accepted，SQLite 中对应草稿历史及永久关闭 ACK 均为零，Renderer 未捕获异常。最终进程为 `zcode-rust app-server --stdio --cwd <测试工作区> --surface desktop`，PID 29575，二进制 SHA-256 为 `b388400cdebbbaaabaa7dbd340af03b7c8960da271f77497247971b66de90055`。

最终证据在 `.zcode-runtime/rust-e2e/20260922/close-final-evidence.json`、`close-final-renderer-errors.txt` 与 `screenshots/16-final-draft-close.png`；首次复验保留在 `close-evidence.json` 和 `screenshots/15-draft-close.png`。完整检查日志位于 `checks/session-close/`。日志只抽取命令 ID、会话 ID、ACK、PID 和产物指纹，完整账号上下文不放入报告。

复验也确认一项启动限制：App 重启后的新草稿默认使用 `build`（Ask before changes），Rust 按当前只支持 yolo 的边界拒绝预热；从真实界面选择 Full access 后预热和清理正常。此处没有自动把询问权限降为 yolo，未宣称默认模式选择已经对齐。

## 仍未对齐

fork、editUserQuery、retryTurn、文件回退、反馈，完整 prompt/记忆、shared context、引导抢占、问答/计划、工具扩展等仍按 [完整剩余清单](../specs/rust-parity-remaining.md) 推进。工作流、插件目录及旧历史缺附件导入等实机发现未因本包被标为完成。单会话冷恢复已按需读取，但进程首次启动仍加载全部 workspace 历史；发布级容量、跨平台和 TS/Rust release 性能矩阵仍待验。
