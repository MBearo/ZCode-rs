# Rust Coding 工具交付记录

2026-09-22，ZCode-Pro main / 872ad96。基于第一包未提交改动继续实现。默认 runtime 仍为 TS，Rust 显式选择；本包权限范围仅 yolo。

## 交付内容

- 新会话 yolo 自动执行，输入、queue、配置投影保持一致；显式其它模式拒绝。旧 native 会话 mode 缺省为 build，必须显式 switchCollaborationMode(yolo) 才能执行，不在恢复时自动改变执行权限。
- Read/Write/Edit/Glob/Grep/Bash/TaskOutput/TaskStop 使用当前 TS 工具输入 schema；生成脚本及 schema 差分测试防止漂移。List 保留旧 native 调用兼容，不再公开给模型。
- 文本行分页、UTF-8/BOM/CRLF、读取新鲜度缓存按 session 隔离；已有文件先读，写入前核对观察版本，创建父目录、同目录临时文件与原子替换；Edit 支持唯一精确匹配和 replace_all。文件变更投影为既有 file_diff。
- 原生 ignore/globset/regex 搜索：Glob 修改时间排序；Grep 正则、类型/glob、上下文、大小写、only-matching、多行及分页。保留最多四只读并发及写操作顺序。
- 前台/后台 Shell、非零退出/超时/取消的结构化结果；大输出落盘并给出路径；后台任务按 session 隔离，可跨正常前台结束与新回合查询/停止，支持 App cancelBackgroundWork。
- Session owner 唯一持有模式、后台登记/终态、canonical 消息和投影；进程 adapter 只持有 handle、IO 与读取观察。后台登记提交失败不启动命令；终态以原始 session/run/task 身份接纳，前台换代不丢失后台结果。任务摘要随下一次用户输入同事务保存。EOF/stop 收口进程树，冷恢复不自动重启副作用。

## 验证

| 检查         | 实际结果                                                                                                                                                                                 |
| ------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Rust         | 13 个测试通过：原有请求/存储/调度故障，加文件新鲜度、搜索、后台登记失败、进程回收、Unicode 大输出、非法 timeout 无副作用                                                                 |
| App/native   | 28 个集成测试通过；真实 Rust 子进程、当前 App ProtocolClient/schema/Host、桌面与手机双订阅；包括搜索→读取→Edit→后台测试→TaskOutput、跨会话拒绝、TaskStop、App 取消、EOF、旧模式恢复      |
| TS/Rust 差分 | 相同 fixture 调用当前 TS NodeFileSystemAdapter 及 Read/Write/Edit/Glob/Grep handlers；比较代表性读取内容/行数、搜索结果、修改后的文件，校验 schema；不是完整参数组合或全部工具的差分矩阵 |
| 静态         | cargo fmt、Clippy all-targets -D warnings、Rust 分层/文件长度、测试类型检查、pnpm typecheck 全通过                                                                                       |
| 仓库         | pnpm lint：0 errors / 70 个既有 warnings；architecture baseline 0 / new 0；feature graph、变更格式和 git diff --check 通过                                                               |

Node 的 SQLite ExperimentalWarning 属于现有测试运行时提示。测试不读取个人账号、不请求真实模型。本轮 Rust src/tests 相对开工快照 +1858/-348，净 +1510 行（不含生成 JSON、文档、测试 example 和 TS 测试）。改动集中于 Rust domain/app/adapters、App/native 测试及构建测试脚本；保留此前 App 启动接入改动。

## release 回归测量

Apple M1 Max / darwin arm64；Rust 1.95.0、Node 24.14.0、pnpm 10.33.2。第一包 release 与本包 release，串行交替，每版本/场景各五次，共 30 样本。耗时和 RSS 取中位数，RPC 是各次 p95 的中位数。首段列为后续回合样本中位数；启动列为首个 RPC。原始样本和 summary 在 `.zcode-runtime/rust-bench/coding-tools-final`。

| 负载                                | 总耗时：第一包 → 本包 | 本包启动 | 本包后续首段 | 本包 RPC p95 | 本包采样峰值 RSS |
| ----------------------------------- | --------------------: | -------: | -----------: | -----------: | ---------------: |
| 固定流式：1 会话 × 8 轮 × 2048 片段 |    269.47 → 271.07 ms |  8.00 ms |      1.51 ms |      0.59 ms |        28.78 MiB |
| 长历史：1 会话 × 100 轮 × 64 片段   |    347.03 → 347.46 ms |  7.26 ms |      0.99 ms |      0.40 ms |        24.95 MiB |
| 多会话：4 会话 × 8 轮 × 512 片段    |    242.73 → 243.29 ms |  7.50 ms |      1.94 ms |      1.15 ms |        29.42 MiB |

本测量验证新增工具定义和状态字段没有带来数量级的流式回归；小幅涨跌不作显著性结论。负载仍是本地确定性 SSE，不执行搜索/后台任务，不是新工具吞吐量或真实供应商生成速度测试。RSS 是采样峰值；存储字段为 SQLite/WAL/SHM 占用，不是物理写入量。

- baseline SHA-256：`518fca0037b6467f0429cae984b0fe859ada4b610c2a5893369d38f460a6aadf`，文件 `.zcode-runtime/rust-bench/coding-baseline`。
- candidate SHA-256：`2a5b6118099074423577d6174765ea69f328adb2ed8531ba0648d5f2c3cded78`，文件 `apps/zcode-rust/target/release/zcode-rust`。

复现：`node scripts/bench-rust-agent-suite.mjs .zcode-runtime/rust-bench/coding-baseline apps/zcode-rust/target/release/zcode-rust`。

## 明确保留的差异

Read 展示预算 64 KiB，写/编辑最多 8 MiB，Grep 单文件最多 16 MiB、输出 20 KB；这些预算与 TS 不完全一致。Edit 的宽松引号/缩进/锚点等策略、多媒体读取、完整供应商和全参数差分尚未实现。后台完成只更新 App 状态并附入下一次输入，不自动发起额外模型回合；冷恢复可以读输出文件，不能通过 TaskOutput 重新接管旧进程。Shell 每流内联 24 KiB、文件 16 MiB，超过文件预算停止命令。

完整 Electron Renderer、真实远端、Windows/Linux 实机、真实模型供应商未在本包验证。当前完成的范围以 `../specs/rust-coding-tools.md` 为准，尚未达到常用 Coding 全面替换里程碑 A；上下文压缩及 App Registry/账号/多模型协议仍是下一阶段。
