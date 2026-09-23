# Rust Composer 附件进展

2026-09-22，`main` / `872ad96` 上的未提交工作区。本轮补上新附件的基础输入链路，完整 TS 附件行为和真实界面验收仍未全部完成。总目标仍按 [剩余清单](../specs/rust-parity-remaining.md) 推进。

## 已实现与验证

- 既有 `v4/attachment/begin/chunk/commit/abort`：相同 connection/session/upload 的重试幂等，重复片冲突、乱序、校验和、总字节数、并发/暂存预算、TTL 和连接关闭清理。以同一调用序列对照 TS AttachmentUploadRegistry。
- 本地路径快照与已上传 ref：支持仅附件输入、create firstInput、普通输入、busy queue；排队后删除源文件仍使用 admission 时的不可变内容。不同输入同一路径的不同版本获得独立引用。
- 图片/PDF 请求覆盖 Chat Completions、Responses 和 Anthropic；视频覆盖 TS 当前补丁的 Chat/Anthropic 网关形状。模型能力不支持、损坏 PDF、跨会话引用或上传元数据不匹配会报错；未验证的 Responses 视频与音频不冒充支持。
- 快照先持久化、引用与输入再提交；存储失败不能启动模型。附件预览仍验证 Session/row/entity/index；冷恢复继续使用已保存快照，快照缺失会终止请求，不发空内容代替附件。
- 媒体 base64 不进入 canonical/Session/队列/stdio。请求 adapter 按需读取快照，每次模型调用只编码一次；重试复用编码，流期间释放临时展开的请求树。一般文本请求继续保留原 2 MiB 上限，含附件请求有独立且有界的媒体预算。

所有权保持在 Session/Engine；异步文件操作在 SessionStore adapter。改动归属 Rust domain/app/adapters 和 services 回归测试。TS 上传 registry 仅补充已由范围检查证明的非空类型断言，以便其源码参与严格类型的差分测试，没有改变 TS 上传行为。

## 检查结果

| 命令                                | 结果                                    |
| ----------------------------------- | --------------------------------------- |
| `pnpm test:rust-agent`              | 28 个 Rust 测试、84 个 App 集成测试通过 |
| `pnpm check:rust-agent`             | Rust 边界/fmt/Clippy 全 target 通过     |
| `pnpm typecheck`                    | 通过                                    |
| `pnpm lint`                         | 0 errors / 70 条既有 warnings           |
| `pnpm fmt:check`                    | 通过                                    |
| `pnpm architecture:check --changed` | baseline 0 / new 0 / violations 0       |
| release 构建                        | 通过                                    |

测试入口为 `rust-agent-attachments.test.ts`、`rust-agent-attachment-media.test.ts`、Rust `attachment_upload.rs` 和 `runtime_consistency.rs`。新用例已先在旧二进制复现缺失接口/仅附件输入失败，再在新二进制通过。新增媒体场景包含超过 2 MiB 的请求和缺失快照恢复；测试检查真实请求体、磁盘和数据库事实，不只检查 ACK。

首次验收因 Mac 锁屏而暂停；16:12–16:20 已在可操作的真实 App 中重新构建、重启并补验本地文本附件。Composer 选择文件、附件内容驱动 Write/Read/Bash、源文件移走后的冷重启续聊均通过。详见 [真实 App 复验](rust-app-e2e-attachments-2026-09-22.md)；图片/PDF/视频与 Web 上传的真实界面路径仍待验。

## release 对照

Apple M1 Max / macOS arm64；已有 P0 release 对本轮候选，每场景各 5 次，串行交替，共 30 次。候选同时包含上一轮 `session/read` 修复。固定 stream 8×2048、history 100×64、sessions 4×8×512，双方 contextWindow 256000。表为中位数，RPC 是每次运行 p95 的中位数；这次对照没有附件载荷，仅验证既有热路径没有明显退化。

| 场景     |   启动 ms |   首轮首段 ms | 后续首段 ms |     总耗时 ms | RPC p95 ms | 采样峰值 RSS MiB |          存储 B |
| -------- | --------: | ------------: | ----------: | ------------: | ---------: | ---------------: | --------------: |
| stream   | 7.53→8.01 | 156.37→154.05 |   1.83→1.61 | 283.15→278.08 |  1.80→0.63 |      29.31→31.03 | 2871456→2871456 |
| history  | 7.39→7.31 | 152.68→150.59 |   1.14→1.18 | 378.79→384.00 |  0.44→0.46 |      24.06→23.69 | 5430944→5410416 |
| sessions | 7.22→7.17 | 153.51→150.99 |   2.96→2.76 | 258.50→254.04 |  2.79→1.26 |      31.50→30.56 | 6364904→6368976 |

未出现数量级退化；小样本 RPC 波动不作为确定加速承诺。原始样本在 `.zcode-runtime/rust-bench/attachments-20260922/`。RSS 是采样峰值，存储是文件占用，均不是精确分配/物理写入计数。这不是 TS/Rust 对照，也不证明大附件内存上界或实际模型供应商性能。

- 基线 SHA256：`bf587951abfec3b619d05560042703d737efef8e1a24997adfaabc3bf2843754`。
- 候选 SHA256：`964dac28bd72eba2d868ef532c729931322a3ec95999b98f4098e233571f324b`。

## 尚未对齐

本地超过 20 MiB 的文件、大图校验/缩放、TS 完整 Read 文本预览策略、音频、Responses 视频、未引用附件回收，以及多媒体 Composer/手机/远程上传恢复尚未验收。当前文本预览最多 64 KiB，显示截断提示；本地来源路径保留供模型按需读取，上传大文本的完整读取工具还需要补齐。shared context refs 属于持久化 provenance 功能，不因附件支持而算作完成。

默认仍为 TS；本轮不宣布 A 或 B 替换里程碑达成。
