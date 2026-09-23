# Rust App 历史身份查询接入验收

2026-09-22，macOS arm64，当前工作区 debug Rust 产物。修复真实 App Host 索引补齐调用 `session/list` 时的 Unsupported method；默认 runtime 仍是 TS，Rust 显式选择，权限仍只支持 yolo。

## 结果

- `session/list` 接入现有 App schema：严格参数及 trim、默认 50/显式 limit、按更新时间和 ID 降序、归档与任务类型过滤、跨工作区身份隔离。
- 显式 ID 批量查询保留顺序和重复项，可读取隐藏子会话，忽略 limit，不激活会话。现有 `repairSubagentTaskIndex` 对真实 Rust 子进程执行，只有同身份、明确为 subagent_child 的派生索引被标记；缺失记录、普通任务、其他身份和 stale owner 都保留。
- SQLite 只选身份元数据，不读取 rows/messages，不修改水位或写入历史。旧路径/trace 从已提交 TS 备份只读补齐，单次查询复用备份连接；已知 null trace 不反复访问备份。directory 与真实 path 分开保存。SQL 查询计划验证 workspace 排序、全局排序和 ID 查询均使用索引，无临时排序树。
- 900 KiB 回复预算、损坏正文隔离、备份不可读、数据库缺表等故障均有测试。失败不会变成空列表成功。
- 通过真实 TS store 和 `mapSessionInfo` 差分发现并修正两处细节：无 goal 时省略 target；持久化/旧 SessionInfo 保留 first_input，V4 meta/index 则按 TS 映射为 generated。后一处在首轮全量回归造成 5 个迁移测试失败，修正后全部通过。

所有权保持 `Host -> Engine -> SessionStore -> SQLite worker`，没有新增任务索引或第二份可变会话状态。新增 4 个 Rust 源文件共 368 行，其中 34 行是既有 storage load 逻辑迁出；各源文件不超过 400 行。规格见 [rust-session-list.md](../specs/rust-session-list.md)。

## 真实 App

在原隔离 E2E profile 上关闭旧 App，清理约 520 MiB 的 Chromium Cache/Code Cache/GPU 缓存后重启。保留会话、TS 导入备份、附件和已有证据，没有改动正式 ZCode 进程。

- Electron main PID 3273，Host PID 3938，测试会话 Rust PID 4007。
- 启动恢复中的真实 Host `session/list` 请求由 Rust PID 4004 处理，23 ms 返回合法 `{sessions: []}`。该次查询的旧索引 ID 在当前 Rust 历史中不存在，因此保留派生索引；不能把这次空结果说成找到了子会话。包含真实子会话的删除条件由上面的真实子进程 + Host 函数测试覆盖。
- 新日志中 `Unsupported method: session/list` 和「子代理历史列表索引修复失败」均为 0。
- 在 UI 点击已有任务冷恢复，再通过 Composer 发送限定回复请求，GLM-5.3 返回 `RUST_SESSION_LIST_RESUME_OK`；数据库状态为 completedSuccess，UI 恢复 Send，Full access 状态正确。
- Renderer errors 为空。已有 `v4/conversation/workflowRuns` 未实现警告仍存在，单列到剩余清单。

二进制 SHA-256：`f5200b7b619f3d6d29b09adbe4377ac455b1b4b79deb59683e585a6e61e64fc7`。

本地证据位于 `.zcode-runtime/rust-e2e/20260922/`：`session-list-evidence.json`、`session-list-app.log`、`session-list-renderer-errors.txt`、`screenshots/42-session-list-resume.png`、`checks/session-list/`。真实 stdio 请求/响应关联路径记在 evidence 中；日志和凭据不纳入提交。

## 验证

| 检查                                        | 结果                                                                                             |
| ------------------------------------------- | ------------------------------------------------------------------------------------------------ |
| `CARGO_INCREMENTAL=0 pnpm test:rust-agent`  | Rust 50 / App 139 全通过，0 跳过；包括测试 TypeScript 编译、生成的 prompt/tool schema 一致性检查 |
| `CARGO_INCREMENTAL=0 pnpm check:rust-agent` | boundary / fmt / Clippy `-D warnings` 通过                                                       |
| `pnpm typecheck`                            | 通过                                                                                             |
| `pnpm lint`                                 | 0 errors，70 个既有 warnings                                                                     |
| `pnpm fmt:check`                            | 通过                                                                                             |
| `pnpm architecture:check --changed`         | 0 violations / 0 new                                                                             |

新增 1 个 Rust 索引检查、5 个 App 子进程测试。构建始终关闭 incremental，完成时 incremental 目录为 0 B，磁盘约 11.8 GiB 可用。23 ms 只是本次空 ID 查询的单次观测，不代表吞吐量或 TS/Rust release 性能对比。

## 仍未覆盖

普通列表暂不追加超过 limit 的 live runtime。TS 与 Rust 的启动加载语义不同，不能将 Rust 启动时加载的全部历史当成 TS live runtime；此项与按需加载/运行时生命周期继续对齐。无 workspace 查询保留 remote identity，且 identity 在 limit 前过滤，遵守工作区隔离约束。

本包没有完成工作流、fork/edit/retry、shared context refs、完整扩展工具、手机/远端/三平台和 release 性能矩阵。全范围差距仍见 [剩余对齐清单](../specs/rust-parity-remaining.md)，不能据此宣称全量替换。
