# Rust stdio 核心扩展交付与验收

2026-09-22，当前工作区 `/Users/mbear/bearspace/ZCode-Pro`，macOS arm64。完成用户指定的 MCP、Skill、子代理、Goal、session 按需加载、重试回复、编辑重跑、分支和文件回退的核心链路。默认仍为 TS；Rust 显式选择、仅 yolo、没有 TUI。以下区分实现、真实 App 验收和仍未通过的发布门槛。

## 实现

| 范围     | 交付内容                                                                                                                                    | 主要代码                                                                                      |
| -------- | ------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------- |
| 会话加载 | 启动 metadata index、按 key 查 ACK、单会话激活、空闲 LRU 8、运行/订阅/队列/子任务 pin                                                       | `app/session_residency.rs`、`adapters/storage_index.rs`                                       |
| MCP      | rmcp 真实 stdio、Streamable HTTP、legacy SSE、动态工具注册/调用、连接复用、取消和进程清理、损坏配置单服务器隔离                             | `adapters/mcp_*`、`app/mcp.rs`                                                                |
| Skill    | 用户/项目/启用插件发现与优先级；冻结会话 metadata，调用时读内容并提交 canonical result                                                      | `adapters/tool_skills.rs`、`app/skills.rs`                                                    |
| 子代理   | 真实 child Session、前后台、消息恢复/边界投递、等待/停止、profile/model/tool/maxTurns、继承 Skill/MCP、memory 文件；父 stop/EOF 清理树      | `app/subagents.rs`、`subagent_tools.rs`、`subagent_completion.rs`                             |
| Goal     | durable 目标/迭代/验证、隐藏且无工具 verifier、继续/暂停/恢复、队列/后台等待、预算和冷恢复                                                  | `app/goal_*`、`domain/goal.rs`                                                                |
| 历史操作 | canonical 输入/响应边界、附件快照与 Goal 意图；retry/edit 同会话切断、新 epoch/单调 rowId；fork 子会话与 ACK 同事务                         | `app/history_commands.rs`、`domain/history.rs`、`adapters/storage_history.rs`                 |
| 文件恢复 | Write/Edit 修改前持久化字节/hash/权限 checkpoint；内容去重；expected hash 冲突；恢复 journal 和提交失败补偿；App header 摘要与按需 diff RPC | `app/file_rewind.rs`、`app/file_changes.rs`、`adapters/file_rewind.rs`、`checkpoint_blobs.rs` |

上述路径继续由 Session actor 独占事实状态，IO 通过 ports。模型、工具、输入、Goal 与子任务的提交回执仍是执行屏障。历史边界单独增量存储，不把整个 history index 重写到每个流式 checkpoint；剪裁后的 command receipt 独立保留。

## 自动化验收

最终 `CARGO_INCREMENTAL=0 pnpm test:zcode-cli-rust`：**57 个 Rust 测试、185 个 App client/schema 集成用例全部通过**，无跳过。使用真实 Rust 子进程，包含已有模型/工具/存储/协议回归。

本工作包重点新增/扩展：

- MCP stdio/HTTP/SSE、现代/旧协商、分页工具、只读声明、取消清理、鉴权缺失和 invalid config。
- Skill 来源、冻结目录、调用结果、插件边界及冷恢复。
- 子代理隔离、前台兄弟并发但结果按调用顺序、profile 工具约束、后台消息/通知、冷恢复、MCP/Skill/memory 继承、存储失败屏障。
- Goal 未通过后继续并验证成功、无效 verdict、队列/后台等待、暂停恢复、取消、持久化失败。
- retry/edit 双连接 snapshot、附件源删除后重跑/显式清空、Goal 编辑、稳定 fork、不合法目标不取消父轮、fork ACK 故障不残留 child。
- 文件外部修改拒绝覆盖、BOM/CRLF/权限、重复撤销、缺失/损坏备份、进程恢复 journal；准备提交失败不写文件，历史提交失败恢复本次文件操作。
- 300 个冷会话中故意放置损坏 transcript，证明启动/index 不读全部历史；超过 8 个空闲历史会淘汰，订阅会话保持 epoch，坏历史只影响目标读取。

检查结果：Rust fmt、Clippy `--all-targets -- -D warnings`、Rust 源码边界检查、根 typecheck、lint、格式检查、`architecture:check --changed` 均通过。Lint 保留 **70 个既有 warning，0 errors**；架构 **0 violations / baseline 0**。

完整日志保存在 `.zcode-runtime/rust-critical-20260922/validation/`；首次故障测试及修复过程日志在 `/tmp/rust-*.log`，不把失败尝试计作最终通过。

## 真实 App 验收

通过隔离的 `ZCode Rust E2E` App 启动 Rust debug stdio，使用真实账号模型 GLM-5.3。生产 App 未关闭、用户配置未覆盖。清理的是隔离 App 可再生成缓存，约 54 MiB，历史数据库/备份未删除。

