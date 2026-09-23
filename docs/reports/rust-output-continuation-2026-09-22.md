# Rust 输出上限续写交付记录

ZCode-Pro main / 872ad96，2026-09-22。继续使用默认 TS、显式 Rust、yolo 权限。

## 行为

三协议显式输出上限终态进入同一 Agent loop 恢复路径：部分 assistant（含推理）先提交，收到 receipt 后最多续写三次；持续空截断也消耗次数，第四次返回 model_output_limit_exceeded。Continue 提示和当前位置只在 RunContext 存活，不成为用户输入、ACK 或 canonical 消息；冷恢复不自动续写。

续写仍经过预算及 micro/auto/reactive compact。摘要 offset 只统计 canonical 消息；压缩后保留尚需继续的提示。工具截断拒绝执行；摘要截断保留旧上下文。Anthropic 请求投影按 TS 规则移除没有正文/工具的孤立 thinking，保留 canonical 推理。常规请求保持批量 clone 历史，仅续写时合并临时提示。

## 验证

23 个 Rust 测试、57 个真实 Rust 子进程 App/Host 测试通过；本包新增 11 个 App 场景和 2 个 Rust 场景。覆盖三协议续写后成功、空截断预算耗尽、截断工具无副作用、自动压缩后的继续及冷恢复、截断摘要回滚。可控存储 gate 验证 partial 未提交时不发下一请求，以及提交失败时停止。未更改 App schema/version。

pnpm test:zcode-cli-rust、check:zcode-cli-rust、typecheck、lint、fmt:check、architecture:check --changed 均通过。lint 0 errors / 70 条既有 warnings；架构 baseline/new 均为 0；Node SQLite ExperimentalWarning 为现有测试环境提示。全仓格式检查发现并修正了本任务 Cargo.toml 的数组格式。Rust src/tests 相对第四包 +199/-34，净 +165 行。

## 性能

Apple M1 Max / macOS arm64 release，对照第四包。两端 contextWindow=256000，固定 stream 8×2048、history 100×64、sessions 4×8×512，串行交替各五次。负载不触发截断，用于评估新增功能对常规执行的开销，不代表真实模型续写总耗时。初测发现常规投影走了逐消息合并路径，增加无需续写时的批量 clone 路径后重新测量；两轮样本均保留。

| 场景     | 第四→第五包总耗时 |    启动 |  首轮首段 | 后续首段 | RPC p95 |  峰值 RSS |      存储 |
| -------- | ----------------: | ------: | --------: | -------: | ------: | --------: | --------: |
| stream   |  296.79→294.30 ms | 9.88 ms | 167.73 ms |  1.73 ms | 1.10 ms | 31.64 MiB | 2871456 B |
| history  |  440.34→429.63 ms | 8.63 ms | 177.17 ms |  1.23 ms | 0.49 ms | 29.27 MiB | 5422704 B |
| sessions |  268.43→266.07 ms | 8.20 ms | 163.44 ms |  2.63 ms | 1.36 ms | 32.33 MiB | 6401960 B |

各列是五次中位数，RPC 是各次 p95 的中位数。总耗时变化：stream: -0.8%，history: -2.4%，sessions: -0.9%。RSS 是采样峰值，存储是 SQLite/WAL/SHM 占用，不是累计物理写入量；本地运行存在调度抖动，不将小幅差异视作确定加速。

原始样本：`.zcode-runtime/rust-bench/continuation/{initial,final}`。第四包 SHA256 `994acfdc8ee28ac8275c16d4bab1c739a8642cdd8df47b3c1521556087b028ba`；第五包 `1023d6d32c0f6ba38a46ce91fcacf62548f9de2ac110042d9cdeed2c09de2040`。

## 剩余边界

账号 Registry/overlay/请求期鉴权与动态模型选择、目录级 rules/完整 prompt、多媒体及超长摘要分块、AskUserQuestion/Todo/plan、会话 fork/retry/edit/rewind/delete、MCP/Skill/子代理等扩展、旧 TS 数据迁移和跨平台发行仍未完成。本次没有访问真实供应商，未完成完整 Electron Renderer、Windows/Linux 和远端实机验收。不能宣称全量替换。
