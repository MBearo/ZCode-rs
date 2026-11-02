//! 内置工具的名字（工作线 F126，docs/specs/rust-cli/README.md §4.1 第 8 条）：工具自己的 `Tool::name`、权限规则、hook 的
//! matcher、界面按名字的分支与按名字查的属性表都引用这里的常量；产品代码里不再写这些名字的字面量（边界检查拦）。
//! 测试可以照旧写字面量。旧名（`Task`、`KillShell` 等）在 [`crate::tool::LEGACY_TOOL_NAMES`]。

pub const AGENT: &str = "Agent";
pub const ASK_USER_QUESTION: &str = "AskUserQuestion";
pub const BASH: &str = "Bash";
pub const CRON_CREATE: &str = "CronCreate";
pub const CRON_DELETE: &str = "CronDelete";
pub const CRON_LIST: &str = "CronList";
pub const EDIT: &str = "Edit";
pub const ENTER_PLAN_MODE: &str = "EnterPlanMode";
pub const ENTER_WORKTREE: &str = "EnterWorktree";
pub const EXIT_PLAN_MODE: &str = "ExitPlanMode";
pub const EXIT_WORKTREE: &str = "ExitWorktree";
pub const GLOB: &str = "Glob";
pub const GREP: &str = "Grep";
pub const LIST_AGENTS: &str = "ListAgents";
pub const LIST_MCP_RESOURCES: &str = "ListMcpResourcesTool";
pub const MONITOR: &str = "Monitor";
/// 本工程没有这个工具：界面显示 App 与旧会话记下的调用、工作区信任的授权表按名字认它。
pub const MULTI_EDIT: &str = "MultiEdit";
pub const NOTEBOOK_EDIT: &str = "NotebookEdit";
/// 本工程没有这个工具：子代理的工具表与工作区信任的授权表按名字认它（与 shell 的名字无关）。
pub const POWER_SHELL: &str = "PowerShell";
pub const READ: &str = "Read";
pub const READ_MCP_RESOURCE: &str = "ReadMcpResourceTool";
pub const READ_MCP_RESOURCE_DIR: &str = "ReadMcpResourceDirTool";
pub const REPORT_FINDINGS: &str = "ReportFindings";
pub const SCHEDULE_WAKEUP: &str = "ScheduleWakeup";
pub const SEND_MESSAGE: &str = "SendMessage";
pub const SEND_USER_MESSAGE: &str = "SendUserMessage";
pub const SKILL: &str = "Skill";
pub const TASK_CREATE: &str = "TaskCreate";
pub const TASK_GET: &str = "TaskGet";
pub const TASK_LIST: &str = "TaskList";
pub const TASK_OUTPUT: &str = "TaskOutput";
pub const TASK_STOP: &str = "TaskStop";
pub const TASK_UPDATE: &str = "TaskUpdate";
pub const TODO_WRITE: &str = "TodoWrite";
/// 搜索并加载延迟工具。
pub const TOOL_SEARCH: &str = "ToolSearch";
/// 等连接中的 MCP 服务器。
pub const WAIT_FOR_MCP_SERVERS: &str = "WaitForMcpServers";
pub const WEB_FETCH: &str = "WebFetch";
pub const WEB_SEARCH: &str = "WebSearch";
pub const WRITE: &str = "Write";
