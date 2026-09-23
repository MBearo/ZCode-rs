# Rust M0 核心解耦验收报告

日期：2026-09-23。规格：[rust-m0-core-decoupling.md](../specs/rust-m0-core-decoupling.md)。分支 `rust-p0-p1`，提交 `17232ed`..`900e25a`。

## 交付

| 项                           | 提交      | 结果                                                                                              |
| ---------------------------- | --------- | ------------------------------------------------------------------------------------------------- |
| 命令行子命令与 TUI 入口      | `17232ed` | `app-server` 参数不变；`tui` 占位，退出码 2，不写 stdout                                          |
| 错误模型、连接模型与投递迁移 | `b92ba13` | core 只产出类型化事件；路由、订阅、分帧、写出迁到 app-server；错误码对齐 Node；分片按三种载体测量 |
| 等待者统一收口               | `1a5ee34` | 权限、问答、Host 请求进入 `Waiters`，唯一收口入口                                                 |
| 状态枚举                     | `5b32f49` | `Mode`、`Phase` 枚举，序列化字符串不变                                                            |
| 命令 admission 校验          | `3a456d4` | 以 TS zod 导出的 JSON Schema 为唯一来源，漂移检查纳入测试；非法命令回 `proto.invalidPayload` ACK  |
| 日志                         | `900e25a` | 非阻塞 JSONL，字段与脱敏对齐 Node                                                                 |

## 验证

- `pnpm test:zcode-cli-rust`：Node 驱动集成测试 190/190 通过；`cargo test --workspace` 83/83 通过（原 63 个，新增 20 个）。
- `pnpm check:zcode-cli-rust`（rustfmt、clippy `-D warnings`、边界检查）、`pnpm typecheck`、`pnpm lint`（0 error，70 条既有 warning）、`pnpm architecture:check --changed` 均通过。
- `pnpm fmt:check` 在基线上已有 1013 个文件不合格，本次改动涉及的文件单独检查均通过。
- 手动冒烟：未知方法返回 `-32601 Method not found`，stdout 只有协议帧，日志写入 `zcode-rust-<日期>.jsonl`，EOF 后进程正常退出。
- 集成测试调整：
  - 夹具按真实 Host 的 CAS 方式补齐 `baseRevision`（取当前修订号，stale 时重试）；
  - 缺 `baseRevision` 的用例改为断言 `proto.invalidPayload`；
  - resync 的错误文本改为 Node 的 `fault.subscription.notOwned`；
  - 非法共享引用允许 rejected ACK 或 RPC 错误两种结果。

## 性能

同机 Apple M2 Pro、macOS arm64，release 构建。基线为 M0 前的 `796f74e`，候选为 `900e25a`。使用 `scripts/bench-zcode-cli-rust-suite.mjs`，三个场景各 5 轮，基线与候选交替运行，下表为中位数。

| 场景                       | 指标                 |        基线 |        候选 |                  变化 |
| -------------------------- | -------------------- | ----------: | ----------: | --------------------: |
| stream（8 轮 × 2048 片段） | 总时长               |    298.9 ms |    305.5 ms |     +2.2%（噪声范围） |
|                            | 流式期间控制 RPC p95 |     5.10 ms |     3.24 ms |                −36.5% |
|                            | 峰值 RSS             |    36.9 MiB |    39.1 MiB |              +2.2 MiB |
| history（100 轮）          | 总时长               |    462.5 ms |    433.8 ms |                 −6.2% |
|                            | 控制 RPC p95         |     0.67 ms |     0.57 ms |                  −15% |
| sessions（4 会话）         | 总时长               |    246.9 ms |    245.6 ms |                  持平 |
|                            | 流式期间控制 RPC p95 |     2.87 ms |     2.17 ms |                −24.4% |
| 全部                       | 启动                 |  8.8–9.3 ms |      9.6 ms |            约 +0.5 ms |
| 全部                       | 空闲 RSS             | 8.5–8.8 MiB | 9.2–9.6 MiB | 约 +0.8 MiB，5 轮一致 |
| 全部                       | release 二进制       |     19.2 MB |     19.5 MB |               +0.3 MB |

- 流式期间控制 RPC 延迟明显下降：actor 不再等待 stdout 写出，写出按批 flush。
- 空闲 RSS 增加约 0.8 MiB，主要来自日志写线程、时区数据加载与 app-server 事件循环，仍远低于 50 MiB 的预算。
- 协议帧数不变（基准未订阅 sessions-index，因此 index 去重在此不体现）。
- 首轮 TTFT 三个场景中有升有降（140.7/148.8、138.0/140.9、142.0/139.1 ms），属于噪声范围。

## 限制

- 订阅仍只发快照：日志回放、按 profile 过滤与缓冲在 M8 实现。
- 命令处理仍读取 `Value` payload；admission 已按 TS schema 校验，类型化 payload 随各命令改写时推进。
- 日志 7 天保留在 M9 实现。
