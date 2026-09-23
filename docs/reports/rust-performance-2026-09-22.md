# Rust stdio 性能与内存优化

2026-09-22。本次以关键功能包交付后的 Rust release 为基线，对相同功能的修改前后版本比较。非仓库首段回退已定位并修复；大历史空闲缓存与分页开销显著下降。TS 仍为默认 runtime，不把本次结果解释为已经快于 TS 或达到全量替换。

## 实现

| 路径       | 变更                                                                                                    | 保留的约束                                                                          |
| ---------- | ------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------------------------- |
| 模型请求   | ModelPort 消费 owned projection；附件直接填充该投影，编码后释放中间 JSON 树                             | canonical 仍由 Session actor 拥有，三协议/reasoning/附件不变，重试复用 bytes        |
| Store      | 待提交 rows/messages/history 直接编码一次，不克隆第二份 Value 树；SQLite 相同 row/history 不重复 UPDATE | 输入、ACK、工具/模型结果和历史切断的事务屏障不变                                    |
| 空闲缓存   | 同时限制 8 个会话和 16 MiB 估计驻留内存，LRU 淘汰；单个超大空闲会话也会释放                             | 运行、订阅、队列、上传、子任务和后台状态继续 pin；16 MiB 不是进程 RSS 硬限制        |
| 历史分页   | 从尾部逐行计数字节，去除全历史引用数组和反复编码缩小整页                                                | 200 行、900 KiB、beforeRowId、顺序、hasMore 与原路径一致                            |
| 环境初始化 | 物理 cwd 的祖先没有 `.git` 时跳过 Git 子进程                                                            | 显式 Git 环境、worktree `.git` 文件、符号链接和不确定权限继续走 Git；取消仍回收进程 |
| App 入口   | `pnpm dev:desktop:rust` 默认构建/启动 release，`--debug` 显式选择调试构建，关闭 incremental             | 普通 App 入口继续选择 TS；本次没有自动重启用户 App                                  |

分阶段 profiler 的单次结果：非仓库环境 snapshot 从 123.105 ms 降到 3.082 ms；HTTP client 系统证书初始化从 157.539 ms 到 152.123 ms，保留系统信任与 TLS 行为。Skill、MCP definitions、profile 发现合计约数毫秒。首段回退的大头是非仓库也启动系统 Git，不是扩展发现本身。

## 固定负载：同机 release 各五次

机器：Apple M1 Max，macOS 15.7.7 / arm64，Node 24.14.0。本地确定性 HTTP SSE、隔离 HOME/配置；基准期间无本任务构建和测试并行。顺序交错，取五次中位数。两端 contextWindow 均为 256000，负载在当前估算的上下文边界内。

- stream：1 会话 × 8 轮 × 2048 chunks。
- history：1 会话 × 100 轮 × 64 chunks。
- sessions：4 会话 × 8 轮 × 512 chunks。

| 修改前 → 修改后 | 启动 ms     | 首段 ms         | 总耗时 ms       | 整体 RPC p95 ms | 结束 RSS MiB  |
| --------------- | ----------- | --------------- | --------------- | --------------- | ------------- |
| 固定流式        | 8.47 → 8.69 | 240.81 → 153.40 | 385.86 → 298.14 | 2.17 → 2.19     | 37.00 → 35.94 |
| 100 轮历史      | 8.99 → 9.17 | 237.14 → 152.16 | 583.40 → 483.12 | 0.74 → 0.70     | 28.91 → 26.23 |
| 4 会话          | 8.52 → 8.51 | 247.85 → 154.67 | 362.30 → 268.57 | 1.99 → 1.44     | 37.80 → 35.20 |

首段降低 36%–38%，总耗时降低 17%–26%；100 轮历史 RSS 降低 9.2%。协议帧数分别保持 177、601、324。

启动指标是 spawn 到 `runtime/capabilities` RPC 返回，包含 storage-ready 等待；不等同于单独的 SQLite ready 通知。首段包含本进程第一次 client/上下文初始化。后续轮首段分别为 2.10 → 2.13、0.71 → 0.70、1.44 → 1.40 ms。

整体 RPC 混入第一次初始化期间的空闲响应，因此额外记录至少一个会话进入第二轮后的 RPC p95：流式 6.52 → 6.96 ms，历史 0.75 → 0.72 ms，四会话 3.13 → 2.97 ms。**流式稳定阶段 p95 仍有约 0.44 ms 回退，本次不宣称所有延迟指标均改善。** 流式每次仅 18–21 个稳定阶段控制样本，不能据此推断生产长尾。

