# Rust 导入生命周期与缓存清理复验

2026-09-22，macOS arm64，同一 ZCode-Pro 工作树。本轮修复旧库导入失败时重复占用磁盘及部分历史提交；全量替换目标仍未完成，默认 runtime 仍为 TS。

## 已完成

- 失败复现：旧实现导入第一个会话后，第二个会话缺失附件导致启动失败，但 Rust 库已留下 1 个会话、1 条模型消息、2 行 UI 历史。再次启动又复制源库。
- 同一 source/workspace 的会话、行、消息、ACK 与导入标记改为一个事务。复用 Store 的原有写入 SQL；既有原生会话不覆盖。
- 独占 import 文件锁串行跨 workspace 导入与恢复；工作区 Session owner 锁保持原职责。备份每步 128 页检查取消，SIGTERM/Ctrl-C、启动导入期间 EOF 以及已检测到的 stdout 错误可取消。退出前等待 worker 回滚与清理。
- 每次尝试独立拥有备份和附件目录。失败回收；重启只回收未被导入标记引用的严格 UUID 目录。成功备份继续保留。提交错误而数据库查询不可用时保留文件，待下次持锁恢复确定归属。
- 备份发布前关闭连接、转为独立 DELETE journal 数据库并同步文件/目录；成功时不依赖 WAL sidecar。

规则、所有者与事件顺序见 [spec](../specs/rust-import-lifecycle.md)。

## 验证

新增 2 个 Rust 测试：确定性分步备份取消及源库保留、取消锁等待。新增 4 个实际 Rust 子进程 / TS 存储 / App client 场景：

1. 后续会话缺失附件，重复失败均无半份历史或遗留本次文件；修复后导入、重启幂等，原库字节不变。
2. 恢复未提交的目录及已发布备份；保留已提交目录和旧格式备份。
3. EOF 与独立 SIGTERM 取消持锁等待，不输出 ready；随后解除锁可成功重启。
4. 最终导入标记写入失败，回滚新历史和已发布备份；保留原生会话，修复后导入成功。

本轮完整门禁均执行，`CARGO_INCREMENTAL=0`：

| 门禁                                | 结果                                 |
| ----------------------------------- | ------------------------------------ |
| `pnpm test:zcode-cli-rust`          | 36 Rust / 101 App，通过，0 跳过      |
| `pnpm check:zcode-cli-rust`         | 边界、fmt、Clippy `-D warnings` 通过 |
| `pnpm typecheck`                    | 通过                                 |
| `pnpm lint`                         | 0 错误，原有 70 条警告               |
| `pnpm fmt:check`                    | 通过                                 |
| `pnpm architecture:check --changed` | 0 违反、0 基线、0 新增               |

门禁日志与修复前失败证据：`.zcode-runtime/rust-e2e/20260922/checks/import-lifecycle/`。

## 真实 Desktop

重启隔离的 `ZCode Rust E2E`，App PID 83927，测试 workspace native PID 84600；显式 `zcode-cli-rust` 和 `app-server --stdio`，使用原有隔离 profile / 数据库。GLM-5.3 Max / yolo。

新二进制 SHA256：`4968f5336b60a6f13c89c1d8b6152f81d045c8f446f7c0a25795c53fc406dcea`。

打开原有上下文验证会话，再发送重启续聊请求，实际返回 `RUST_IMPORT_RESTART_OK`。SQLite 显示 completedSuccess、2 条 canonical user 消息、0 工具行；原先的 `PROMPT_CONTEXT_CEDAR_58` 仍在 UI。renderer error 为空。证据：`import-lifecycle-evidence.json`、`screenshots/18-import-lifecycle-restart.png`、`import-lifecycle-app.log`（均位于上述隔离目录）。

本次真实 App 通过进程级 `ZCODE_SESSION_DB_PATH` 指向不存在的测试源，避免再次复制开发者大库；本轮导入故障证据来自真实 TS fixture 与真实 Rust 子进程，不能据此宣称生产大库导入已经完整验收。

## 磁盘与边界

前一轮已经清理约 6.8 GiB（Rust 增量缓存及确认未引用的旧导入副本等）；本轮另清理两个已退出的故障测试目录，约 1.9 MiB。17:36 复验可用 11.09 GiB，增量缓存 0 字节。保留 6 个旧备份文件，其中包括已有提交引用和此前保留的最新源副本；不会把已提交备份当缓存清除。清理清单见隔离目录 `cache-cleanup.json`。

仍待对齐：大型历史按需加载、成功备份跨工作区去重及明确保留策略、大数据库启动 RSS/磁盘峰值、跨平台故障矩阵。单次 SQL / 文件 IO 不可抢占，不承诺硬实时取消。旧实现遗留的部分导入及旧命名文件不会被本轮自动重写或清除。其余功能缺口仍跟踪于 [剩余清单](../specs/rust-parity-remaining.md)。
