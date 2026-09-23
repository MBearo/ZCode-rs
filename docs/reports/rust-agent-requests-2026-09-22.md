# Rust 请求与 Agent loop：交付及性能记录

日期：2026-09-22。源码基线 `main` / `872ad96`，在此前未提交的 Rust core 上继续实现。默认 TypeScript runtime 保持不变。

## 已交付

- HttpModel 按需初始化并复用连接池；系统证书读取移至阻塞线程池；每个模型步骤只编码一次 HTTP body，重试复用 bytes。
- 线性 SSE、UTF-8/CRLF/多事件合包、独立工具参数 String 汇编、完整调用校验。首段即时发送，后续按 16 ms/8 KiB 合并；reasoning_content 持久化并回传。
- 结构化错误、已知业务码/额度/上下文/HTTP/网络/TLS 分类、CLI 默认重试预算与退避、Retry-After、空响应预算、闲置/显式总超时和取消。非空正文或推理交付后不透明重放。
- 使用现有 control.apiRetry 展示并清除等待状态；无 App wire/schema 版本变化。
- 模型/工具耐久提交屏障；Read/List 最多四并发，结果保持原顺序，写/Shell 为屏障；一次权限提交成功后才执行副作用。
- 存储失败后只取消和收口，不再次提交失败的内存事实；迟到事件仍校验 run generation。读取工具和 AGENTS 拒绝特殊文件。

## 正确性验证

| 验证                 | 结果与证据                                                                                                                                                                                                                                                   |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Rust 单测            | 8/8：SSE 分片/大小边界、重试曲线、Retry-After、无隐式总截止、旧 native 数据迁移、文件与 Shell 取消                                                                                                                                                           |
| 受控 runtime/storage | 3/3：输入/模型/工具结果提交失败不提前执行、不复活事实；四只读并发与写屏障；权限批准只提交一次且失败不产生副作用；同时注入旧 run 事件                                                                                                                         |
| App/native 集成      | 23/23：实际 Rust 进程 + 当前 TS ProtocolClient/schema/assembler/Host，包括 socket reset、503、SSE 错误、限流、额度终止、空响应、reasoning 回传/冷恢复、HTTP 复用和请求字节一致、首段后断流、取消、闲置 flush、错误工具调用、权限/队列/EOF/EPIPE/大帧和双连接 |
| TLS                  | 本地临时自签名 HTTPS：证书失败只尝试一次，reason=tls_error，错误不包含 endpoint/原始响应；按类型展开两层 io::Error                                                                                                                                           |
| 静态检查             | cargo fmt、Clippy all-targets（-D warnings）、Rust 源码边界、测试 TS 类型检查、根 typecheck 通过                                                                                                                                                             |
| 仓库检查             | lint 0 errors / 70 个既有 warnings；architecture baseline 0 / new 0；变更格式与 git diff --check 通过                                                                                                                                                        |

运行命令：`pnpm test:rust-agent`、`pnpm check:rust-agent`、`pnpm typecheck`、`pnpm lint`、`pnpm architecture:check --changed`。测试脚本重建真实 debug 二进制，无个人账号或外部模型请求。

## 性能方法与产物

机器为 Apple M1 Max，darwin/arm64；Node 24.14.0、pnpm 10.33.2、rustc 1.95.0。两版均为本机 release 构建。baseline 是本交付包开始前、已经包含增量存储优化的 Rust 版本；这不是 TypeScript 对 Rust 的语言性能比较。

