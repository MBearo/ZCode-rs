# Rust 默认请求上下文对齐

2026-09-22，`main` / `872ad96` 的未提交工作区，macOS arm64。本包补齐主会话默认 system prompt、AGENTS 来源和环境上下文；全量替换目标继续以 [剩余清单](../specs/rust-parity-remaining.md) 为准。

## 实现与证据

- 三段 system 的静态文案直接从当前 TS 构造器生成；`generate-rust-prompt.mjs --check` 已接入 `pnpm test:rust-agent`，漂移会失败。运行时只读编译进产物的模板，不启动 Node。
- ContextPort 负责有界异步 IO，Session actor 唯一持有 durable 环境/Git/日期快照。首次普通请求先提交快照，再发模型请求；每模型步骤重新读取 AGENTS 并使用已绑定模型的真实名称。当前 surface 来自进程参数，跨 desktop/terminal 冷恢复不沿用旧文案。
- AGENTS 按 TS 当前实际规则加载用户默认和最近工作区文件，Git 根截断查找，不拼接所有父子目录。每份最多读取 100 KiB；超限、跨 UTF-8 边界、缺失/目录、去重及嵌套 reminder 标签均处理。
- AGENTS/日期是 user reminder 请求投影，压缩和用户历史不增加假输入。三协议传递相同正文语义；Anthropic 保留三段 system 及 ephemeral cache hints。
- Git 状态/最近提交只在首次初始化探测，后续步骤、回合及冷恢复不反复扫描仓库。探测有输出/时间上界，取消收回探测进程，控制 RPC 不被阻塞。

详细行为及所有者时序图见 [spec](../specs/rust-request-context.md)。改动位于 `zcode-rust` 的 domain/app/adapters，未新增 App 协议或服务层实现依赖。TS source adapter 仅新增非空来源守卫：差分测试的严格索引类型检查暴露了原有隐含不变量，没有更改有效来源的装配规则。

## 测试

新增请求差分测试先在旧产物失败：只有一段短 system，缺少环境和 meta user context；实现后三协议 × desktop/terminal 的实际 HTTP 请求逐段与 TS ContextBuilder 比较通过。另覆盖真实 Git 仓库及 origin/HEAD、跨入口重启、AGENTS 热刷新/截断/Git 边界、快照事务拒绝、慢 Git 取消。现有模型切换用例补充 prompt 模型名称断言。

原生提交屏障测试增加 prompt 初始化事务的故障位置，继续证明输入、模型消息和工具结果未提交时不执行后续步骤。附件用例更新为按实际用户消息定位内容，并明确断言 Anthropic 合并后的 reminder；未减少媒体、不可变快照或重试复用断言。

| 检查                                | 结果                                           |
| ----------------------------------- | ---------------------------------------------- |
| `pnpm test:rust-agent`              | 34 Rust / 97 App 集成测试通过，0 失败、0 跳过  |
| `pnpm check:rust-agent`             | 边界、fmt、全 target Clippy `-D warnings` 通过 |
| `pnpm typecheck`                    | 通过                                           |
| `pnpm lint`                         | 0 errors / 70 条既有 warnings                  |
| `pnpm fmt:check`                    | 通过                                           |
| `pnpm architecture:check --changed` | violations / baseline / new 均为 0             |

本工作区 Rust、测试及文档主体已是未跟踪内容，Git 的总 diff 不能用作本包独立净行数。新增上下文来源/纯渲染模块通过每 Rust source 文件不超过 400 行的边界检查；TS adapter 的独立改动为新增 2 行守卫和说明。

## 真实 App 复验

17:18–17:20，使用现有隔离 profile、最新 Rust debug 产物和真实账号 GLM-5.3 / Max / Full access。在测试 workspace 的 AGENTS 写入合成校验码后，从 App Composer 新建任务发送“验证请求上下文：不要调用任何工具。请仅返回本次请求附带的工作区 AGENTS 规则中指定的校验码。”消息中没有提供校验码。

模型回复 `PROMPT_CONTEXT_CEDAR_58`，UI 显示完成，SQLite 确认 `completedSuccess`、0 toolCall、1 canonical user 消息及已提交的 PromptSnapshot；Renderer 未捕获异常。实际 native PID 55276，产物 SHA-256 `ec5761a07c2e3bb30ab80fdf01f4b433fd730dd6c209b9b16ac57eca8655d6e2`。没有通过内部 RPC 或修改 Renderer store 伪造输入。

证据位于 `.zcode-runtime/rust-e2e/20260922/prompt-evidence.json`、`prompt-renderer-errors.txt` 和 `screenshots/17-prompt-context.png`，检查日志在 `checks/request-context/`。本次启动显式跳过测试进程的 TS 库导入，保留现有 Rust 库；清理后六份保留备份数量未增加。普通生产导入路径未被这个测试覆盖。

## 缓存与磁盘

按用户要求清理了 1.9 GiB Rust debug 增量缓存、两个已退出测试的临时目录，以及五份无导入记录引用、无打开句柄的旧导入副本（共 4,999 MiB）。所有已提交导入所引用的备份、每来源最新副本、实时数据库、测试截图与检查结果保留。后续本包构建使用 `CARGO_INCREMENTAL=0`。可用空间由约 4.4 GiB 回升到约 11 GiB；本地明细在 `.zcode-runtime/rust-e2e/20260922/cache-cleanup.json`。

## 尚未完成

项目记忆的开关/路径/权限/索引与自动提取、output style、自定义 system、Skill/工作流身份尚待对齐；它们未被默认主会话 prompt 的通过结果覆盖。AGENTS 动态刷新保留 Rust 已交付行为，TS 当前默认 runtime 则使用其初始化 source snapshot，不把该增强误称为 TS 自动加载全部子目录。发布级性能对照、Windows/Linux 与手机/远端入口仍须验收。原默认 TS runtime 与 Rust yolo 限定不变。
