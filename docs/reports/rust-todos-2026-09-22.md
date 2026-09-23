# Rust Todo 与 App 工作计划验收

2026-09-22，ZCode-Pro 当前工作树，macOS arm64。已实现主会话 TodoRead/TodoWrite、持久化、提醒和 App 计划摘要，保持 yolo-only、TS 默认、Rust 显式选择。规则与所有权见 [spec](../specs/rust-todos.md)。

## 行为与数据

工具输入 schema、描述来自当前 TS 生成资产，测试直接调用 TS handler 比较读写结果。完整替换返回旧值、新值和统计；无效输入不改变状态；多个 in_progress 和 item 未声明字段处理遵循当前 TS 校验。TodoRead 可并发，TodoWrite 保持顺序屏障。

Session 是清单的唯一所有者。更新清单和完成工具行同事务保存，提交成功后才发布计划摘要并唤醒 loop；canonical 结果仍按调用顺序经过下一道提交屏障。工具行已提交但 canonical 尚未提交时，冷恢复使用实际结果，不重跑更新。停止、关闭和压缩不清空清单。

App V4 plan、旧 session/read.todos 都由同一清单派生。模型结果按 100000 UTF-8 字节预算截断，完整清单继续存储并用于 App 投影；空清单明确清除摘要。相比 TS 当前非空列表抽取函数，Rust 清空后不会继续显示旧摘要；纯空白内容仍按 TS 接受，但不会投影空标题项。

提醒沿用 TS 十个 assistant 消息阈值与文本模板，作为隐藏 synthetic user 事实先提交再请求；输出上限续写时不追加。原生冷恢复保留提醒来源标记，压缩后的计数基于有效历史。

TS 新导入从一致性备份读取 todo 表。旧版本已导入却缺少 todos 字段时，仅从已提交备份补齐；变化后的生产库不会覆盖原快照，不增加数据库副本，Rust 显式清空也不会被反向填回。非法存量内容导致整个导入回滚。

## 自动化结果

新增 7 项真实 Rust 子进程 / App client/schema 测试，覆盖 TS handler 差分、顺序屏障、双订阅、严格输入、空列表、会话隔离、提醒跨重启、中文大结果预算、压缩/关闭/恢复，以及 TS 首次导入与已提交备份补齐。新增 1 项 Rust 受控存储测试覆盖状态提交、canonical 提交失败和两者之间的崩溃恢复；已有 stale run fixture 加入 Todo 事件，验证其不能修改当前状态。另新增 1 项提醒阈值单测。

| 检查                                        | 结果                                   |
| ------------------------------------------- | -------------------------------------- |
| `CARGO_INCREMENTAL=0 pnpm test:rust-agent`  | 49 Rust / 134 App，通过；0 跳过        |
| `CARGO_INCREMENTAL=0 pnpm check:rust-agent` | 原生边界、fmt、Clippy -D warnings 通过 |
| `pnpm typecheck`                            | 通过                                   |
| `pnpm lint`                                 | 0 错误，既有 70 条警告                 |
| `pnpm fmt:check`                            | 通过                                   |
| `pnpm architecture:check --changed`         | 0 违反，0 新增                         |

日志位于 `.zcode-runtime/rust-e2e/20260922/checks/todos/`。最终 App 集成测试耗时 23231.8ms，仅作本次运行记录，不作为 release 性能对比。

过程中修正了三个测试问题，未放宽产品断言：HTTP fixture 分块 Buffer 隐式转字符串损坏跨块中文，改为流式 UTF-8 解码；Coding fixture 将新的隐藏 Todo 提醒错当测试指令，改为跳过已知系统通知；直接静态导入整个 TS runtime reminder 模块会把无关 CLI 浏览器/子代理源码纳入测试专用 tsconfig，现以运行期路径导入真实 formatter，保持差分执行和测试编译边界。上述修正后全量重跑通过，Node SQLite 实验性提示保留。

新增 Rust 源文件 3 个，共 313 行，均在 zcode-rust 的既有 domain/app/adapters 层内；没有增加状态 owner、跨模块运行时依赖或协议版本。原生目录此前已是未跟踪工作，因此该数字不是相对 HEAD 的整包净增统计。

## 真实 App 验收

复用隔离 `ZCode Rust E2E`、GLM-5.3 Max、`mode-workspace` 和会话 `2266a711-b002-4841-9136-e4acb2678e80`，通过原 Composer 提交，未直接调用 UI 内部 store。

1. TodoWrite 创建“检查清单 / in_progress / high”和“验证恢复 / pending / medium”，模型回复 `RUST_TODO_CREATED`，顶部显示“检查清单”。
2. TodoWrite 更新为 completed 和 in_progress，回复 `RUST_TODO_UPDATED`，顶部切换到“验证恢复”。
3. 对本测试工作区 Rust PID 70399 发 SIGTERM，经 Host 重启为 84245；顶部仍显示“验证恢复”。TodoRead 读回两项实际状态，模型准确复述后回复 `RUST_TODO_RESUME_OK`。
4. TodoWrite 写入空数组，回复 `RUST_TODO_CLEARED`，存储 todos=[]，顶部摘要消失。

四次工具调用依次为 TodoWrite、TodoWrite、TodoRead、TodoWrite，均 success，最终 completedSuccess。全程使用同一二进制 SHA256 `a11a376590c4d8a902e655e937cf1f91f6e69058bb57deaa199132eb2407dd49`。Renderer errors 文件为 0 B；创建和清空截图已查看。

证据位于上述目录的 `todo-evidence.json`、`todo-renderer-errors.txt`，截图为 `37-todo-created.png`、`38-todo-updated.png`、`39-todo-cold-plan.png`、`40-todo-resumed.png`、`41-todo-cleared.png`。SIGTERM 验证正常进程重启，崩溃窗口由受控 Store 验证。

## 剩余边界

启动 Host 日志另确认 `session/list` 未实现，历史索引补齐会收到 Unsupported method；这是独立于 Todo 的 App 接入缺口，已升为剩余清单的优先项。workflowRuns 仍未实现，不能将 Renderer errors=0 解释为所有 Host 功能通过。

Todo 不启用 `independentPlanState` 或 Plan 执行模式。旧 TS 提醒来源元信息的完整导入、Goal iteration 与子代理 Todo 镜像继续归入对应连续性/扩展包。真实手机、三平台和 TS/Rust release 性能仍未完成；当前只证明本包列出的主会话路径。

Cargo 增量缓存维持 0 B；验收时可用磁盘约 11.46 GiB。没有重新复制生产数据库，保留已提交备份和实机证据。后续按 [剩余对齐清单](../specs/rust-parity-remaining.md) 继续，不宣称全量替换。
