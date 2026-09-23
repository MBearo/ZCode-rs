# Rust 多模型协议交付记录

ZCode-Pro main / 872ad96，2026-09-22。基于前三包未提交工作；TS 默认、Rust 显式选择、权限仅 yolo。

## 已完成

原生配置支持 Chat Completions、Responses 和 Anthropic Messages。共用 HTTP client、一次 body 编码、SSE/16 ms 合并、重试/取消、模型与工具提交屏障。Responses 保存 encrypted reasoning items，Anthropic 保存 thinking 签名/redacted_thinking；只在对应模型请求回传，不进入 App 正文。工具参数分片和终态校验通过后才能执行。Anthropic 网关补 /v1，保留路径，发送版本及双鉴权头。仍仅配置一个模型。

## 验证

21 个 Rust 测试、46 个真实 Rust 子进程 App/Host 测试通过；其中新增 11 项协议场景，覆盖请求形状、headers、工具效果、推理冷恢复、重试 bytes、首段前后断流、空响应一次重试、读流取消、非法工具终态和 compact。新增 Rust 测试覆盖 URL 归一化、工具失败映射和元数据隔离。当前 App client/schema 无版本改动。

fmt/Clippy -D warnings、根 typecheck/lint、架构检查通过；lint 0 errors / 70 既有 warnings，架构 baseline/new 均为 0。Node SQLite ExperimentalWarning 为测试环境提示。Rust src/tests 相对本包开始快照 +799/-33，净 +766 行。

## 性能

Apple M1 Max / macOS arm64，release。每场景、每版本或协议五次，串行交替，共 90 次。两端 contextWindow=256000，同样文本片段、轮数和会话数；Responses/Anthropic 只更换 SSE 封装。传输字节数不同，跨协议比较反映本地 fixture 下的 adapter 与传输开销，不等于真实供应商性能。各列为中位数，RPC 为各次 p95 的中位数。

| 对比                  | 负载    | 总耗时 baseline→candidate |    启动 |  首轮首段 | 后续首段 | RPC p95 |  峰值 RSS |      存储 |
| --------------------- | ------- | ------------------------: | ------: | --------: | -------: | ------: | --------: | --------: |
| Chat 第三→第四包      | 8×2048  |          290.10→288.27 ms | 8.33 ms | 160.50 ms |  1.74 ms | 0.72 ms | 32.92 MiB | 2871456 B |
| Chat 第三→第四包      | 100×64  |          386.35→389.93 ms | 8.44 ms | 161.61 ms |  1.11 ms | 0.43 ms | 26.64 MiB | 5398056 B |
| Chat 第三→第四包      | 4×8×512 |          264.71→270.97 ms | 8.21 ms | 166.49 ms |  2.82 ms | 0.97 ms | 32.53 MiB | 6373144 B |
| 同产物 Chat→Responses | 8×2048  |          288.87→297.46 ms | 8.46 ms | 160.84 ms |  1.71 ms | 1.02 ms | 31.81 MiB | 2871456 B |
| 同产物 Chat→Responses | 100×64  |          376.80→399.11 ms | 8.05 ms | 164.15 ms |  1.15 ms | 0.43 ms | 27.50 MiB | 5414464 B |
| 同产物 Chat→Responses | 4×8×512 |          263.29→268.45 ms | 8.21 ms | 163.83 ms |  2.68 ms | 1.21 ms | 33.58 MiB | 6360808 B |
| 同产物 Chat→Anthropic | 8×2048  |          284.80→292.08 ms | 8.36 ms | 159.19 ms |  1.75 ms | 0.64 ms | 32.36 MiB | 2871456 B |
| 同产物 Chat→Anthropic | 100×64  |          384.43→404.93 ms | 8.12 ms | 164.04 ms |  1.19 ms | 0.43 ms | 26.50 MiB | 5410344 B |
| 同产物 Chat→Anthropic | 4×8×512 |          263.48→264.42 ms | 8.72 ms | 160.65 ms |  2.61 ms | 0.79 ms | 32.70 MiB | 6397840 B |

新 Chat 相对第三包总耗时分别 -0.6%、+0.9%、+2.4%；100 轮场景 Responses/Anthropic 相对同产物 Chat 为 +5.9%/+5.3%。不能据此宣称所有协议更快。RSS 为采样峰值，存储为 SQLite/WAL/SHM 占用而非物理写入量。

原始样本 `.zcode-runtime/rust-bench/protocols/{chat,responses,anthropic}`。第三包 SHA256 `3edd350b3a23bfef00a66f12e5a47736d93df8c89ec98e9ce0191c354829cc6a`，第四包 `994acfdc8ee28ac8275c16d4bab1c739a8642cdd8df47b3c1521556087b028ba`。

## 边界

本包未接入 App Registry/账户 overlay/请求期鉴权、动态模型切换、完整多媒体、provider hosted tools、非 SSE 回退或输出上限续写。没有访问真实模型或个人凭据；未完成 Electron Renderer E2E、Windows/Linux 实机、远端发行与旧 TS 数据迁移，不能宣称全量替换。
