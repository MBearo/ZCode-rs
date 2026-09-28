# Rust stdio 剩余对齐清单

2026-09-23 范围调整：目标扩大为用 Rust 完整替换 Node CLI。权限模式（build/edit/plan/yolo/auto）纳入 P0；TUI 只保留子命令入口；动态工作流不做。P0/P1 的架构、刻意差异与里程碑以 [P0/P1 架构设计](rust-p0-p1-architecture.md) 为准，下文 2026-09-22 的"只支持 yolo"限制随 M2 交付解除。

2026-09-22，目标是替换 App 使用的 stdio runtime。权限只支持 yolo，不做 TUI；默认仍为 TS，Rust 显式选择。用户指定的关键工作包已实现并经过核心验收，不能再把 MCP、Skill、子代理、Goal 和历史操作笼统列为“未实现”。证据见 [核心交付报告](../reports/rust-critical-parity-2026-09-22.md)。

## 核心能力交付快照（2026-09-22）

下表保留当时的交付范围；其中 Goal 验证、存储与历史边界等后续变化以 M11 及下方 2026-09-28 更新为准。

| 能力             | 已实现与验证                                                                                                                                | 剩余边界                                                                       |
| ---------------- | ------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| Session 按需加载 | 启动只读 metadata index、ACK 按 key 查询、单会话激活、8 个/16 MiB 空闲会话 LRU；300 会话故障/驻留测试                                       | 单个超大会话仍整体加载；活跃/有订阅会话按真实需求驻留                          |
| MCP              | 真实 stdio、Streamable HTTP、legacy SSE；工具发现/调用、连接复用、取消、进程回收、失效配置隔离；插件 options 模板与官方鉴权（M10）          | OAuth、legacy SSE 自动重连与全部服务器兼容矩阵                                 |
| Skill            | 用户/项目/启用插件发现；会话冻结 metadata、调用时读正文、冷恢复                                                                             | 主代理记忆系统                                                                 |
| 子代理           | Agent/Task、SendMessage、TaskOutput/TaskStop；前后台、真实 child Session、profile/model/tools/maxTurns、继承 Skill/MCP、memory 文件、取消树 | 完整 JSONL transcript artifact、子树预算聚合和更多平台验证                     |
| Goal             | 设置/推进/隐藏验证/继续、暂停恢复、队列/后台等待、预算与冷恢复；真实 App verified                                                           | 无效 verifier JSON 保留失败状态，明确区别于 TS 的宽松通过规则                  |
| 重试与编辑       | 原 canonical 意图、附件快照、Goal；同会话历史截断，独立 ACK 凭据、新 epoch 与迟到事件隔离                                                   | 旧历史缺少可证明边界时拒绝；当前 App 产品 UI 不渲染普通 retry 按钮，协议已验收 |
| 分支             | 稳定回复边界、父会话运行中 fork、隔离队列和执行状态、child 与 ACK 原子提交                                                                  | 更多历史导入来源的边界迁移                                                     |
| 文件回退         | Write/Edit 字节/hash/权限 checkpoint、去重 blob、外部修改冲突、恢复 journal、提交失败补偿；App 摘要/diff/编辑并回退                         | 不追踪 Shell 或外部工具任意改文件；保留策略和未引用 blob 回收                  |

2026-09-22 性能更新：首段降低 36%–38%，12 会话空闲 RSS 降低 26.6%，大页查询 312 → 5.3 ms；同机 release 各五次，完整口径及剩余回退见 [性能报告](../reports/rust-performance-2026-09-22.md)。

## P0/P1 交付记录（2026-09-24）

[P0/P1 架构设计](rust-p0-p1-architecture.md) §9 的里程碑 M0–M10 均已交付。各期的刻意差异写在对应分项 spec 的"与 Node 的差异"一节。本轮提交如下：

