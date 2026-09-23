# Rust 本地会话与子代理详情修复

2026-09-23，macOS arm64。用户运行 `pnpm dev:desktop:rust` 后，本地会话的归档入口呈现远程语义，Agent 工具无法点击查看子代理详情。真实启动日志与 stdio 记录证明连接为本地 `desktop-continuous`，进程为 Rust `app-server --stdio`；传输没有切换成远控。

## 原因与修复

- Rust `session/read` 将本地 workspacePath fallback 同时填写为 workspaceIdentity。Host 随 snapshot 写入 tasks-index，UI 根据 identity 是否存在选择远程图标和行为。现已省略本地身份、保留真实远端身份；旧索引中 key/path/identity 三者相等的行，在 metadata 与 grouped structure 读取时统一纠正。保留主键、归档、置顶、标题和排序。
- Rust 确实执行并保存了子会话，但父会话只有 Agent toolCall 与 subagents 状态，缺少 App 用于关联详情按钮的 `subagent` 行。现在 actor 在启动、完成、取消、恢复消息时持久化同一行，带原始 parentToolCallId 和真实 childSessionId。父回复结束不会提前终止后台子代理的展示状态。
- 旧 Rust 会话按需冷加载时，从持久化 child 与真实 Agent/Task 锚点补齐缺失行；没有锚点不猜测。恢复建立一次行索引，复杂度为 O(rows + children)，不重跑模型、不改 canonical 消息，也不全量加载其他会话。

规则和事件顺序见 [spec](../specs/rust-app-session-projection.md)。

## 验证

真实 Rust 子进程与 App client/schema 覆盖本地/远端身份的 live/cold 读取、旧索引同路径隔离、壳状态保留、并发 child 配对、取消、后台完成和 SendMessage 继续执行。旧存储 fixture 删除展示行并保持历史边界后，冷恢复补齐两条关联；再次重启身份稳定，canonical 字节与模型请求次数不变。

隔离 Electron 开发实例使用 loopback 模型 fixture，通过真实 Composer 提交 Agent 任务，点击展开工作记录及子代理摘要：

| 场景             | 结果                                                                |
| ---------------- | ------------------------------------------------------------------- |
| 新任务点击子代理 | 右侧打开真实 child session，显示独立输入和 `CHILD_DETAIL_CONFIRMED` |
| 冷历史点击子代理 | 在另一个 Rust 进程生成并结束的历史中，入口与正文正常恢复            |
| 本地归档图标     | DOM 为 `lucide-archive`，按钮为 Archive task                        |
| 归档与取消归档   | 确认后进入 Archived 列表，Unarchive 后返回原项目                    |
| tasks-index      | 测试任务 workspace_identity 为 NULL                                 |
| Renderer 异常    | 连接自动化工具后的 errors 输出为空                                  |

本地截图在 `.zcode-runtime/rust-projection-20260923/`：`child-detail.png`、`cold-child-detail.png`、`local-archive.png`、`archived.png`。测试不是手机远控或真实供应商的重新验收。

自动化：61 个 Rust 测试、190 个 App 集成测试通过；Rust fmt、Clippy 全 targets `-D warnings`、根 typecheck、fmt 和架构检查通过。Lint 为 0 errors、70 条既有 warnings。release 二进制重新构建，日常入口仍为 `pnpm dev:desktop:rust`。

## 测试隔离记录

首轮测试脚本仅隔离 data/profile，没有隔离设置服务的 home，误写了真实配置的最近工作区及界面偏好。已停止该实例，使用测试前最近日志快照恢复并逐字段验证完整配置一致（包含历史字段）；会话数据库未改写。后续脚本显式设置 `ZCODE_DESKTOP_HOME_DIR`，退出后再次核对真实配置不变。测试进程、临时数据库和模型服务均已清理，保留本地截图和结果。
