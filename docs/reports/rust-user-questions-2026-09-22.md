# Rust 用户问答与 App 验收

2026-09-22，ZCode-Pro 当前工作树，macOS arm64。Rust 已通过现有 App 的 AskUserQuestion 交互继续 Agent loop；权限仍仅支持 yolo，TS 保持默认，Rust 显式选择。

## 实现与所有权

工具 schema 与描述从当前 TS 生成，并通过漂移检查。Rust 校验问题/选项数量、唯一性、预览安全约束和可选字段，保留 TS 对缺省值与 null 的区分。单选、多选、自定义回答、部分回答、显式跳过、拒绝、取消和旧回答格式统一为 TS 模型结果格式；模型伪造的 answers 不会代替用户实际回答。

Session actor 是问题、定时器和工具行的唯一所有者。问题提交成功后才发布；回答的工具行、pending 移除和 ACK 提交成功后才唤醒工具。随后工具结果仍经已有 canonical 提交屏障进入下一模型请求；同批多个问题可独立回答，历史保持调用顺序。数据库失败立即停止执行。

头部问题默认隐藏倒计时 60 秒，累计 300 秒后自动以空回答继续。App snooze 永久停用该问题倒计时；关闭工作区自动继续设置会停用现有问题，再开启仅影响后续问题。actor 按最近截止时间等待，不为每个问题创建常驻轮询。测试加速时钟只在 `ZCODE_ENV=test` 生效。

停止、立即发送、关闭、EOF 与旧 run 事件沿用既有边界。冷恢复不会重新提问或自动重跑工具；回答已提交、canonical 结果尚未提交的窗口，从已持久化工具行恢复实际回答。接口和时序见 [spec](../specs/rust-user-questions.md)，未新增协议版本或 UI 状态所有者。

## 自动化验证

新增 12 项真实 Rust 子进程 / App client/schema 测试，覆盖回答格式与 TS formatter 差分、严格输入校验、伪造答案隔离、多问题顺序、重复/跨会话/关闭后迟到回答、双连接订阅、stop/startNow/EOF/关闭、自动继续、snooze 与设置切换。预览和 notes 的透传/格式化也有自动化覆盖。

新增 1 项受控存储测试包含六个场景：问题登记、回答、snooze、自动回答和 canonical 工具结果各自的提交失败，以及实际回答已提交但 canonical 尚未提交时的恢复。验证未越过失败提交发起下一模型请求，也未误执行 ToolPort。

| 验证                                            | 结果                               |
| ----------------------------------------------- | ---------------------------------- |
| `CARGO_INCREMENTAL=0 pnpm test:zcode-cli-rust`  | 47 Rust / 127 App，通过，0 跳过    |
| `CARGO_INCREMENTAL=0 pnpm check:zcode-cli-rust` | 边界、fmt、Clippy -D warnings 通过 |
| `pnpm typecheck`                                | 通过                               |
| `pnpm lint`                                     | 0 错误，既有 70 条警告             |
| `pnpm fmt:check`                                | 通过                               |
| `pnpm architecture:check --changed`             | 0 违反、0 新增                     |

最终日志位于 `.zcode-runtime/rust-e2e/20260922/checks/user-questions/`。实现前问答用例因收不到 interaction 超时；开发中的 fixture 缺少 workspaceKey、工具 registry 期望未包含新工具、Clippy 和 JSON 格式问题均已修正，最终全量重跑通过。生成器现使用仓库 oxfmt API，生成后立即得到规范格式；Node SQLite 实验性提示保留。

## 实际 App 验收

隔离的 `ZCode Rust E2E` 使用原问答界面、真实 GLM-5.3 Max 和 `mode-workspace`，会话为 `2266a711-b002-4841-9136-e4acb2678e80`。

- 一次调用包含单选和多选，选择“列表”和“搜索、导出”，模型正确复述后返回 `RUST_QUESTION_APP_OK`。
- 点击 Dismiss，App 提交 decline，工具保存 error，模型返回 `RUST_QUESTION_DISMISS_OK`；未伪造用户偏好。
- 等待未回答的问题时向 Rust 发 SIGTERM，经 Host 重启，旧工具显示 cancelled，pending 清空；续聊返回 `RUST_QUESTION_RESUME_OK`，没有重新执行工具。
- 最终产物重启后选择 Other 并输入 `RUST_CUSTOM_ANSWER`，提交后模型原样复述并返回 `RUST_QUESTION_FINAL_OK`。

四条问答工具记录依次为 success、error、cancelled、success，7 条 resolve/snooze 命令均 accepted。最终会话为 completedSuccess，pending 为空；stdio 可见 hiddenGrace、visibleCountdown 和 snoozed 投影。Renderer errors 文件为空，最终截图已查看。

前三个场景的初始运行 PID 为 45887，二进制 SHA256 为 `a777a27b80f0452dbc8ca467246471e316ac4d0ff572493d6b83d0cb794a8e78`，冷恢复 PID 为 52645。随后仅规范生成资产格式并重跑全部检查，最终运行 PID 为 59547、SHA256 为 `fab205c4dd50faf3b69190ce1ab5e0bc2514897ffcec7a16ddc958ac7f269f24`；最后的自定义回答场景使用此最终产物。

证据为上述目录的 `user-questions-evidence.json`、`question-restart-before.json`、`question-cold-evidence.json`、`user-questions-renderer-errors.txt` 和截图 `31-question-pending.png` 至 `36-question-final-custom-answer.png`。未把原始含账号配置的 stdio 记录写入报告。

## 容量和剩余边界

延续用户要求，Cargo 增量缓存保持 0 B，记录时磁盘剩余约 11.71 GiB；未复制生产数据库。此前清理的缓存与未提交导入残留约 6.8 GiB；当前已提交迁移备份和验收证据保留。

真实 App 覆盖单选、多选、自定义回答、拒绝、暂停倒计时及 SIGTERM 后恢复。preview/notes 仅验证协议和 formatter，当前 App 不呈现对应编辑控件；自动超时及双订阅在原生集成测试覆盖，未进行真实手机操作。SIGTERM 是正常终止验证，回答提交后的崩溃窗口由受控存储 fixture 验证。本包不替代 TS/Rust release 性能或跨平台验收。

Todo/独立任务计划、其他会话动作、上下文和扩展工具仍按 [剩余对齐清单](../specs/rust-parity-remaining.md) 推进；Plan 权限模式不在迁移范围，全量替换尚未完成。
