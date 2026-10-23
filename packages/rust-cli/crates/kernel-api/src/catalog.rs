//! 会话可用的工具、agent 与 skill 的清单（docs/specs/rust-cli/headless.md §3.3，修复线 F39）：`-p` 的
//! stream-json 在每个回合开始前写 `system/init`，它的 `tools`、`agents`、`skills` 从这里取。

use serde::{Deserialize, Serialize};

use crate::env::WorkspaceRef;
use crate::ids::SessionId;
use crate::model::ModelSelection;

/// 查哪个会话的清单。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogQuery {
    pub session: SessionId,
    pub workspace: WorkspaceRef,
    /// 会话当前的模型：决定 WebSearch 的去留与是否启用工具搜索（与主循环的工具快照同一规则）。
    pub selection: ModelSelection,
}

/// 清单。各项都是名字，顺序就是给出的顺序。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCatalog {
    /// 会话可用的全部工具（含延迟加载的），顺序：内置按名字，MCP 在后。
    pub tools: Vec<String>,
    /// agent 类型（内置、自定义与插件，按 agent 目录的顺序）。
    pub agents: Vec<String>,
    /// 用户可调用的 skill。
    pub skills: Vec<String>,
}

#[cfg(test)]
#[path = "catalog_tests.rs"]
mod tests;
