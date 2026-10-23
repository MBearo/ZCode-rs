//! 工具注册表。

use std::sync::Arc;

use rust_cli_kernel_api::orchestration::ToolRegistry;
use rust_cli_kernel_api::tool::Tool;

use crate::file_state::ReadFileState;
use crate::{glob, grep};

/// Bash 工具的名字（工作线 L4 注册）。
pub(crate) const BASH: &str = "Bash";

/// 当前可用的工具。由 runtime 组装：内置工具、子代理工具、MCP 工具。
///
/// Glob / Grep 的暴露规则：
/// 有 Bash 时由 Bash 承担文件名与内容搜索，Glob / Grep 不出现；没有 Bash，或启动参数
/// （`--tools` / `--allowedTools`）点名了 Glob 或 Grep 时才出现（tools-files.md §1.1）。
#[derive(Default)]
pub struct BuiltinTools {
    tools: Vec<Arc<dyn Tool>>,
    search_opt_in: bool,
    file_state: Option<Arc<ReadFileState>>,
}

impl BuiltinTools {
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        Self {
            tools,
            search_opt_in: false,
            file_state: None,
        }
    }

    /// 启动参数点名了 Glob 或 Grep。
    pub fn with_search_opt_in(mut self, opt_in: bool) -> Self {
        self.search_opt_in = opt_in;
        self
    }

    /// 加一个排在前面的工具（Bash 在文件与搜索工具之前）。
    #[must_use]
    pub fn with_leading(mut self, tool: Arc<dyn Tool>) -> Self {
        self.tools.insert(0, tool);
        self
    }

    /// 追加工具（顺序不影响请求：发给模型前按名字排序，tools-more.md §3.3）。
    #[must_use]
    pub fn with_tools(mut self, tools: Vec<Arc<dyn Tool>>) -> Self {
        self.tools.extend(tools);
        self
    }

    pub(crate) fn with_file_state(mut self, state: Arc<ReadFileState>) -> Self {
        self.file_state = Some(state);
        self
    }

    /// 文件工具共用的 readFileState；会话的 owner 用它在关闭时释放、冷恢复时重建。
    pub fn file_state(&self) -> Option<&Arc<ReadFileState>> {
        self.file_state.as_ref()
    }

    fn exposed(&self, tool: &dyn Tool) -> bool {
        let search = tool.name() == glob::NAME || tool.name() == grep::NAME;
        if !search || self.search_opt_in {
            return true;
        }
        !self
            .tools
            .iter()
            .any(|other| other.name() == BASH && other.is_enabled())
    }
}

impl ToolRegistry for BuiltinTools {
    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        self.tools
            .iter()
            .filter(|tool| self.exposed(tool.as_ref()))
            .cloned()
            .collect()
    }

    fn find(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .filter(|tool| self.exposed(tool.as_ref()))
            .find(|tool| tool.name() == name || tool.aliases().contains(&name))
            .cloned()
    }

    /// 被 Bash 藏起来的 Glob / Grep（子代理点名它们、又没有 Bash 时补回，agents.md §4.2，修复线 F16）。
    fn withheld(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .filter(|tool| !self.exposed(tool.as_ref()))
            .find(|tool| tool.name() == name)
            .cloned()
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
