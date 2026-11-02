// 词表与构造的规则表（工作线 F126）：枢纽 DTO 只经 kernel-api 的构造函数建（boundaries/constructors.mjs）；内置工具名只在
// kernel-api 的 `tool_names`（literals.mjs 的一条规则）。policy.mjs 重新导出这里的表，check-boundaries.mjs 照旧只读 policy。

/// 枢纽 DTO（architecture-review.md S0）：`owner` 以外的 crate 不用完整的结构体字面量构造它们。`hint` 是该用的构造函数；
/// `skipCrates` 里的 crate 有同名的别的类型（执行协议的镜像 DTO），不查。
export const DTO_CONSTRUCTORS = {
  owner: "kernel-api",
  types: [
    { name: "ModelSelection", hint: "ModelSelection::new(provider, model)" },
    {
      name: "ContextRequest",
      hint: "ContextRequest::new(session, workspace, source, mode, selection)",
    },
    {
      name: "QueryRequest",
      hint: "QueryRequest::new(session, run, workspace, messages, selection, mode, source)",
    },
    { name: "SessionView", hint: "SessionView::new(meta, messages, selection, mode)" },
    {
      name: "ModelCall",
      hint: "ModelCall::new(message, source, selection, started_at_ms, completed_at_ms, status)",
    },
    { name: "BackgroundEntry", hint: "BackgroundEntry::new(id, kind, description, status)" },
    { name: "HookEvent", hint: "HookEvent::new(name, session, workspace, payload)" },
    {
      name: "OperationContext",
      hint: "OperationContext::new(environment, generation, operation, origin)",
      skipCrates: ["exec-wire", "exec-client", "exec-server", "env-remote"],
    },
    {
      name: "McpTransportConfig::Stdio",
      hint: "McpTransportConfig::stdio(command, args)",
      defaults: {
        env: ["Default::default()", "BTreeMap::new()", "std::collections::BTreeMap::new()"],
        cwd: ["None"],
      },
    },
    { name: "McpTransportConfig::Sse", hint: "McpTransportConfig::sse(url, headers)" },
    { name: "McpTransportConfig::Http", hint: "McpTransportConfig::http(url, headers)" },
    {
      name: "ToolUseContext",
      hint: "rust_cli_kernel_api::testing::tool_use_context() (tests; the execution pipeline builds it)",
    },
    // 工作线 F137：执行协议有同名的镜像 DTO 与 `Request::StartCommand` 变体的 crate 不查（同 OperationContext）。
    {
      name: "StartCommand",
      hint: "StartCommand::new(command, cwd, task)",
      skipCrates: ["exec-wire", "exec-client", "exec-server", "env-remote"],
    },
    { name: "RunView", hint: "RunView::new(mode, workspace)" },
    // 工作线 F137：只批准这一次（`once: true`）只有判决链给出，照写字面量。
    {
      name: "PermissionDecision::Ask",
      hint: "PermissionDecision::ask(message, suggestions)",
      defaults: { once: ["false"] },
    },
  ],
};

/// 例外（按文件）：`{ files, types, reason }`。
export const DTO_CONSTRUCTOR_EXCEPTIONS = [
  {
    files: [
      "crates/query/src/executor.rs",
      "crates/session/src/slash.rs",
      "crates/compact/src/restore.rs",
    ],
    types: ["ToolUseContext"],
    reason:
      "执行管线（主循环、斜杠命令、压缩后重读文件）装出一次工具调用的上下文：12 个字段都来自调用方自己的状态，没有缺省值可省；穷举字面量让加字段时这里编译失败",
  },
  {
    files: ["crates/exec-server/src/convert_in.rs"],
    types: ["McpTransportConfig::Sse", "McpTransportConfig::Http"],
    reason: "执行协议的 MCP 传输换算成内核的配置：逐字段对应（同 X1 的穷举换算）",
  },
];

/// 内置工具名（README §4.1 第 8 条）：产品代码里整个字面量是这些名字时，只许在 kernel-api 的 `tool_names.rs`。是
/// literals.mjs 的一条规则（`VOLATILE_LITERALS` 的写法，policy.mjs 把它接在后面）；测试不查。旧名（`Task`、`KillShell` 等）
/// 不在里面。
export const TOOL_NAME_LITERAL = {
  name: "built-in tool name",
  pattern: new RegExp(
    `^(?:${[
      "Agent",
      "AskUserQuestion",
      "Bash",
      "CronCreate",
      "CronDelete",
      "CronList",
      "Edit",
      "EnterPlanMode",
      "EnterWorktree",
      "ExitPlanMode",
      "ExitWorktree",
      "Glob",
      "Grep",
      "ListAgents",
      "ListMcpResourcesTool",
      "Monitor",
      "MultiEdit",
      "NotebookEdit",
      "PowerShell",
      "Read",
      "ReadMcpResourceTool",
      "ReadMcpResourceDirTool",
      "ReportFindings",
      "ScheduleWakeup",
      "SendMessage",
      "SendUserMessage",
      "Skill",
      "TaskCreate",
      "TaskGet",
      "TaskList",
      "TaskOutput",
      "TaskStop",
      "TaskUpdate",
      "TodoWrite",
      "ToolSearch",
      "WaitForMcpServers",
      "WebFetch",
      "WebSearch",
      "Write",
    ].join("|")})$`,
  ),
  owners: ["crates/kernel-api/src/tool_names.rs"],
  tests: false,
  hint: "use rust_cli_kernel_api::tool_names",
};

/// 内置工具名的例外（同 `VOLATILE_EXCEPTIONS` 的写法，policy.mjs 把它接在后面）。
export const TOOL_NAME_EXCEPTIONS = [
  {
    files: ["crates/env-local/src/host.rs"],
    rules: ["built-in tool name"],
    reason: "Windows 上 shell 的名字 PowerShell（环境事实），不是工具名",
  },
];