- baseline SHA-256：`36e482890bc0d9d94e6e0a686144c693e376b2e9ab57f2740700f508313c1a3d`。
- candidate SHA-256：`518fca0037b6467f0429cae984b0fe859ada4b610c2a5893369d38f460a6aadf`。
- 每个场景、每个版本各 5 次；串行运行，交替先后次序，共 30 个样本。每次使用新的 workspace/SQLite；运行时没有并发执行仓库构建或测试。
- 本地确定性 SSE fixture 不模拟真实供应商推理时间。每个片段为同一 68-byte UTF-8 文本；协议解析/文件落盘均为真实路径。
- 报告耗时、启动、RSS、文件占用采用五次中位数。RPC 列为每次运行 p95 的中位数。RSS 为约数十毫秒间隔及结束时采样的峰值，不能视作 OS 精确最大 RSS；文件占用包括 SQLite、SHM 和 WAL，不能视作累计写入字节。

复现：

```sh
cargo build --locked --release --manifest-path apps/zcode-rust/Cargo.toml
node scripts/bench-rust-agent-suite.mjs \
  .zcode-runtime/rust-bench/request-baseline \
  apps/zcode-rust/target/release/zcode-rust \
  .zcode-runtime/rust-bench/requests-final
```

原始 30 个 JSON 和 `summary.json` 位于上述输出目录；baseline 二进制在 `.zcode-runtime/rust-bench/request-baseline`。这些工作区产物不参与发布。

## 五次重复结果

| 场景                      | baseline 总耗时 | candidate 总耗时 | 加速比 | candidate RPC p95 | candidate 采样峰值 RSS |
| ------------------------- | --------------: | ---------------: | -----: | ----------------: | ---------------------: |
| 1 会话 × 8 轮 × 2048 片段 |      2754.76 ms |        269.14 ms | 10.24× |           0.93 ms |              27.20 MiB |
| 1 会话 × 100 轮 × 64 片段 |     15038.06 ms |        364.38 ms | 41.27× |           0.43 ms |              25.08 MiB |
| 4 会话 × 8 轮 × 512 片段  |      2434.81 ms |        251.76 ms |  9.67× |           1.38 ms |              28.80 MiB |

| 指标                 | 固定流式 baseline → candidate | 长历史 baseline → candidate | 多会话 baseline → candidate |
| -------------------- | ----------------------------- | --------------------------- | --------------------------- |
| 首个 RPC             | 7.13 → 7.07 ms                | 7.32 → 8.06 ms              | 7.43 → 7.33 ms              |
| 首轮首段             | 149.91 → 152.03 ms            | 152.17 → 156.75 ms          | 175.91 → 155.60 ms          |
| 后续轮首段中位数     | 137.93 → 1.43 ms              | 140.73 → 0.98 ms            | 170.28 → 2.11 ms            |
| RPC p95              | 0.39 → 0.93 ms                | 0.22 → 0.43 ms              | 0.62 → 1.38 ms              |
| Conversation wire 帧 | 16409 → 169                   | 6701 → 501                  | 16484 → 292                 |
| 空闲 RSS             | 6.05 → 5.98 MiB               | 6.05 → 6.02 MiB             | 6.02 → 6.00 MiB             |
| 采样峰值 RSS         | 31.19 → 27.20 MiB             | 30.28 → 25.08 MiB           | 37.14 → 28.80 MiB           |
| 数据文件占用         | 3.32 → 2.74 MiB               | 5.16 → 5.16 MiB             | 5.75 → 6.04 MiB             |

连接复用消除了后续轮反复读取系统证书/初始化 HTTP client 的成本；合并输出大幅减少了投影与协议帧。首轮首段仍包含初始化成本，没有同步获得数量级提升。控制 RPC 的 p95 略有上升，仍在本地目标 20 ms 内；多会话的文件占用略增，不能声称所有指标都改善。

## 交付限制

本包实现的是单一 Chat Completions 配置下的请求与 loop；完整 TS SDK 的全部供应商特例、App Registry/账号动态鉴权、多协议、自动压缩、规范工具扩展和旧 TS 会话迁移仍按后续计划推进。真实供应商、完整 Electron Renderer、Windows/Linux 实机和远端发行未在本包验证。当前收益反映确定性本地流式处理开销，不能推广成真实模型生成速度或端到端生产性能承诺。
