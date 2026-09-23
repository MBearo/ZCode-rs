# Rust 运行中输入对齐与 App 验收

2026-09-22，ZCode-Pro 当前工作树，macOS arm64，Rust debug 子进程与真实 GLM-5.3 Max。新增 guide、busy startNow 与 setFollowupMode；默认 TS runtime 和 yolo-only 边界保持不变。

## 完成内容

- guide 在正常 text-only 回复或完整工具批次提交后，同 product turn 继续。一次消费一条 guide，普通 queue 不挡 guide 子序列；保留来源与 guided 行。中断和附件回退有明确 reasonCode。
- guide 的冻结模型选择在持久消费后更新，下一个请求才使用；user_steer 文案由 TS 源码生成资产并检查漂移。
- startNow 复用 Session 队列及唯一预留，ACK/预留提交后取消旧执行，旧工具退出和 Finished 提交后才启动新轮。原有队列继续按原序执行；stop/EOF/close 撤销提升，冷恢复区分已消费与未消费的 ACK。
- Session actor 仍唯一拥有接纳输入、canonical history、选择与投影。新增内部 StepBoundary 回执，不增加 App 协议版本。每步只返回新消息，无 guide 不额外提交数据库，不复制整段历史。
- 修正取消与已入队 Finished 的竞态：最终状态同时检查 owner 的取消事实，旧 success 不能覆盖新的停止指令。

时序与验收合同见 [spec](../specs/rust-busy-input.md)。主要代码为 `app/busy_input.rs`、`input_admission.rs`、`agent_loop.rs`、`event_projection.rs` 及 Session 队列/模式投影；`run.rs` 从 engine 提取启动装配，保持每个 Rust 源文件不超过 400 行。

## 自动化证据

7 项真实 Rust 子进程 / App client/schema 测试覆盖连续引导、同轮恢复、双连接、完整工具批次与部分失败、流式抢占、Shell 退出屏障、模式持久化、附件回退、停止保留、模型冻结及永久请求失败。

4 项可控 Store/工具 fixture 测试覆盖 admission、guide 消费、旧轮终态、新输入提升各事务失败；竞争抢占拒绝、预留期间 stop、EOF 不提升。失败事务后没有下一模型调用，也不会在清理路径恢复未提交事实。此前直接拒绝的 guide/startNow/setFollowupMode 已由失败测试复现。

| 验证                                        | 结果                               |
| ------------------------------------------- | ---------------------------------- |
| `CARGO_INCREMENTAL=0 pnpm test:rust-agent`  | 40 Rust / 110 App，全通过，0 跳过  |
| `CARGO_INCREMENTAL=0 pnpm check:rust-agent` | 边界、fmt、Clippy -D warnings 通过 |
| `pnpm typecheck`                            | 通过                               |
| `pnpm lint`                                 | 0 错误；既有 70 条警告             |
| `pnpm fmt:check`                            | 通过                               |
| `pnpm architecture:check --changed`         | 0 违反、0 新增                     |

完整日志保存在 `.zcode-runtime/rust-e2e/20260922/checks/busy-input/`。本轮一次新增测试的 trait 返回类型编译失败已修正，之后全量重新执行通过；未把中途失败列为通过。

## 真实 App

通过 Electron/agent-browser 操作隔离 `ZCode Rust E2E`，使用原 App 设置页与 Composer；工作区为隔离 `mode-workspace`，会话为 `2266a711-b002-4841-9136-e4acb2678e80`。

1. 在设置页把 Queue 切换为 Guide，stdio 收到 setFollowupMode 并 accepted。Bash 前台执行 25 秒等待时发送引导；工具只执行一次，随后回复 `RUST_GUIDE_APP_OK`。数据库确认两个 userInput 属于同一 turn，第二条 guided=true。
2. 在设置页恢复 Queue。Bash 运行中先普通 Enter 排队，再用 macOS Meta+Enter 立即发送。旧 Shell PID 已退出、旧轮 completedInterrupted，新轮回复 `RUST_START_NOW_APP_OK`，原队列随后回复 `RUST_AFTER_NOW_QUEUE_OK`。输入顺序和 ACK delivery 与预期一致。
3. 最终二进制在上述场景启动前完成构建；SHA256 为 `9b7384388683f9a46a9c7d611bc01c2ba87f0d960e5cddf51e746335ed669f36`。真实 stdio 使用该 Rust executable，未回落 TS。
4. SIGTERM 结束此工作区 Rust 后，App 自动启动新进程（15290 → 15912），保留 guided 历史和完成/中断事实；续聊实际回复 `RUST_BUSY_RESUME_OK`，未再次执行工具，Renderer errors 为 0。

结构化证据为上述目录的 `busy-input-evidence.json`，截图为 `23-guide-pending.png`、`24-guide-complete.png`、`25-start-now-preempted.png`、`26-start-now-and-queue-complete.png`；Renderer errors 另存。测试只写隔离工作区内的 PID 文件，不访问其他项目。

## 容量与边界

本轮始终使用 `CARGO_INCREMENTAL=0`，增量缓存 0 B；实测剩余约 12 GiB。沿用此前清理出的空间，不复制生产旧库，保留已提交的迁移备份。fixture 的数据库、工作区与进程在测试结束后回收。

本包确认了运行中输入在本机 App 的实际操作闭环。跨平台、真实手机/远端、TS/Rust release 五轮性能对照、AskUserQuestion、Todo/独立计划及其他工具仍在 [剩余清单](../specs/rust-parity-remaining.md)，不宣称全量替换完成。

额外待查：本轮全量执行中，既有 `background_registration_requires_commit_and_eof_reaps_processes` 用例耗时约 30 秒，期间观察到父进程为 1 的 `sleep 30`。用例最终通过，但当前只断言 Shell 父 PID 已退出，未覆盖后代在取消竞态中的生存期。下一步需用子进程 PID 和有限收口时间复现、定位；本次实机 startNow 的父 Shell 退出通过，不能据此宣称完整进程树矩阵已通过。

后续更新：上述 30 秒取消问题已复现并修复，新增后代/进程组断言及真实 App stop/startNow 复验，见 [Shell 生命周期报告](rust-shell-lifecycle-2026-09-22.md)。
