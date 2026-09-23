# Node.js 与 Rust CLI 资源和功能对照

测试日期：2026-09-23。测试机为 Apple M2 Pro、macOS arm64、Node.js 24.14.0、Rust release 二进制。Node 版本由 [`apps/zcode-cli/packages/cli/src/main.ts`](../../apps/zcode-cli/packages/cli/src/main.ts) 构建，Rust 版本由 [`apps/zcode-cli-rust/src/main.rs`](../../apps/zcode-cli-rust/src/main.rs) 构建。

## 资源对比

基准使用同一个本地 OpenAI Chat Completions SSE fixture，8 个回合，每回合 256 个流式 chunk，单 session，context window 256000。两端都通过 stdio App Server 创建 session、订阅 conversation、发送文本并等待 `completedSuccess`；Node 通过 Provider Registry 选中 fixture，Rust 通过等价的显式模型 JSON 选中 fixture。两端均使用临时 HOME、工作区和 SQLite，未读取用户历史或真实模型。

脚本按 Node/Rust 交错顺序各运行 5 次，记录 `ps` 的 RSS 和累计 CPU time，汇总取中位数。RSS 是采样峰值，不是分配瞬间的硬峰值；平均 CPU 是 `CPU time / workload wall time`，超过 100% 时表示使用了多个核心。原始样本保存在本机 `.zcode-runtime/node-rust-bench-5/`，可用以下命令复现：

```sh
pnpm --filter @zcode/cli build
CARGO_INCREMENTAL=0 cargo build --locked --release --manifest-path apps/zcode-cli-rust/Cargo.toml
node scripts/bench-zcode-cli-node-rust.mjs \
  apps/zcode-cli/packages/cli/dist/zcode.cjs \
  apps/zcode-cli-rust/target/release/zcode-cli-rust \
  .zcode-runtime/node-rust-bench 5
```

| 指标（中位数）                |   Node.js |     Rust |        Rust 相对 Node |
| ----------------------------- | --------: | -------: | --------------------: |
| 启动到 `runtime/capabilities` |  573.2 ms |   8.2 ms |                -98.6% |
| 空闲 RSS                      | 408.8 MiB | 11.2 MiB |                -97.3% |
| 负载采样峰值 RSS              | 522.2 MiB | 23.4 MiB |                -95.5% |
| 8 回合墙钟时间                |   1.289 s |  0.147 s |                -88.6% |
| 8 回合累计 CPU time           |    0.78 s |   0.10 s |                -87.2% |
| 负载平均 CPU                  |     61.0% |    68.9% | Rust 更集中地使用 CPU |

这组结果说明，在当前本地 fixture 和当前构建形态下，Rust 子进程的驻留内存、累计 CPU time 和墙钟时间明显更低。平均 CPU 百分比不适合单独判断“谁更省 CPU”：Rust 完成得更快，所以相同工作被压缩在更短的时间内。该结果不覆盖 Electron/Host 总进程、真实供应商网络、超大历史、MCP 服务器、Windows/Linux 或移动远控；切换默认 runtime 前仍需按发布矩阵重复测量。

Rust 的启动中位数受本机文件缓存影响：5 次为 21.4、8.1、8.2、8.4、7.7 ms；Node 为 602.8、572.4、574.0、573.2、566.5 ms。后续应补充磁盘冷缓存和完整桌面启动测量。

## 功能对照和 TODO

勾选表示 Rust 当前已有实现，并有自动化或真实 App 验收证据；部分完成项保留在 TODO 中，不能按勾选项推断已经全量替换 TypeScript CLI。

### 已完成

- [x] App Server stdio/V4 协议、Session actor、SQLite 持久化、stop、队列、冷恢复和 workspace identity 隔离。
- [x] Session 按需加载、metadata index、空闲 LRU 和订阅/运行状态 pin。
- [x] OpenAI Chat Completions、OpenAI Responses、Anthropic Messages；流式文本、reasoning、tool call、usage、重试、取消和输出上限续写。
- [x] Provider Registry、默认模型、模型/思考档位选择、账号 overlay、每请求 Host 鉴权和连通性/文本生成接口的 Rust 接入。
- [x] yolo 执行模式的能力协商、权限门禁和普通 Coding 自动执行。
- [x] Read、Write、Edit、Glob、Grep、Bash，以及后台任务、TaskOutput、TaskStop、输出文件和 App 文件 diff 投影。
- [x] 手动/自动/反应式 compact、microcompact、上下文预算、队列保留/清空发送和 `sendQueuedNow`。
- [x] AskUserQuestion、TodoRead/TodoWrite、历史读取、重命名、retry/edit、fork、Goal 和文件回退的核心链路。
- [x] MCP stdio、Streamable HTTP、legacy SSE 的发现、调用、取消和进程回收核心链路。
- [x] Skill 发现与会话 metadata 冻结；子代理 Agent/Task、SendMessage、TaskOutput/TaskStop 的核心隔离和恢复链路。
- [x] 文本、图片、PDF 的基础附件上传、快照、预览和模型投影。
- [x] 性能基准脚本和本次 Node/Rust 同条件资源对照。

### TODO

- [ ] **权限模式**：Rust 当前只支持 yolo；补齐 build/edit/Plan 等模式、模式切换和通用审批规则，或继续保持明确的能力拒绝并完成产品入口隐藏。
- [ ] **MCP 完整兼容**：OAuth、完整插件 options/template/hooks、legacy SSE 自动重连，以及在用真实服务器的兼容矩阵。
- [ ] **上下文和主代理记忆**：目录级 rules、完整 system prompt/记忆、output style、自定义 system、插件 hooks 与主代理记忆索引/提取。
- [ ] **附件和工具细节**：大图缩放、媒体 Read、完整文本预览、音频和部分视频、Edit 的宽松匹配、Web 工具和工具装配开关。
- [ ] **会话和文件边界**：单个超大会话分页/按需投影、备份跨 workspace 去重和保留策略、未引用附件/checkpoint GC；Shell 或外部 MCP 任意写入的回退范围。
- [ ] **扩展执行能力**：工作流、自动任务、OffPeak、浏览器/CUA、Cron，以及完整 JSONL 子代理 transcript 和子树预算聚合。当前 `workflowRuns` 仍会返回 unsupported。
- [ ] **历史迁移和生命周期矩阵**：带 Plan 的旧历史导入、TS/Rust 热切换、远端/手机 `web-remote-replayable` 恢复、账号多协议回归和跨窗口/多 session 组合。
- [ ] **发行和平台**：Windows/Linux 原生进程、远端部署、安装升级/回退、真实供应商矩阵和发布产物验证。
- [ ] **性能发布门槛**：补充真实 TS release、真实长历史/大活跃会话、MCP/工具负载、磁盘容量和三平台测量；确认 RSS 上界、CPU 长尾和取消/恢复成本后再切默认 runtime。

功能依据：[Rust 剩余对齐清单](../specs/rust-parity-remaining.md)、[核心扩展验收报告](rust-critical-parity-2026-09-22.md)、[Rust 性能报告](rust-performance-2026-09-22.md)。
