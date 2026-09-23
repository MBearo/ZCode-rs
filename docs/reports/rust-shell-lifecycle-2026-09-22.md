# Rust Bash 进程树回收与 App 验收

2026-09-22，ZCode-Pro 当前工作树，macOS arm64。修复停止工具时的后代泄漏与 30 秒收口延迟，保持 yolo-only、TS 默认、Rust 显式选择。

## 原因与实现

旧实现对原进程组只发一次 SIGKILL，随后等待父进程与输出管道。启动后立即取消可能与 fork 竞争：本机观察到父进程退出，而 `sleep 30` 已被 reparent 到 PID 1，仍持有管道。既有后台测试稳定耗时 30.02 秒；仅断言父 PID 已死不足以验证回收。

按照 TS Bash 生命周期，终止先快照拥有的后代并发送 TERM，在终止开始后的 1500ms 宽限期内等待，再刷新关系并强制回收。原进程组不会因组长退出而丢失所有权；跨组后代与后续新生组员一起验证。PID 加启动时间用于跨阶段身份检查，进程表有大小与时间上限。正常无后代完成不扫描全局进程表，也不增加常驻轮询。

输出管道和 child exit 一起决定工具完成；超时、取消、输出限制都走同一回收路径。清理无法确认时发送内部 `ToolCleanupFailed`，停止 actor admission，不能继续下一模型请求。Session actor 等实际前后台工具结束，移除了早于 TERM 宽限期的 1200ms 退出截止时间。所有者和时序见 [spec](../specs/rust-shell-lifecycle.md)。

## 自动化验证

新增 4 项本机进程测试覆盖 TERM trap、跨进程组且忽略 TERM 的后代、超时，以及 leader 正常退出但子进程持有管道。旧后台测试增加 3 秒保护与进程组断言。断言在 fixture 清理前执行，避免用测试清理掩盖产品泄漏。

新增 1 项可控 actor 失败屏障测试，另在现有测试覆盖迟到旧 run 的清理错误隔离。新增 5 项真实 Rust 子进程 / App client/schema 测试覆盖 stop、startNow、EOF、EPIPE、TaskStop；验证父子进程已退出，才允许新请求或观察终态。

| 验证                                            | 结果                               |
| ----------------------------------------------- | ---------------------------------- |
| `CARGO_INCREMENTAL=0 pnpm test:zcode-cli-rust`  | 46 Rust / 115 App，通过；0 跳过    |
| `CARGO_INCREMENTAL=0 pnpm check:zcode-cli-rust` | 边界、fmt、Clippy -D warnings 通过 |
| `pnpm typecheck`                                | 通过                               |
| `pnpm lint`                                     | 0 错误；既有 70 条警告             |
| `pnpm fmt:check`                                | 通过                               |
| `pnpm architecture:check --changed`             | 0 违反、0 新增                     |

失败前后及最终日志保存在 `.zcode-runtime/rust-e2e/20260922/checks/shell-lifecycle/`。初版测试使用 macOS Bash 不提供的 BASHPID，已改为子 Bash 的 `$$` 并清理该失败 fixture；最终验证全部重新执行。Node SQLite 实验性提示单列保留。

旧 30.02 秒回归用例在修复后重复五次：测试进程 wall time 为 82.99、75.48、67.13、62.77、62.23ms，测试体为 0.06–0.07 秒。随后全量重跑仍为 0.07 秒。这是同机 debug 回收回归测量，不替代 TS/Rust release 性能验收。

## 实际 App 验收

隔离 `ZCode Rust E2E` 使用原 Composer、停止按钮和 Meta+Enter，真实 GLM-5.3 Max，工作区为 `mode-workspace`。

- Stop：父 PID 34094、子 PID 34095，PGID 不同，子进程忽略 TERM。点击停止后 1745.83ms 内两者均已退出；stdio 从 stop 到 completedInterrupted 为 1607ms，App 显示 Stopped。
- 立即发送：父 PID 34663、子 PID 34664，同样跨 PGID 且忽略 TERM。Meta+Enter 后 1759.84ms 内两者均已退出；旧轮中断提交后，新轮回复 `RUST_SHELL_NOW_OK`。新轮创建时间晚于进程退出观测点，原生集成测试另在下一模型请求入口断言旧进程已消失。
- 冷恢复：运行 Rust PID 32579 → 35445；两条中断历史保留，续聊回复 `RUST_SHELL_RESUME_OK`，该轮工具调用为 0；旧测试父子进程仍不存在，Renderer errors 为 0。
- 以上均使用最终二进制，SHA256 `0d159b3bbccbd119eaf98440fa2c89ff40687b7048217f734d84548900e0dca4`，未回落到 TS。

证据文件为上述目录的 `shell-lifecycle-evidence.json`、`shell-lifecycle-renderer-errors.txt` 和截图 `28-shell-stop.png`、`29-shell-start-now.png`、`30-shell-cold-resume.png`。交互耗时包括自动化命令开销，不作为单独 UI 性能基准。

## 容量与剩余边界

继续关闭 Cargo 增量缓存（0 B），磁盘剩余约 12.03 GiB；本轮没有重新复制生产数据库。保留已提交迁移备份和实机证据，fixture 自动清理临时数据与进程。

本包验证 POSIX 已拥有的 Bash 进程生命周期，不是 OS 沙箱；快照前已脱离原 PGID 且完成 reparent 的 daemon 不在完整覆盖声明内，身份精度受系统启动时间字段限制。Windows 仍使用 taskkill，Windows/Linux 本机矩阵、PTY 全矩阵、跨平台发行与 release 性能仍待验证。AskUserQuestion、Todo/独立计划、其余 Coding 工具差分和扩展工具仍见 [剩余清单](../specs/rust-parity-remaining.md)，全量替换尚未完成。
