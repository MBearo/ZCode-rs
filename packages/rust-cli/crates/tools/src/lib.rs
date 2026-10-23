//! 工具注册、执行管线与内置工具（docs/specs/rust-cli/README.md §3、§6 规则 2）。
//!
//! - 注册：内置工具 + MCP + 插件，按名字与别名查找；
//! - 管线：validate → PreToolUse → 权限判决链 → call → PostToolUse / PostToolUseFailure；
//!   结果超过预算时落盘为 Artifact，只把预览交给模型（[`budget`]）；
//! - 内置工具：一个工具一个模块。文件与搜索（Read、Write、Edit、Glob、Grep、NotebookEdit）已实现，
//!   见 docs/specs/rust-cli/tools-files.md；Bash 见 docs/specs/rust-cli/shell-permissions.md；
//!   AskUserQuestion、计划模式、ToolSearch 与 Web 工具、Cron、Worktree、ReportFindings 见
//!   docs/specs/rust-cli/tools-more.md。
//!
//! 工具只做语义，IO 都经 `ToolUseContext::env`；readFileState 以会话为键（[`file_state`]）。

mod ask_user_question;
mod asking;
mod base64;
mod bash;
mod bash_mode;
pub mod bash_output;
mod bash_timeouts;
pub mod budget;
mod checkpoint;
mod common;
mod cron_board;
mod cron_durable;
mod cron_expr;
mod cron_list;
mod cron_loop;
mod cron_resume;
mod cron_texts;
mod cron_tools;
mod cron_wakeups;
mod edit;
mod edit_match;
mod encoding;
pub mod file_state;
mod file_state_restore;
mod glob;
mod grep;
mod grep_output;
mod html_markdown;
mod mentions;
mod notebook;
mod notebook_edit;
mod pipeline;
mod pipeline_hooks;
mod plan_mode;
mod policy_filter;
mod read;
mod read_pdf;
mod read_support;
mod registry;
mod report_findings;
mod schedule_wakeup;
mod session_slug;
#[cfg(test)]
mod test_more;
mod texts;
mod texts_more;
mod tool_search;
mod tool_search_keywords;
mod tool_search_mcp;
mod user_input;
mod validate;
mod web_cache;
mod web_fetch;
mod web_search;
mod web_side;
mod web_texts;
mod worktree;
mod worktree_exit;
mod worktree_texts;
mod write;

use std::sync::Arc;

use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::orchestration::PermissionGate;
use rust_cli_kernel_api::tool::Tool;

pub use ask_user_question::AskUserQuestionTool;
pub use bash::{BashTimeouts, BashTool};
pub use common::FileTools;
pub use cron_board::CronBoard;
pub use cron_tools::cron_tools;
pub use edit::EditTool;
pub use file_state::ReadFileState;
pub use file_state_restore::restore_file_state;
pub use glob::GlobTool;
pub use grep::GrepTool;
pub use notebook_edit::NotebookEditTool;
pub use pipeline::Pipeline;
pub use plan_mode::{EnterPlanModeTool, ExitPlanModeTool};
pub use policy_filter::PolicyFiltered;
pub use read::ReadTool;
pub use registry::BuiltinTools;
pub use report_findings::ReportFindingsTool;
pub use schedule_wakeup::ScheduleWakeupTool;
pub use tool_search::{ToolCatalog, ToolSearchTool};
pub use user_input::UserInputTools;
pub use web_fetch::{SideModel, WebFetchTool};
pub use web_search::WebSearchTool;
pub use worktree::{WorktreeBoard, worktree_tools};
pub use write::WriteTool;

/// 文件与搜索工具，顺序固定（Glob、Grep 在 Read 之前）。
pub fn file_tools(files: &FileTools) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(GlobTool::new()),
        Arc::new(GrepTool::new()),
        Arc::new(ReadTool::new(files.clone())),
        Arc::new(EditTool::new(files.clone())),
        Arc::new(WriteTool::new(files.clone())),
        Arc::new(NotebookEditTool::new(files.clone())),
    ]
}

/// 交互、计划模式与 ToolSearch（docs/specs/rust-cli/tools-more.md）。`interactive` 为假（`-p`）时
/// AskUserQuestion 与计划模式的两个工具不可用（非交互会话里不提供它们）；`catalog` 由 runtime
/// 在主线程注册表建好后绑定，同时作为 `deferred_tools_delta` 的清单来源。
pub fn interaction_tools(catalog: &Arc<ToolCatalog>, interactive: bool) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(AskUserQuestionTool::new(interactive)),
        Arc::new(EnterPlanModeTool::new(interactive)),
        Arc::new(ExitPlanModeTool::new(interactive)),
        Arc::new(ToolSearchTool::new(catalog.clone())),
    ]
}

/// Web 工具（docs/specs/rust-cli/tools-more.md §4、§5）：WebFetch 与 WebSearch 的旁路模型请求用 `side`；
/// WebFetch 的页面缓存按 `clock` 过期。
pub fn web_tools(side: SideModel, clock: Arc<dyn Clock>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(WebFetchTool::new(Some(side.clone()), clock)),
        Arc::new(WebSearchTool::new(side)),
    ]
}

/// 内置工具集：文件与搜索工具共用一份 readFileState。`gate` 用于判断写入会不会被直接放行
/// （Edit / Write 的先读规则，tools-files.md §1.2），应与执行管线用同一套判决。
pub fn builtin(gate: Arc<dyn PermissionGate>) -> BuiltinTools {
    builtin_with(gate, None)
}

/// 同 [`builtin`]，另给 Read 查模型能力用的模型端口（整读 PDF 前看 `supports_pdf`，tools-files.md §1.5）。
pub fn builtin_with(gate: Arc<dyn PermissionGate>, model: Option<SideModel>) -> BuiltinTools {
    let state = Arc::new(ReadFileState::default());
    let mut files = FileTools::new(state.clone(), gate);
    if let Some(model) = model {
        files = files.with_model(model);
    }
    BuiltinTools::new(file_tools(&files)).with_file_state(state)
}
