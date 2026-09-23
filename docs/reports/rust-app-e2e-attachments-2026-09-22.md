# Rust 最新产物的真实 App 复验

2026-09-22 16:12–16:20，ZCode-Pro `main` / `872ad96` 的未提交工作区，macOS arm64、Electron 41.0.3、Rust debug。本次重新构建并两次正常启动真实 App，使用现有账号 GLM-5.3 / Max，权限为 yolo。没有模拟模型、直接注入 Renderer store 或绕过 Host 发消息。

本次二进制 SHA256：`b5e33b695f25eb448e178eee686df0c0ca87609a6401945a60e7002c85b184ac`。实际链路为 Electron → window Local Host → 开发 stdio tap → `zcode-cli-rust app-server --stdio --cwd <测试工作区> --surface desktop`。普通启动入口仍默认 TS。

## 用户路径与断言

| 场景         | 真实操作                                                                                              | 结果                                                                                                                                                    |
| ------------ | ----------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 启动与历史   | 退出旧测试 App，构建最新 Rust 后重启，点击原任务                                                      | 历史、工具卡片和模型配置恢复；实际进程为 Rust                                                                                                           |
| 本地文本附件 | Add context → Attachments → 原生文件选择器，选择合成 `composer-attachment.txt`，在 Composer 点击 Send | 附件显示在输入和历史中；模型取得提示词未提供的校验码 `COBALT-417-ORCHID`                                                                                |
| 请求与工具   | 要求根据附件依次 Write、Read、Bash                                                                    | 三个工具均 success；数据库顺序与实际文件内容一致；没有使用工具读取附件源文件                                                                            |
| 停止         | 前台 Bash 先写 STARTED、sleep 30、再写后续标记；确认 STARTED 后点击 Stop                              | 界面 Stopped，持久化工具状态 cancelled；超过原定等待时间后，后续文件仍不存在                                                                            |
| 排队         | 运行前台 sleep 15 时点击 Queue message；通过 Continue 恢复停止留下的暂停队列                          | 第一轮助手完成 seq 72，排队输入随后 admission 为 seq 83；第二轮 Read 成功，未重跑停止的命令                                                             |
| 新建任务     | 点击 New task，发送一次 Bash `printf NEW_TASK_RUST_OK`                                                | 新任务进入侧栏，yolo 自动执行，真实 stdout 正确，最终 NEW_TASK_E2E_OK                                                                                   |
| 附件冷恢复   | 将合成附件源文件改名移走，正常退出 App 并重启，打开原任务续聊                                         | 不调用工具即正确回答附件中此前未在回复里出现的 `expected_tool_output=ATTACHMENT_TOOL_VERIFIED`；快照内容与移走的源文件相同，Session 为 completedSuccess |

用户操作仅使用现有 Composer、按钮和原生选择器。验证另外读取测试目录文件和 SQLite，未修改数据库或通过内部 RPC 伪造界面状态。源文件移走后保留为 `composer-attachment.source-moved.txt`，可用于复核。测试工作区和 Electron profile 沿用上一轮隔离目录；账号登录本身未重测。

## 自动化复验

| 检查                                                                  | 本次结果                                              |
| --------------------------------------------------------------------- | ----------------------------------------------------- |
| `cargo build --locked --manifest-path apps/zcode-cli-rust/Cargo.toml` | 通过，真实 App 使用该产物                             |
| `pnpm test:zcode-cli-rust`                                            | 28 Rust / 84 App 集成测试通过，无失败或跳过           |
| `pnpm check:zcode-cli-rust`                                           | native 边界、fmt、Clippy 全 target `-D warnings` 通过 |
| `pnpm typecheck`                                                      | 通过                                                  |
| `pnpm lint`                                                           | 0 errors，70 条既有 warnings                          |
| `pnpm architecture:check --changed`                                   | violations / baseline / new 均为 0                    |
| Renderer 未捕获异常                                                   | 自动化连接记录为空；业务警告另列如下                  |

证据目录：`.zcode-runtime/rust-e2e/20260922/`。`latest-evidence.json` 保存二进制指纹、测试任务、文件与持久化断言；`screenshots/08` 至 `14` 对应附件草稿、工具完成、停止、排队、新任务和冷恢复；`checks/` 保存本次命令输出。开发日志含本机环境和账号上下文，仅留本地，不作为公开日志包。

## 尚未通过的范围

- `v4/conversation/workflowRuns`、`plugins/referenceCatalog` 仍返回 Unsupported method。当前对话场景可完成，相关产品能力未实现。
- 本次产物切换离开预热草稿时，清理命令返回 `guard.capabilityUnsupported`。后续会话关闭包已修复，并在最终产物完成真实 App 复验，见 [会话关闭报告](rust-session-close-2026-09-22.md)；这里保留首次发现记录。
- 启动预热另一已保存工作区时，旧 TS 历史导入报告附件源文件缺失并保留源库。不能用本次合成工作区成功证明任意旧数据都能迁移。
- 本次界面新增验收是本地文本附件；图片/PDF/视频、Web 分片上传、手机/远程恢复仍需真实入口验证。文本附件点击后的独立预览没有获得可见结果，不记为通过。
- 这是 macOS debug 与一个真实供应商的功能复验，不是 release 性能对照、跨平台验证或全量替换验收。

结论：最新 Rust CLI 的 App stdio 主对话、新任务、yolo 工具、停止、队列、历史恢复和本地文本附件链路可用。完整替换仍按 [剩余对齐清单](../specs/rust-parity-remaining.md) 推进。
