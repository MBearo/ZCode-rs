//! 工具的种类（架构整理 U8）：按名字只认一次，记在工具行上（`ToolRow::kind`）；排版、分组、改动的投影与询问框都按
//! 种类匹配。工具名只写在 [`ToolKind::of`] 里（以前 `"Bash"`、`"Edit"` 等散在 8 个文件，“改文件的工具”有 4 种写法）。
use rust_cli_kernel_api::tool_names;
use rust_cli_kernel_api::tool_names::WAIT_FOR_MCP_SERVERS;

/// Bash 工具的名字（询问框写“不再询问”的规则时也用它）。
pub const BASH: &str = tool_names::BASH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    Bash,
    Read,
    Grep,
    Glob,
    /// `List` 与旧名 `LS`。
    List,
    Edit,
    MultiEdit,
    Write,
    NotebookEdit,
    WebSearch,
    WebFetch,
    AskUserQuestion,
    ExitPlanMode,
    /// `Agent` 与旧名 `Task`。
    Agent,
    TodoRead,
    ToolSearch,
    WaitForMcpServers,
    /// MCP 服务器的工具。
    Mcp,
    /// 其他工具：参数按 JSON 摘要显示。
    Other,
}

impl ToolKind {
    pub fn of(name: &str) -> Self {
        match name {
            BASH => Self::Bash,
            tool_names::READ => Self::Read,
            tool_names::GREP => Self::Grep,
            tool_names::GLOB => Self::Glob,
            "List" | "LS" => Self::List,
            tool_names::EDIT => Self::Edit,
            tool_names::MULTI_EDIT => Self::MultiEdit,
            tool_names::WRITE => Self::Write,
            tool_names::NOTEBOOK_EDIT => Self::NotebookEdit,
            tool_names::WEB_SEARCH => Self::WebSearch,
            tool_names::WEB_FETCH => Self::WebFetch,
            tool_names::ASK_USER_QUESTION => Self::AskUserQuestion,
            tool_names::EXIT_PLAN_MODE => Self::ExitPlanMode,
            tool_names::AGENT | "Task" => Self::Agent,
            "TodoRead" => Self::TodoRead,
            tool_names::TOOL_SEARCH => Self::ToolSearch,
            WAIT_FOR_MCP_SERVERS => Self::WaitForMcpServers,
            _ if rust_cli_kernel_api::mcp_name::is_mcp(name) => Self::Mcp,
            _ => Self::Other,
        }
    }

    /// 改文件的工具：结果画成 diff，算进每回合的改动（`/diff`、rewind 的统计）。
    pub fn edits_files(self) -> bool {
        match self {
            Self::Edit | Self::MultiEdit | Self::Write | Self::NotebookEdit => true,
            Self::Bash
            | Self::Read
            | Self::Grep
            | Self::Glob
            | Self::List
            | Self::WebSearch
            | Self::WebFetch
            | Self::AskUserQuestion
            | Self::ExitPlanMode
            | Self::Agent
            | Self::TodoRead
            | Self::ToolSearch
            | Self::WaitForMcpServers
            | Self::Mcp
            | Self::Other => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use rust_cli_kernel_api::tool_names;

    use super::ToolKind;

    #[test]
    fn names_map_to_kinds_once() {
        assert_eq!(ToolKind::of("LS"), ToolKind::List);
        assert_eq!(ToolKind::of("Task"), ToolKind::Agent);
        assert_eq!(ToolKind::of("mcp__github__search"), ToolKind::Mcp);
        assert_eq!(ToolKind::of(tool_names::SKILL), ToolKind::Other);
        let edits: Vec<_> = [
            tool_names::EDIT,
            tool_names::MULTI_EDIT,
            tool_names::WRITE,
            tool_names::NOTEBOOK_EDIT,
            tool_names::BASH,
            tool_names::READ,
        ]
        .into_iter()
        .filter(|name| ToolKind::of(name).edits_files())
        .collect();
        assert_eq!(
            edits,
            [
                tool_names::EDIT,
                tool_names::MULTI_EDIT,
                tool_names::WRITE,
                tool_names::NOTEBOOK_EDIT
            ]
        );
    }
}
