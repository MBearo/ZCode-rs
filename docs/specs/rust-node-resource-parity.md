# Node.js 与 Rust App Server 资源及功能对照

## 目标与范围

比较当前源码中两个 `app-server --stdio` 实现的进程资源开销，并以当前源码和验收报告列出功能对齐进度。仅比较 Agent 子进程；不把 Electron、Host、基准驱动、模型 fixture 或其他子进程计入 RSS/CPU。结果是本机确定性负载，不代表真实供应商、桌面总资源或发布平台。

## 测量契约

- 由 `scripts/bench-zcode-cli-node-rust.mjs` 独占 fixture、工作区、临时 HOME、模型服务及采样。Node 使用当前源码构建的 `zcode.cjs`；Rust 使用当前源码的 release 二进制。两者串行交错运行，每场景各五次。
- 两端使用同一临时 OpenAI Chat Completions SSE 服务、同一模型选型、相同输入、回合数、chunk 数和上下文窗口。各自使用独立的空 SQLite/配置目录，不导入用户历史。Node 的 Provider Registry 与 Rust 的静态模型配置都指向该 fixture；配置方式的差异须在报告披露。
- 至少覆盖空闲启动与单会话固定流式负载。所有回合必须完成并收到预期数量的模型请求；失败样本不得纳入汇总。
- `ps` 采样进程 RSS 与累积 CPU time。记录启动后空闲 RSS、负载采样峰值 RSS、负载 CPU 秒、负载墙钟秒和由两者计算的平均单核 CPU 百分比。峰值 RSS 仅是采样最大值；CPU 百分比可能超过 100%，表示占用多个核心。记录采样间隔、版本、机器和原始 JSON。
- 启动和负载测量的状态边界为：进程 spawn → `runtime/capabilities` 成功 → createSession/subscribe → sendText 回合 → completedSuccess → 退出。基准驱动只读采样，不写入 runtime 业务状态；两个 runtime 各自持有会话与持久化事实。不存在跨进程重放或共享队列。
- 报告给出中位数和逐次样本位置，不把微小差异解释为稳定的语言性能优势。若两端无法执行等价负载，明确记为未完成，不填推测数值。

## 功能清单契约

- `docs/reports/` 下的对照报告按「已完成核心」「部分完成」「待完成/发布门槛」列 TODO。勾选只表示当前 Rust 实现及已有自动化或 App 验收覆盖对应表述，不能扩大为 TypeScript 全功能等价。
- 每项未完成能力写明具体差异或缺少的验收场景。以 `docs/specs/rust-parity-remaining.md` 和当前源码为依据；旧报告的阶段性“未实现”若已被后续交付覆盖，不再照抄。
