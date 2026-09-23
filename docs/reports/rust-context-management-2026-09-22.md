# Rust 上下文与队列交付记录

ZCode-Pro main / 872ad96，2026-09-22，基于前两包未提交工作继续实现；默认 TS、Rust 显式选择，权限仅 yolo。

## 已完成

- 手动 compact 与 `/compact [instructions]`、FIFO 维护队列、自动预算压缩、未输出时一次反应式压缩；旧成功工具结果 request-local microcompact 保留最近五项及调用结构。
- ContextState 由 Session 唯一拥有，摘要边界与时间线同事务提交，收到 commit receipt 后才能请求下一步。完整历史保留；失败/取消不修改边界。摘要流隐藏，重试使用既有 apiRetry。
- 对齐当前 TS preflight 阈值与 UTF-16/3 估算，正文、推理、工具参数和定义进入预算。估算随 Session canonical 消息追加增量更新，冷恢复重建派生缓存。根 AGENTS.md 每次模型请求前刷新。
- held queue 保留/清空发送、确认集合过期拒绝；sendQueuedNow 提交预留后取消旧 run，等 Finished 后晋级，保留原始命令和 client 来源。清空/删除输入 ACK 与新的会话事实同事务提交。
- 沿用当前 App V4 schema，没有新增协议版本；队列仍为进程内状态，重启明确标记丢弃，不自动重跑工具。

## 验证

19 个 Rust 测试、35 个真实 Rust 子进程 App/Host 集成测试通过。新增验收包含手动/自动/反应式压缩、工具轮次分界、reasoning/Unicode 估算、错误与近期工具结果保护、摘要取消/失败/冷恢复、重复 ACK、FIFO、held queue 与立即执行。可控存储 fixture 证明摘要未提交时不发下一个请求；SQLite 故障触发器证明清空 ACK 与新输入同事务回滚。

cargo fmt、Clippy all-targets -D warnings、Rust 分层/400 行限制、测试 typecheck、根 pnpm typecheck、pnpm lint、architecture:check --changed 通过。Lint 0 errors / 70 条既有 warnings；架构 baseline 0 / new 0。Node SQLite ExperimentalWarning 为现有测试运行时提示。Rust src/tests 相对第三包开工快照 +1245/-104，净 +1141 行；未计 TS 测试、文档、benchmark 脚本。

## 性能

Apple M1 Max / macOS arm64；release 串行交替，每版本、每场景五次。仍为相同固定 SSE 负载：stream 8×2048、history 100×64、sessions 4×8×512。第三包显式 contextWindow=256000，以容纳原固定流式历史；第二包没有 token 预算配置，只有限制请求字节数。未修改生产默认 200000。该测量不触发摘要，用于评估新增上下文管理的运行时开销，不代表摘要吞吐或真实模型耗时。

最初测量发现重复 token 扫描；修正为 Session 增量估算后，最终样本如下。各列为中位数；RPC 为每次 p95 的中位数。

| 负载       | 第二包→第三包总耗时 |    启动 |  首轮首段 | 后续首段 | RPC p95 |  峰值 RSS |  存储占用 |
| ---------- | ------------------: | ------: | --------: | -------: | ------: | --------: | --------: |
| 固定流式   |    293.93→291.81 ms | 8.04 ms | 161.91 ms |  1.94 ms | 0.82 ms | 32.30 MiB | 2871456 B |
| 100 轮历史 |    380.41→392.94 ms | 7.77 ms | 158.78 ms |  1.16 ms | 0.47 ms | 27.41 MiB | 5426824 B |
| 四会话     |    269.32→266.99 ms | 8.44 ms | 163.82 ms |  2.67 ms | 1.37 ms | 32.45 MiB | 6336088 B |

长历史总耗时仍增加约 3.3%；后续首段分别较基线增加 0.23 / 0.07 / 0.49 ms。新增每次请求的 context usage 通知会增加协议帧。RSS 为采样峰值，存储是 SQLite/WAL/SHM 占用，不是累计物理写入量。不能据此宣称所有场景更快。

原始样本：`.zcode-runtime/rust-bench/context/final-incremental`，前两次优化过程也保留。第二包二进制 SHA256 `2a5b6118099074423577d6174765ea69f328adb2ed8531ba0648d5f2c3cded78`；第三包 `3edd350b3a23bfef00a66f12e5a47736d93df8c89ec98e9ce0191c354829cc6a`。

## 未完成

目录级 rules、完整 TS prompt、多媒体、长度续写和过长摘要分块；App Registry/restricted CEL option maps、账号 Overlay/请求期鉴权、模型切换和额外模型协议；AskUserQuestion/Todo/plan、fork/retry/edit/rewind/delete；扩展能力、TS 数据迁移、跨平台实机与发行。此记录只验收第三包，不能宣称全量替换。
