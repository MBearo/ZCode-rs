# Rust App stdio P0 交付记录

2026-09-22，ZCode-Pro `main` / `872ad96` 上的未提交工作区。三项 P0 的实现与本地自动化验收已完成；TS 仍为默认 runtime，Rust 显式启用，仅支持 yolo。完整产品替换仍受 P1/P2 与实机验收限制。

后续真实 Electron / GLM-5.3 验收及 `session/read` 修复见 [App 端到端记录](rust-app-e2e-2026-09-22.md)。下列测试数量、二进制哈希及性能数据保留为本包初次交付快照，不代表后续修复后的重新测量。

## 交付范围

| P0           | 已实现                                                                                                                                                             | 验证依据                                                                                                                                      |
| ------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------- |
| App 接入契约 | 复用既有 stdio/V4、存储握手、能力协商、命令 ACK 和 continuous/replayable 投影；补齐双向鉴权回复、workspace 文本生成/取消、模型连通性测试、旧附件读取及历史计划查询 | 真实 Rust 子进程、App client/schema/assembler、实际 Host 服务；取消、迟到回复、队列、重启、EOF/EPIPE、背压、双连接回归                        |
| 配置与模型   | 直接读取 App builtin/personal Registry；模板、账号覆盖、个人规则、手动配置、参数映射、默认选型、热更新、运行中模型/档位切换；每次 HTTP 尝试经 Host 获取临时凭据    | 与 TS ProviderConfigResolver 及 option-map 的差分；所有当前内置模板与代表模型；真实 AccountProviderConfigSource/Host 服务接入本地模型 fixture |
| 数据连续性   | 当前 TS SQLite 只读一致性备份和幂等导入；保留任务身份/类型/归档、选型、模式、消息、工具关联、中断结果、压缩边界、输入处置和附件快照                                | 实际 TS SessionStore 建库；续聊、重启、重复导入、源库字节不变、身份隔离、未知语义失败、附件授权及源缓存删除后读取                             |

Session actor 继续是选型、队列、历史和 ACK 的唯一所有者。选型提交后才生效，每个模型步骤绑定一份完整 Registry 快照；排队模型消失会保持队列并返回错误。账号配置按 builtin revision 原子配对，凭据只在请求期间存在，不进入数据库、队列和日志。模型/工具结果仍等待持久化 receipt 才进入下一步。

`runtime/capabilities.accountProviderConfig` 仅在 Registry 模式为 true；静态 JSON 模式不消费账号覆盖。`independentPlanState=false`，Plan 执行仍明确拒绝。`process/childProcesses` 的既有 schema 表示 MCP 子进程；当前未启用 MCP，返回空列表，不把 Shell 进程伪装成 MCP。

`workspace/generateText`、`workspace/cancelGenerateText` 和 `provider/testModelConnectivity` 使用同一模型/鉴权/取消端口，不创建持久会话、不执行工具。`v4/attachment/read`、`v4/attachment/previewSource`、`v4/conversation/attachmentRead`、`v4/conversation/attachmentStat` 对已导入附件按 session/row/index 授权。历史 `ExitPlanMode` 内容可通过既有 plans 查询读取，不代表支持新的计划执行。

## 验证结果

- `pnpm test:zcode-cli-rust`：25 个 Rust 测试、73 个 App 集成测试通过。新增配置差分、Host 鉴权及迁移场景；既有模型请求、工具、上下文、提交屏障和传输回归继续执行。
- `pnpm check:zcode-cli-rust`：Rust 边界检查、fmt、所有 target 的 Clippy `-D warnings` 通过。
- `pnpm typecheck`、`pnpm lint`、`pnpm fmt:check`、`pnpm architecture:check --changed` 通过；架构 baseline/new 均为 0。
- lint 为 0 errors / 70 条既有 warnings；Node SQLite ExperimentalWarning 为测试环境既有提示。
- release 构建及 `dev-desktop:rust --help` 验证通过。未启动完整 Electron Renderer，也未访问真实模型供应商。

本包未更改 App 协议版本。主要验收代码位于 `packages/services/tests/zcode-cli-rust-{registry,registry-parity,options,account-host,migration,migration-boundaries}.test.ts`；底层端口和状态约束见 `apps/zcode-cli-rust/CONTRACT.md` 及 `docs/specs/rust-app-p0.md`。

改动归属为 zcode-cli-rust，以及既有 Host/Agent 服务启动和 capability 边界；状态所有者未迁移到 Host。相对 HEAD，整个尚未提交的 Rust `src/tests/examples` 共新增 59 个文件、10467 个物理行（含注释/空行）；这包含前五包，不能当成本次 P0 的净增行数。

