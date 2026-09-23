# Rust App 执行模式对齐

2026-09-22，ZCode-Pro 当前工作树，macOS arm64。解决 App 在 Rust yolo-only runtime 下仍显示可选 build/edit/Plan，并按默认 build 预热失败的问题。默认 TS runtime 不变；Rust 仍显式选择，权限仍仅支持 yolo。

## 行为和边界

- Runtime 提供 workspace 级 `executionCapabilities`，权限和独立 Plan 分开声明。Host hello 不承担执行能力事实。
- 保留草稿/历史/Recent 的原权限。build 或 Plan 不会自动转成完全访问；Composer 提示如何显式选择，可编辑正文但禁止不支持配置的预热和发送。
- 模式菜单禁用不受支持的项，快捷键只循环支持的权限；已有 Plan 可关闭。未知或读取失败时等待能力，不先启动错误预热。
- 统一 workspace presentation hook 复用原 RPC，以 service、workspace identity、generation 隔离 SWR 读取；移除 Composer 的另一条独立水合请求。模型目录及 slash 显示仍使用既有 store。能力不写入权限偏好或会话配置。
- 新增字段经过能力协商：`runtime/capabilities.workspaceExecutionCapabilities` 为 true 时 Host 才请求 `includeExecutionCapabilities`。Rust 未收到该请求时保持旧 presentation 形状，兼容旧 App strict schema。协议版本不变。

规格与所有者时序见 [spec](../specs/rust-app-execution-modes.md)。主要修改位于 shared 执行能力 schema、Host presentation 读取、UI workspace hook / Composer 和 Rust workspace 投影；未增加业务状态所有者。

## 自动化

新增两项 Rust 子进程 / App schema 测试，扩展现有 Host + Composer 提交测试：

- build / yolo / Plan 组合门禁，保留输入意图，旧 TS 缺字段兼容，空权限列表和非法 Plan 权限拒绝。
- presentation 与两种 delivery 的 workspace-config 返回一致能力；旧 strict schema 在未协商时仍通过；runtime 拒绝 build 首发且不调用模型。
- 实际 Host 协商并返回 Rust 能力；真实 Composer 冻结提交拒绝 build 与 yolo+Plan，接受 yolo。

修复前测试记录了字段缺失；兼容复核另复现了旧 strict schema 因新增字段失败，协商后通过。

| 门禁                                        | 本轮结果                       |
| ------------------------------------------- | ------------------------------ |
| `CARGO_INCREMENTAL=0 pnpm test:rust-agent`  | 36 Rust / 103 App 通过，0 跳过 |
| `CARGO_INCREMENTAL=0 pnpm check:rust-agent` | Rust 边界、fmt、Clippy 通过    |
| `pnpm typecheck`                            | 通过                           |
| `pnpm lint`                                 | 0 错误、原有 70 条警告         |
| `pnpm fmt:check`                            | 通过                           |
| `pnpm architecture:check --changed`         | 0 新增、0 基线违反             |

日志：`.zcode-runtime/rust-e2e/20260922/checks/execution-modes/`。

## 真实 Desktop

使用隔离 `ZCode Rust E2E`、真实 GLM-5.3 Max 和 stdio Rust 子进程。通过原生 Open folder 新增 `mode-workspace` 测试项目，未手改 App 设置或草稿数据。

1. 全新项目默认仍为 Ask before changes；填入正文后 Send 禁用，显示 yolo-only 提示。菜单当前 build 仍选中，build/edit/Plan 禁用，Full access 可选。
2. 选择前 stdio 实测 `createSession=0`、`sendText=0`、presentation 读取 1 次。选择 Full access 后才预热，Send 可用；首发实际返回 `RUST_MODE_GATE_OK`。
3. Shift+Tab 后仍为唯一可用的 Full access；430×850 viewport 下菜单可操作。该检查是 Desktop 窄窗口，不代表手机远控已经验收。
4. SIGTERM 终止该工作区 runtime，App 自动启动新进程并恢复原会话；随后最终协商版本完整重启 App，实测请求带 `includeExecutionCapabilities:true`，响应为 yolo-only，原会话恢复可继续使用。

最终二进制 SHA256：`3b334ddc93ae734ed38692c545fab1af3c0ba990ee40fb49ae3e293dcdb0bb83`。最终协议协商证据见 `execution-modes-negotiation.json`，选择前计数见 `execution-modes-before-selection.json`；两者均在上述隔离目录。

截图：`screenshots/19-mode-build-blocked.png`、`20-mode-availability-menu.png`、`21-mode-narrow-menu.png`。最终历史续聊和数据库结果见 `execution-modes-evidence.json`。

## 尚未验收

跨 TS/Rust 热替换时的迟到请求交错、多 Pane 不同挂载时刻的请求合并、带 Plan 的真实旧历史导入、手机远控的权限选择矩阵仍需扩大测试。当前是基本 App 操作闭环完成，不宣称全量替换。其余功能缺口继续列于 [剩余清单](../specs/rust-parity-remaining.md)。构建保持关闭增量缓存，不复制生产大库；已提交备份保留。