- M10.3（`6f9082b`）：插件写配置、卸载、恢复内置，以及存储锁。
- M10.4a（`0860fa8`）、M10.4b（`494aa5a`）：市场、安装、更新、校验、描述；zip 与 GitHub archive 来源；建议引用。见 [M10.4 spec](rust-m10-4-plugin-sources.md)。
- M10.5（`2924783`）：官方 MCP 鉴权，见 [M10 spec](rust-m10-plugins.md) §3.11。
- M5.2c（`04cacae`）：按工具的结果预算，见 [M5 spec](rust-m5-tools.md) §2.3。

验证：Node 驱动的集成测试 297 个全部通过（App schema 严格校验），cargo 单测 305 个通过，typecheck、lint、fmt、架构检查通过。

## 当前进度（2026-09-28）

以下更新依据当前源码、对应测试和已提交的验收记录；上述测试数量只代表 2026-09-24 当次运行。

- 权限模式 build/edit/yolo/auto、独立 Plan 状态、审批、hooks 与工作区信任已交付，见 [M2](rust-m2-permissions-plan-hooks.md)。
- Edit 宽松匹配、WebFetch/WebSearch、图片/视频/PDF Read、图片缩放与请求媒体预算已交付，见 [M5](rust-m5-tools.md) 与 [M11](rust-m11-node-storage.md) §5.3。
- V4 按 desktop-continuous / web-remote-replayable 分 profile 投递、增量恢复和流控已通过协议集成测试；真实远端/手机全链路仍待验收，见 [M8](rust-m8-delivery.md)。
- WebSearch 嵌套用量已按 Node 规则计入本轮、目标及子代理用量；不会计入 `model_usage` 或 V4 usage。用量差分、实时遥测、MCP 进程遥测和本地首字时间已补齐，见 [M9](rust-m9-usage-logs.md)。
- 历史单向迁移限制已解除：Node 会话库成为唯一存储，Node/Rust 交替续写、崩溃恢复、不同会话并发写入及真实库比对通过。目标暂停/恢复/清除的模型提醒、选区侧聊与自动标题也已交付，见 [M11](rust-m11-node-storage.md) §5.4、§10–11。
- 原版 ZCode 3.14.3 可通过 npm 启动器显式选择 Rust。macOS ARM64、Windows x64 平台包已公开发布；macOS 原版 App 的 Rust → Node → Rust 基础切换通过，Windows 仅覆盖原生构建、公开安装、exe 与启动器 fixture，尚未验证真实桌面 UI。见[启动器报告](../reports/rust-npm-app-launcher-2026-09-28.md)。

动态工作流不在当前迁移范围，TUI 只保留子命令入口；自动任务、Cron/OffPeak、浏览器/CUA、Node REPL 与主代理记忆仍待实现。默认 runtime 保持 TypeScript。

### 待用户决定（当前保持 Node 行为）

| 编号 | 问题                                                                                                                                                          | 建议                                                     |
| ---- | ------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------- |
| D14  | 带文件描述符前缀的进程替换（如 `cat 2<(touch x)`）被判为只读，build / edit 模式下免审批执行（已实测）。见 [M2 spec](rust-m2-permissions-plan-hooks.md) §8     | 解析层把 `\d+[<>]\(` 视为不可判定的动态词，Node 同步修复 |
| D16  | `turn.started`、`tool.updated started` 带 strict schema 之外的字段，Host 丢弃整条事件，手机因此收不到这两类事件。见 [M3 spec](rust-m3-legacy-session.md) §9.6 | 是否改为只发 schema 内字段                               |

### 未完成事项（2026-09-28）