| 文件占用，修改前 → 修改后 | 运行末尾 SQLite/SHM/WAL MiB | 正常 EOF 后持久文件 MiB |
| ------------------------- | --------------------------- | ----------------------- |
| 固定流式                  | 6.490 → 6.486               | 2.223 → 2.223           |
| 100 轮历史                | 5.328 → 5.383               | 1.461 → 1.461           |
| 4 会话                    | 6.422 → 6.445               | 2.473 → 2.473           |

WAL 占用受 checkpoint 和页面复用影响，不是累计物理写入字节。元数据修改的故障 trigger 已证明不再 UPDATE 相同 row/history；本次没有缩减持久事实，退出后的数据库大小相同。

## 大历史内存与分页：真实 App client/schema 各五次

使用真实 Rust 子进程、现有 ZCodeProtocolClient 和 App runtime schemas。十二个会话各注入 4 MiB canonical 历史；启动只读 metadata，依次冷读取后测量，再保留一个订阅重复读取其他会话。数据库 fixture 由实际完成的会话派生；大 canonical 仅用于冷加载，不送入模型。OS 文件缓存保持自然状态，没有清空机器全局缓存。

| 指标，中位数         | 修改前    | 修改后    | 变化     |
| -------------------- | --------- | --------- | -------- |
| 启动 RSS             | 8.30 MiB  | 8.08 MiB  | 基本持平 |
| 依次读取后的空闲 RSS | 52.77 MiB | 38.72 MiB | -26.6%   |
| 保留一个订阅时的 RSS | 71.88 MiB | 47.31 MiB | -34.2%   |
| 冷加载 p95           | 2.89 ms   | 2.90 ms   | 基本持平 |
| 大页 rowsRange       | 312.34 ms | 5.27 ms   | -98.3%   |

大页场景额外准备 200 条各 32 KiB 的展示行，订阅 pin 后每次取 limit=200 的尾页；修改前后均返回相同 27 条行和 hasMore=true。每进程测三次，先取进程内中位数，再取五个进程的中位数。全部冷查询不触发新模型请求；订阅保护的 epoch 在压力后保持一致。

RSS 是操作结束后的 `ps` 采样，采样最大值不代表分配瞬间的真实峰值。活跃/订阅会话仍会加载其完整历史；分段读取单个超大会话、文件备份保留和未引用 blob GC 继续单列。

## 验证与清理

- 59 个 Rust 测试通过；新增容量估算与精确分页边界测试。
- 188 个 App 集成测试通过，包含三协议、附件、reasoning、取消、故障提交屏障、MCP/Skill/子代理/Goal、历史操作、冷恢复、双连接、EOF/EPIPE 和背压。
- 新性能 fixture 的三组场景全部通过；增加大历史解除订阅后重新逐出验证。慢 Git 取消 fixture 显式创建 `.git`，确保优化后仍实际进入等待，未放宽超时断言。
- Rust fmt/Clippy（`-D warnings`）、root typecheck/lint/fmt、测试 tsconfig 与架构检查通过。架构 baseline/new 均为 0。lint 原有 70 warnings、0 errors；Node SQLite 的 experimental warning 仍存在。
- 清理 Cargo deps 中 22:00 前的旧项目对象/库/测试产物：57,829 个文件，逻辑大小 8.19 GiB；debug 目录从 8.5 GiB 降到 3.0 GiB，可用磁盘从约 10 GiB 增至 16 GiB。当前 debug/release 可执行文件、依赖和用户数据库保留。

本次验证到真实子进程及 App client/schema，未重新执行 Electron GUI 点击回归，也未覆盖 Windows/Linux 原生执行或真实供应商延迟。

## 复现与证据

```sh
CARGO_INCREMENTAL=0 cargo build --release --locked --manifest-path apps/zcode-rust/Cargo.toml --bin zcode-rust
node scripts/bench-rust-agent-suite.mjs .zcode-runtime/rust-perf-20260922/baseline apps/zcode-rust/target/release/zcode-rust .zcode-runtime/rust-perf-20260922/final 256000 256000
TSX_TSCONFIG_PATH=packages/services/tests/tsconfig.rust-agent.json node --import tsx scripts/bench-rust-session-memory.mjs .zcode-runtime/rust-perf-20260922/baseline apps/zcode-rust/target/release/zcode-rust .zcode-runtime/rust-perf-20260922/memory
```

二进制 SHA-256：

- baseline：`d12c97f1455b773da0290a0c9384cc6c7503e7ed0ae80546ffd850fe410f9506`
- candidate：`baa5b9d407b40bd29b321b43c527091b3b15f049cf170b1025dff19e15c57c36`

原始样本在 `.zcode-runtime/rust-perf-20260922/final/`、`memory/`；首次固定负载测量保留于 `results/`。阶段 profile、清理清单和验证日志也在该目录。规范见 [性能 spec](../specs/rust-runtime-performance.md)，残余功能见 [剩余清单](../specs/rust-parity-remaining.md)。
