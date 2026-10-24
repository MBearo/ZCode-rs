//! 给模型看的文本（docs/specs/rust-cli/README.md §8、`docs/specs/rust-cli/prompts.md`）。
//!
//! 文本逐字固定，存为
//! `packages/rust-cli/prompts/` 下的数据文件，由 `scripts/rust-cli/prompts/extract.mjs` 生成并
//! 用 `--check` 检查漂移。这里在编译时嵌入这些文件，运行时没有 IO。代码里不手写这些文本。
//!
//! 编入一律用 `rust_cli_base::include_lf!`（修复线 F56，`docs/specs/rust-cli/prompts.md`「换行」）：以前用
//! `include_str!`，Windows 检出成 CRLF 时发给模型的文本带 `\r`；现在文本含 `\r` 就编译失败。
//!
//! | 模块 | 内容 |
//! | --- | --- |
//! | 根 | 工具描述与输入 schema（`prompts/tools/`） |
//! | [`texts`] | system prompt 段落与上下文 reminder（`prompts/system/`、`prompts/context/`） |
//! | [`models`] | 模型卡片：身份文本与请求默认值（`prompts/models.json`） |
//! | [`reminder`] | `<system-reminder>` 包装 |
//! | [`agents`] | 子代理的提示词、结果与通知（`prompts/agents/`） |
//! | [`title`] | 会话标题的提示词与常量（`prompts/title/`） |
//! | [`loops`] | `/loop` 的提示、触发时的短提醒与自主说明（`prompts/loop/`） |
//! | [`goal`] | `/goal` 的说明与 prompt hook 评估的提示词（`prompts/goal/`） |
//! | [`peers`] | 跨会话通信的包装、回执与工具结果的固定句子（`prompts/peers/`） |
//! | [`monitor`] | Monitor 的结果、事件、说明与随开关多出的句子（`prompts/monitor/`，F83） |

pub mod agents;
pub mod commands;
pub mod compact;
pub mod goal;
pub mod loops;
pub mod models;
pub mod monitor;
pub mod peers;
pub mod reminder;
pub mod texts;
pub mod title;
mod words;

pub use words::{SlugWords, slug_words};

use std::collections::HashMap;
use std::sync::OnceLock;

use rust_cli_base::include_lf;
use serde::Deserialize;
use serde_json::Value;

/// 数据文件末尾有一个换行（编辑器友好），加载时去掉。F58：以前 agents、title、loops、compact、goal、texts、peers
/// 各写一份，commands 内联一处。
pub(crate) fn trim(raw: &'static str) -> &'static str {
    raw.strip_suffix('\n').unwrap_or(raw)
}

/// 一个工具的描述与输入 schema。
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolPrompt {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// 新增工具时在这里登记它的数据文件。
const TOOL_FILES: &[&str] = &[
    include_lf!("../../../prompts/tools/Bash.json"),
    include_lf!("../../../prompts/tools/Edit.json"),
    include_lf!("../../../prompts/tools/Glob.json"),
    include_lf!("../../../prompts/tools/Grep.json"),
    include_lf!("../../../prompts/tools/NotebookEdit.json"),
    include_lf!("../../../prompts/tools/Read.json"),
    include_lf!("../../../prompts/tools/Write.json"),
    // 子代理与任务清单（docs/specs/rust-cli/agents.md）。
    include_lf!("../../../prompts/tools/Agent.json"),
    include_lf!("../../../prompts/tools/ListAgents.json"),
    include_lf!("../../../prompts/tools/SendMessage.json"),
    include_lf!("../../../prompts/tools/TaskStop.json"),
    include_lf!("../../../prompts/tools/TaskCreate.json"),
    include_lf!("../../../prompts/tools/TaskGet.json"),
    include_lf!("../../../prompts/tools/TaskList.json"),
    include_lf!("../../../prompts/tools/TaskUpdate.json"),
    // 交互、计划模式、延迟工具与 Web 工具（docs/specs/rust-cli/tools-more.md）。
    include_lf!("../../../prompts/tools/AskUserQuestion.json"),
    include_lf!("../../../prompts/tools/EnterPlanMode.json"),
    include_lf!("../../../prompts/tools/ExitPlanMode.json"),
    include_lf!("../../../prompts/tools/ToolSearch.json"),
    include_lf!("../../../prompts/tools/WebFetch.json"),
    // Monitor（docs/specs/rust-cli/tools-more.md §19，F83）。
    include_lf!("../../../prompts/tools/Monitor.json"),
    include_lf!("../../../prompts/tools/WebSearch.json"),
    // Cron、Worktree、ReportFindings（docs/specs/rust-cli/tools-more.md §10–§12）。
    include_lf!("../../../prompts/tools/CronCreate.json"),
    include_lf!("../../../prompts/tools/CronDelete.json"),
    include_lf!("../../../prompts/tools/CronList.json"),
    include_lf!("../../../prompts/tools/ReportFindings.json"),
    include_lf!("../../../prompts/tools/ScheduleWakeup.json"),
    include_lf!("../../../prompts/tools/EnterWorktree.json"),
    include_lf!("../../../prompts/tools/ExitWorktree.json"),
    // skill 与 MCP（docs/specs/rust-cli/ext.md）。
    include_lf!("../../../prompts/tools/Skill.json"),
    include_lf!("../../../prompts/tools/ListMcpResourcesTool.json"),
    include_lf!("../../../prompts/tools/ReadMcpResourceDirTool.json"),
    include_lf!("../../../prompts/tools/ReadMcpResourceTool.json"),
    // 连接中的 MCP 服务器（ext.md §3.8，修复线 F36）。
    include_lf!("../../../prompts/tools/WaitForMcpServers.json"),
];

fn tools() -> &'static HashMap<String, ToolPrompt> {
    static TOOLS: OnceLock<HashMap<String, ToolPrompt>> = OnceLock::new();
    TOOLS.get_or_init(|| {
        TOOL_FILES
            .iter()
            .filter_map(|text| serde_json::from_str::<ToolPrompt>(text).ok())
            .map(|prompt| (prompt.name.clone(), prompt))
            .collect()
    })
}

/// 按工具名取描述与 schema；没有登记的工具为 None。
pub fn tool(name: &str) -> Option<&'static ToolPrompt> {
    tools().get(name)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