- Skill `rust-app-check` 返回 `RUST_SKILL_APP_OK`。
- 本地 MCP `mcp__rust-e2e__ping` 返回 `RUST_MCP_APP_OK`。
- 默认 general-purpose 按用户已存模型覆盖选择 GLM-5.2，当前账号不可用，真实返回失败。随后使用隔离项目内 `rust-app-worker` profile 继承可用模型，子代理独立调用 Write 创建 `critical-child.txt`，内容为 `RUST_CHILD_APP_OK`，父会话返回 `RUST_AGENT_APP_OK`。
- App 冷恢复暴露了“只有 user/assistant 行能力、缺少 header.fileChanges”的缺口；修复后出现文件摘要、diff 与 Undo，自动化加入严格 schema 回归。
- 从 App 执行普通编辑重跑时保留文件；再次创建 `critical-edit.txt`，点击“Reset chat + files”后文件删除、原工具轮切断，新回复 `RUST_REWIND_CONFIRMED`。
- 点击回复 Fork 创建独立 interactive session，在分支输入 `/goal`，实际 verifier 后持久状态为 `verified`，UI 显示 “Goal complete, ending task” 和 `RUST_GOAL_APP_OK`。
- 再次退出/启动 App，打开分支仍看到相同历史与 Goal 完成状态，文件回退事实保留，无自动重放。
- 当前 App 源码明确不渲染普通 retry 按钮（`ConversationRowView.tsx` 的协议兼容注释）；`retryTurn` 由真实 stdio client/schema 自动化验收，未宣称点击过不存在的按钮。

证据目录 `.zcode-runtime/rust-e2e/20260922/`：`critical-acceptance-evidence.json`、`critical-goal-fork.png`、`critical-cold.png`、两份 snapshot 文本及 `critical-*-app.log`。Renderer errors 为空；已有的 `v4/conversation/workflowRuns` unsupported 仍存在，工作流未在本包实现。

## 性能

后续性能工作已定位并修复非仓库 Git 首段回退，增加请求内存/缓存/分页优化；本节保留当时基线，当前结果见 [性能与内存报告](rust-performance-2026-09-22.md)。

使用固定 SSE、多轮历史和多会话负载，同机 release，旧 Rust 基线与本包交错各重复 5 次。基线为本工作包开始前保存的 Rust release，**不是 TS 二进制**。两边均使用临时 HOME/配置，避免真实 MCP 配置污染基准。最终静态负载未并行执行构建或测试；App 空闲。

| 场景（旧版 → 本包，中位数） | 启动 ms     | 首段 ms         | 稳态首段 ms | 总耗时 ms       | RPC p95 ms  | RSS 峰值 MiB  | SQLite/WAL MiB |
| --------------------------- | ----------- | --------------- | ----------- | --------------- | ----------- | ------------- | -------------- |
| 固定流式                    | 8.04 → 8.91 | 152.81 → 247.23 | 1.70 → 2.19 | 275.58 → 401.71 | 1.04 → 2.29 | 30.02 → 38.45 | 2.74 → 6.49    |
| 100 轮历史                  | 7.57 → 8.98 | 151.36 → 244.38 | 1.16 → 0.73 | 375.88 → 593.28 | 0.42 → 0.71 | 24.59 → 28.33 | 5.16 → 5.34    |
| 4 会话                      | 8.19 → 9.02 | 151.67 → 252.15 | 2.92 → 1.43 | 251.30 → 368.79 | 1.54 → 1.37 | 30.73 → 37.81 | 6.09 → 6.45    |

原始数据：`.zcode-runtime/rust-bench/critical-20260922/results-idle/summary.json`，包含每场景 5 次明细和机器/二进制信息。

启动指 storage ready；首段包含该进程第一次模型 client/上下文初始化；steady 是后续轮。RSS 是采样峰值，storageBytes 包含 SQLite/WAL 文件占用。单次几十至数百毫秒的本机 fixture 受系统调度影响，不代表供应商网络耗时。

扩展功能增加了首段、总耗时、RSS 与部分 WAL 开销；MCP HTTP client 已改成无服务器时不创建，但重复对比仍有回退。当前不把性能门槛标记完成。需要进一步定位 TLS/扩展发现/历史 action 更新成本，并增加真实 TS release、真实长历史与供应商矩阵。

## 明确边界

- MCP OAuth、完整插件 options/hook、legacy SSE 自动重连和全部真实服务器兼容尚未完成。
- 单会话全历史仍在该会话激活时读取；备份保留、未引用附件/checkpoint GC 尚未完成。
- 文件回退只覆盖受跟踪的 Write/Edit（包含记录到父会话的子代理文件修改）；不宣称撤销 Shell 或任意外部 MCP 的写入。
- 旧 TS/Rust 历史缺少 canonical 边界时拒绝 retry/edit/fork，不根据相同正文猜测。
- Goal 无效 verifier JSON 不标 verified，这是明确保留的 TS 行为差异；子代理完整 JSONL artifact、子树预算聚合仍待扩展。
- 当前验收不能代替 Windows/Linux、远端/手机、主代理记忆/style、工作流/自动任务和完整 TS 替换验收。

后续优先级已更新到 [剩余清单](../specs/rust-parity-remaining.md)，本包契约入口为 [核心工作包](../specs/rust-critical-parity.md)。