## 性能对比

Apple M1 Max / macOS arm64，release；P0 前第五包二进制与本包候选串行交替，每场景各五次，共 30 次。两端 `contextWindow=256000`，固定 stream 8×2048、history 100×64、sessions 4×8×512。初次未指定窗口的运行因默认 200k 无法容纳固定负载而终止，不计入比较；没有修改生产默认窗口。

表中均为基线→本包的中位数；RPC 为五次运行各自 p95 的中位数，后续首段为所有非首轮样本的中位数。首轮包含首次 HTTP 初始化，fixture 不模拟真实供应商推理时间。

| 场景     |   启动 ms |   首轮首段 ms | 后续首段 ms |     总耗时 ms | RPC p95 ms | 峰值 RSS MiB |          存储 B |
| -------- | --------: | ------------: | ----------: | ------------: | ---------: | -----------: | --------------: |
| stream   | 8.96→9.14 | 180.08→175.03 |   1.94→1.92 | 326.37→323.57 |  1.16→1.41 |  33.41→34.80 | 2871456→2871456 |
| history  | 8.83→9.67 | 179.37→181.42 |   1.33→1.35 | 455.75→461.70 |  0.59→0.65 |  31.52→31.02 | 5402176→5422776 |
| sessions | 8.80→8.67 | 173.35→175.20 |   2.98→3.10 | 280.71→286.94 |  1.45→1.59 |  33.23→33.45 | 6360760→6385504 |

总耗时变化分别为 -0.9%、+1.3%、+2.2%。未观察到数量级退化，不将小幅差异解释为确定加速。RSS 为采样峰值；存储是 SQLite/WAL/SHM 文件占用，不是累计物理写入。该负载使用静态模型配置，只验证 P0 改动对已有请求/流式/存储主循环的影响，不证明 Registry 刷新、账号服务或大旧库迁移的性能，也不是 TS/Rust 性能比较。

原始样本位于 `.zcode-runtime/rust-bench/p0-20260922/`；可用 `scripts/bench-zcode-cli-rust-suite.mjs <baseline> <candidate> <output> 256000 256000` 重现。

- 基线 SHA256：`1023d6d32c0f6ba38a46ce91fcacf62548f9de2ac110042d9cdeed2c09de2040`，本次构建前保留的第五包 release 产物。
- 本包 SHA256：`bf587951abfec3b619d05560042703d737efef8e1a24997adfaabc3bf2843754`。

## 启用、迁移与剩余边界

运行 `pnpm dev:desktop:zcode-cli-rust` 使用现有 App 配置和账号。`--data-dir <directory>` 隔离实验数据；`--config <model.json>` 保留静态 fixture 模式。普通 `pnpm dev:desktop` 仍为 TS。

Rust 默认使用独立 `~/.zcode/rust/rust-sessions.sqlite`。首次打开 workspace 时从既有 TS 路径只读导入，保留 `ts-backup-*.sqlite` 和附件字节快照。非 yolo 历史不能静默提升权限；旧 planEnabled 必须通过显式关闭计划后才可继续。回退 TS 时退出 Rust、取消 runtime override，TS 继续读取原库；Rust 新增历史不反向同步，已导入 workspace 不自动合并之后的 TS 修改。

导入面向当前 TS schema；旧 schema 需先运行原 TS 迁移。支持旧文本、图片和 PDF 附件；远端 URL 保留引用，不离线下载。缺失本地附件、不支持的 MIME 或未知持久化语义会在 startup/storageState 报错并停止，不以缺失上下文继续。原始备份保留全部源表；未接入的高级状态不因此获得执行能力。

仍需完成 P1/P2：新附件/context refs、目录规则与完整 prompt/记忆、常用会话操作及交互工具、Coding 差分、MCP/Skill/子代理/工作流等。部分高级 UI 入口尚未按能力隐藏；现有协议未支持的命令明确拒绝。用户编辑的任意 JS 正则/扩展表达式未宣称全量兼容，当前内置规则和产品手动配置已覆盖差分。

切换默认前还需真实 Electron Renderer、真实供应商/账号、手机及远程恢复、Windows/Linux 发行、大旧库启动/按需加载与内存上界、同条件 TS release 对照。当前启动仍加载 workspace 历史，首次导入会备份 TS 源库；本地小型 fixture 不能替代这些验收。本报告不宣称已经全量替换 zcode-cli。
