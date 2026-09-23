# Rust Coding 工具与 yolo

2026-09-22。第二交付包。实现前契约，以当前 TS contracts/tools 与 core/tool/handlers 为比较基准。

## 产品规则与范围

- 新会话只支持 yolo，普通文件与 Shell 工具不产生审批交互；显式 build/edit/plan/auto 被拒绝。Session 持久化 mode，投影、workspace presentation 和执行一致。旧 native 数据缺省仍为 build，只读恢复；用户显式 switchCollaborationMode(yolo) 后才可继续，不能在恢复时静默提权。
- Read 使用 file_path/offset/limit，1-based 行号（0 同 1），分页、有界内存、特殊/二进制文件拒绝。Write 使用 file_path/content；Edit 使用 file_path/old_string/new_string/replace_all。支持父目录创建、原子替换、UTF-8/BOM/CRLF、已有文件先读与新鲜度检查。常用确定性匹配先对齐，未实现的宽松匹配不得冒充成功。
- Glob/Grep 提供 TS 同名字段及结构化结果，原生执行并响应取消；Glob 最近修改优先最多 100 项；Grep 提供正则、glob/type、大小写、上下文、only-matching、多行和 offset/head_limit。尊重 ignore，不跟随目录 symlink，结果有界。未实现的方言/文件类型必须明确错误。
- 普通工具路径按 TS path-policy 解析：相对 cwd、可显式访问工作区外路径；yolo 不构成 OS sandbox。会话 identity 只用于状态隔离。
- Bash 使用 command/timeout/description/run_in_background/dangerouslyDisableSandbox，支持结构化退出码、取消/超时与大输出文件。TaskOutput 支持 task_id/block/timeout；TaskStop 支持 task_id/shell_id。非零退出是执行结果，不因解析成功当作命令成功。
- 后台任务可跨模型回合和正常前台结束；任务 ID 绑定 session，不能跨 session 查询/停止。上限每会话 16 个运行任务及 128 条任务记录，输出落独立数据目录并限制磁盘和内存占用。TaskOutput 等待可取消，TaskStop 等待进程树退出后返回。进程关闭、故障、显式 stop 收口所属任务；冷恢复不重启未知副作用，状态标 interrupted。
- App 复用 backgroundWorks 和 cancelBackgroundWork；任务完成后状态持久化，下一次用户输入可携带任务状态。此包不自动发起额外模型回合，也不扩展子代理任务。
- 附件/图像/PDF、上下文压缩、账号 Registry、多模型协议、完整扩展继续后续阶段。

## 所有者和顺序

Session actor 唯一持有模式、transcript、ACK、后台任务事实及投影。工具 adapter 只持有进程 handle、输出文件和 session-scoped 文件读取观察缓存；缓存不替代历史。任务开始先注册并等待 actor 的耐久提交，再启动 OS 进程；终态经同一 owner 持久化。后台终态按 session + 原始 runId + taskId 验证，不依赖当前前台 runId，不能接受未知任务。

```mermaid
sequenceDiagram
    participant App
    participant Owner as Session owner
    participant Loop as Agent loop
    participant Tool as Tool adapter
    participant DB
    App->>Owner: yolo input / ACK
    Owner->>DB: commit input
    Owner->>Loop: admitted run
    Loop->>Owner: model result / receipt
    Owner->>DB: commit model
    Owner-->>Loop: committed
    Loop->>Tool: execute (session/run scoped)
    Tool->>Owner: background registered / receipt
    Owner->>DB: commit task
    Owner-->>Tool: committed
    Tool->>Tool: spawn and capture bounded output
    Tool-->>Loop: task id + output path
    Loop->>Owner: tool result / receipt
    Owner->>DB: commit tool result
    Tool->>Owner: terminal task event
    Owner->>DB: commit terminal state
    Owner-->>App: existing backgroundWorks projection
```

## 验收

1. 真实 Rust 子进程：yolo 创建/恢复/拒绝其它模式，无 pending permission；文件修改和 Shell 真实执行。旧 build 会话不能未经显式切换执行。
2. 相同 TS schema 校验工具参数/输出；文件读取分页、重复匹配、replace_all、CRLF、未读/外部修改拒绝、跨会话缓存隔离、取消前提交、超大输出。
3. 搜索 fixtures：ignore、正则/Unicode、多行、上下文、类型/glob、分页、无匹配及错误参数；TS handlers 与原生结果做归一化对照。
4. 后台任务：创建后继续模型、跨回合 TaskOutput、blocking timeout、TaskStop/重复停止、跨会话拒绝、结束事件不受前台换代影响、App 双订阅 schema、EOF 收口和冷恢复。
5. 保留第一包模型、队列、背压及提交失败测试，更新审批用例为 yolo；Rust tests/fmt/Clippy、App/native、typecheck/lint、架构检查。
