# Rust session/list 历史身份查询

2026-09-22。真实 App Host 的 repairSubagentTaskIndex 调用 session/list 时收到 Unsupported method。本包复用既有请求/结果 schema，不新增协议版本或第二个任务索引。

## 产品规则

- 参数遵循当前严格 `zcodeSessionListParamsSchema`：可选 workspace、1–64 个非空 sessionIds、includeArchived=false、正整数 limit。所有身份字符串先 trim；null 参数对象等同省略；可选字段显式 null、未知字段、空白 ID/空数组/超 64 个 ID、非整数或非正 limit 均拒绝。
- 普通列表只返回已持久化 interactive/fork/workflow_parent，默认最多 50 条，按 updatedAt DESC、id DESC；无 offset/cursor，不伪造分页协议。显式 IDs 保持请求顺序和重复项，包含隐藏子会话，忽略 limit，缺失 ID 跳过。
- includeArchived 控制归档过滤。workspace 指定时按 trim(identity) || path 匹配，普通列表同时保留 TS directory 查询约束；显式 ID 查询只以身份过滤。workspaceKey 仅回显，不能代替身份鉴别。remoteSessionId 透传。
- 未指定 workspace 时查询存储中的跨 workspace 历史，返回每条记录自身的身份与路径。不能把其他 workspace 的记录标成当前 cwd；旧元数据缺少路径时使用已保存 prompt cwd、当前 owner 路径或已提交 TS 备份，只读补全。无法确认远端路径时失败，不能猜测。
- 未提交首发的 draft 不进入历史列表，关闭 runtime 的持久化历史仍可查询。参数 ID 查询不恢复会话、不启动模型、不消费队列、不建立订阅、不改变序号或写数据库。
- 结果是历史身份，不是实时执行投影：与 TS stored mapSessionInfo 一样使用 status=idle、mode=build，不输出未提供的 target/model；实时状态继续由 session/read 与 V4 提供。保留 title/titleSource、parent、task kind、trace、创建/更新时间和归档时间。
- 持久化及旧 SessionInfo 保留 first_input 标题来源；V4 meta/index 按 TS product projection 映射为 generated，遵守各自 schema，不改变存储事实。

## 所有权与性能

```mermaid
sequenceDiagram
    participant Host as Host 索引修复
    participant Actor as Engine 请求入口
    participant Store as SQLite worker
    Host->>Actor: session/list (workspace + sessionIds)
    Actor->>Actor: 严格解析参数
    Actor->>Store: 只读 metadata 查询
    Store-->>Actor: 历史身份（无 rows/messages）
    Actor-->>Host: 既有 SessionInfo[]
    Note over Host: 仅确认 subagent_child 后更新派生索引
```

SessionStore 增加有类型的只读 listing 端口，SQLite adapter 负责查询；Session actor 仍是运行事实唯一 owner。普通查询使用 workspace/更新时间表达式索引，ID 查询使用主键或 ID 索引；只选 identity 元数据，不反序列化正文、附件或 Todo。响应按既有 900 KiB RPC 预算有界，超限明确失败，不静默丢条目。查询失败不改会话事实或回执。

新增 metadata workspacePath/workspaceDirectory/traceId，由原生创建和 TS 导入填写。directory 用于普通查询，path 用于返回真实操作路径，两者不能合并。已知 directory 且 traceId=null 的新记录不读旧备份；旧记录每次查询按 workspace 复用只读备份连接，补全不写回生产库、不产生新备份。本包不改变启动时 Engine 仍加载历史的现有行为；全量按需加载继续单列。

与 TS 保留的边界差异：无 workspace 查询仍保留远端 identity；workspace identity 在 limit 前过滤，避免其他身份占用限额；不把启动时加载的所有历史误当 TS live runtime 追加到 limit 后。TS 的限额后追加 live runtime 语义需随按需加载、显式 runtime 生命周期一起对齐，不能据此宣称整个旧列表行为完全等价。

## 验收

- 真 Rust 子进程/现有 App schema：普通列表、默认/显式 limit、排序、隐藏 child、归档、请求顺序/重复 ID、无效参数、空库、draft 与关闭历史。
- 使用真实 TS store/list mapper 做身份结果差分，包含 remote identity、同路径不同身份、无 workspace 查询和历史 trace/titleSource。
- 读查询不写库、不加载正文、不修改 V4 水位/订阅，不请求模型；存储失败与超大响应不伪造成功。
- 直接运行现有 Host repairSubagentTaskIndex 对真实 Rust 查询：只标记同身份且明确为 subagent_child 的派生索引，保留缺失、普通会话和 stale owner 情况。
- 真 App 重启后确认 session/list 获得合法回复，索引修复不再因缺接口失败；既有 workflowRuns 未实现单列，不能混同。
- Rust tests/fmt/Clippy、App tests、typecheck、lint、fmt、架构检查；CARGO_INCREMENTAL=0。