| 优先级   | 事项                     | 说明                                                                                                                            |
| -------- | ------------------------ | ------------------------------------------------------------------------------------------------------------------------------- |
| P1       | M3.3b-2：legacy 事件差分 | legacy `TargetChanged` / 压缩事件仍需核对；目标暂停、恢复、清除的模型提醒已由 M11.4m 交付                                       |
| P1       | MCP OAuth                | authorization_code / client_credentials 未实现；声明了 `oauth` 的 MCP（含插件）被禁用，不会以无凭据方式连接                     |
| P2       | M4b 差分脚本             | `scripts/diff-zcode-cli-node-rust.mjs`：同一 fixture 下比较 Node 与 Rust 的 `-p` 输出                                           |
| 发布门槛 | 官方插件资产随 Rust 打包 | Rust 只读取已写入存储的官方插件缓存，写入仍由 Node CLI 启动时完成；`restoreBuiltin` 也不会重新写入                              |
| 发布门槛 | 默认切换与完整发行验收   | npm 启动器仅适配原版 ZCode 3.14.3，本地切换不影响 SSH/WSL runtime；Windows 桌面端到端、其余平台、升级回退与供应商矩阵未全部验收 |

### 尚未在真实 App 与真实账号下验证

- 插件商店 UI 全流程：添加市场、安装、更新、卸载、恢复内置、取消操作。
- 官方 MCP 鉴权：需要 Host 下发真实凭证，也需要真实的官方 MCP 端点。
- 真实负载下大工具结果的截断与落盘。

以上能力目前只有集成测试覆盖：测试驱动真实 Rust 二进制与 App 协议，但模型、Host 与服务端都是夹具。

### 已知测试抖动

`zcode-cli-rust-shell-lifecycle.test.ts` 的 "EPIPE waits for TERM-ignoring job-control descendants" 在全量运行的高负载下失败过一次：进程以 1 退出，报 `Protocol writer stopped`。单独运行 3 次均通过，全量重跑也通过。原因待查。

## 后续优先级

| 优先级   | 后续工作                                      | 完成标准                                                                                                                        |
| -------- | --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| P1       | 性能长尾与真实工作负载                        | 非仓库首段与大历史缓存/分页优化已完成；继续验证真实 TS/供应商、大活跃历史、流式稳定阶段 RPC 长尾，不能凭语言宣称更快            |
| P1       | MCP 鉴权与真实服务器兼容                      | OAuth、断线/凭据变更；按在用服务器逐一实测。插件配置插值、官方鉴权、大结果预算已完成（M10.1、M10.5、M5.2c）                     |
| P1       | 历史与文件恢复剩余边界                        | Node/Rust 共库互操作已验收；继续补齐复杂子代理历史、Shell/外部修改的产品边界和跨平台故障恢复                                    |
| P1       | 大历史与磁盘容量                              | 单会话按需投影、未引用附件/checkpoint GC 和产物保留策略；M11 已删除旧迁移备份流程，大库冷读取已有专项测量                       |
| P1       | 主代理上下文扩展                              | 目录级 rules、记忆索引/提取、output style、自定义 system，与已有 Skill/profile 区分                                             |
| P1       | 附件与工具细节                                | 大图缩放、媒体 Read、Edit 宽松匹配、WebFetch/WebSearch、请求媒体预算已完成；剩余本地文本 token 上限截断、音频及工具装配兼容差分 |
| P1       | App 生命周期完整矩阵                          | 已完成共库交替续聊和 macOS 基础回退；继续补齐带 Plan 历史、跨窗口/多 session、远端/手机实机及账号供应商多协议组合               |
| P2       | 自动任务、Cron/OffPeak、浏览器/CUA、Node REPL | 真实执行与持久化投影；动态工作流按当前规格排除，`workflowRuns` 继续明确返回 unsupported                                         |
| 发布门槛 | 多平台发行与回退                              | macOS ARM64、Windows x64 npm 包已发布；仍需 Windows 桌面端到端、Linux/macOS x64/Windows ARM64 原生验收、远端部署与完整升级回退  |

已确认的基础链路包括账号模型、三种模型协议、AskUserQuestion、Todo、shared context handover、本地文本附件、取消/队列/冷恢复。相关报告保留在 `docs/reports/`。本清单区分功能核心完成、残余兼容差分和发布门槛，不宣称全量 TS 替换已经验收。
