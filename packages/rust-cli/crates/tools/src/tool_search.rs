//! ToolSearch 与延迟工具清单（docs/specs/rust-cli/tools-more.md §3）。
//!
//! [`ToolCatalog`] 是主线程注册表的弱引用：注册表在上下文之后才建好，由 runtime 绑定；ToolSearch 的
//! 查找范围与 `deferred_tools_delta` 清单都从这里取，两处看到的延迟工具总是同一组。
//!
//! 先建后接是为了打破构造的环（注册表 → ToolSearch 工具 → 目录 → 注册表；注册表跨 tools、ext、agents 三个
//! crate 组装，不能一次建好），保留最小形式：绑定一次。缺陷修复（审查 R1-13）：原来没绑定或注册表已释放时静默
//! 当作没有工具（ToolSearch 什么都找不到、延迟清单为空），现在两处都返回错误。

use std::sync::{Arc, OnceLock, Weak};

use async_trait::async_trait;
use regex::Regex;
use rust_cli_kernel_api::listing::{ListingEntry, ListingSource, Listings};
use rust_cli_kernel_api::mcp_state::McpConnections;
use rust_cli_kernel_api::message::ToolResultContent;
use rust_cli_kernel_api::orchestration::{ContextRequest, OrchestrationError, ToolRegistry};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use rust_cli_kernel_api::tool_search::{TOOL_SEARCH, compare_names, is_deferred};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::common::{Definition, definition, text};
use crate::texts_more as texts;
use crate::tool_search_keywords::{self as keywords, Candidate};
use crate::tool_search_mcp;

/// 主线程注册表的弱引用（避免 注册表 → 工具 → 目录 → 注册表 的 `Arc` 环）。
#[derive(Default)]
pub struct ToolCatalog {
    registry: OnceLock<Weak<dyn ToolRegistry>>,
}

impl ToolCatalog {
    /// 建好主线程注册表后绑定一次；之后的绑定被忽略。
    pub fn bind(&self, registry: &Arc<dyn ToolRegistry>) {
        let _ = self.registry.set(Arc::downgrade(registry));
    }

    fn registry(&self) -> Result<Arc<dyn ToolRegistry>, String> {
        let bound = self
            .registry
            .get()
            .ok_or_else(|| "the tool catalog is not bound to the main tool registry".to_owned())?;
        bound
            .upgrade()
            .ok_or_else(|| "the main tool registry has been released".to_owned())
    }

    /// 主线程注册表的 MCP 连接（连接中的服务器，tools-more.md §3.5.1）；没有时 None。
    fn mcp(&self) -> Option<Arc<dyn McpConnections>> {
        self.registry().ok()?.mcp()
    }

    /// 启用的工具，按名字排序（与发给模型的顺序相同）。
    fn tools(&self) -> Result<Vec<Arc<dyn Tool>>, String> {
        let mut tools: Vec<_> = self
            .registry()?
            .tools()
            .into_iter()
            .filter(|tool| tool.is_enabled())
            .collect();
        tools.sort_by(|left, right| compare_names(left.name(), right.name()));
        Ok(tools)
    }

    /// 这些名字里延迟的工具名，按 UTF-16 码元序（`deferred_tools_delta` 的清单顺序）。
    fn deferred_names(&self, names: &[String]) -> Result<Vec<String>, String> {
        let registry = self.registry()?;
        let mut deferred: Vec<String> = names
            .iter()
            .filter(|name| {
                registry
                    .find(name)
                    .is_some_and(|tool| tool.is_enabled() && is_deferred(tool.as_ref()))
            })
            .cloned()
            .collect();
        // 缺陷修复（R4 F1）：原来按不分大小写的 `compare_names`（近似工具池的 `localeCompare`），
        // MCP 工具夹在内置工具中间。清单按 JS 缺省排序（UTF-16 码元序）：大写开头的内置工具在小写开头的
        // MCP 工具之前。工具名是 ASCII，按字节比较与码元序相同。agent 清单仍按 `localeCompare`（context `listings.rs`）。
        deferred.sort();
        deferred.dedup();
        Ok(deferred)
    }
}

/// `deferred_tools_delta` 的清单来源：启用 ToolSearch 时，这一步工具集里的延迟工具，每行是工具名。
#[async_trait]
impl ListingSource for ToolCatalog {
    async fn listings(&self, request: &ContextRequest) -> Result<Listings, OrchestrationError> {
        let has_search = request.tool_names.iter().any(|name| name == TOOL_SEARCH);
        let supported = rust_cli_prompts::models::supports_tool_search(
            &request.selection.model,
            request.capabilities.map(|found| found.supports_tool_search),
        );
        if !has_search || !supported {
            return Ok(Listings::default());
        }
        let deferred_tools = self
            .deferred_names(&request.tool_names)
            .map_err(|message| OrchestrationError::Failed { message })?
            .into_iter()
            .map(|name| ListingEntry {
                line: name.clone(),
                name,
            })
            .collect();
        Ok(Listings {
            deferred_tools,
            ..Listings::default()
        })
    }
}

pub struct ToolSearchTool {
    definition: Definition,
    catalog: Arc<ToolCatalog>,
}

impl ToolSearchTool {
    pub fn new(catalog: Arc<ToolCatalog>) -> Self {
        Self {
            definition: definition(TOOL_SEARCH),
            catalog,
        }
    }

    fn candidates(tools: &[Arc<dyn Tool>], all: &[Arc<dyn Tool>]) -> Vec<Candidate> {
        let names: Vec<String> = all.iter().map(|tool| tool.name().to_owned()).collect();
        let prompt = PromptContext {
            tool_names: names,
            mode: PermissionMode::Default,
        };
        tools
            .iter()
            .map(|tool| Candidate {
                name: tool.name().to_owned(),
                description: tool.description(&prompt),
                hint: tool.search_hint().map(str::to_owned),
                mcp: tool.is_mcp(),
            })
            .collect()
    }

    /// `select:A,B`：先在延迟工具里找，再在全部工具里找（已加载的工具，视为无害的空操作），再按 MCP
    /// 工具名找。返回找到的工具名与找不到的名字（按出现的先后）。
    fn select(
        list: &str,
        deferred: &[Arc<dyn Tool>],
        all: &[Arc<dyn Tool>],
    ) -> (Vec<String>, Vec<String>) {
        let mut found: Vec<String> = Vec::new();
        let mut missing: Vec<String> = Vec::new();
        let by_name = |tools: &[Arc<dyn Tool>], name: &str| {
            tools
                .iter()
                .find(|tool| tool.name() == name || tool.aliases().contains(&name))
                .map(|tool| vec![tool.name().to_owned()])
        };
        for name in list
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let matched = by_name(deferred, name)
                .or_else(|| by_name(all, name))
                .unwrap_or_else(|| {
                    all.iter()
                        .filter(|tool| keywords::mcp_tool_name(tool.name()) == Some(name))
                        .map(|tool| tool.name().to_owned())
                        .collect()
                });
            if matched.is_empty() {
                missing.push(name.to_owned());
            }
            for name in matched {
                if !found.contains(&name) {
                    found.push(name);
                }
            }
        }
        (found, missing)
    }

    /// 在这些工具里搜（`select:` 时只看 `list` 里的名字，否则按关键词）。
    fn search(
        query: &str,
        select: Option<&str>,
        max_results: usize,
        all: &[Arc<dyn Tool>],
    ) -> Vec<String> {
        let deferred: Vec<Arc<dyn Tool>> = all
            .iter()
            .filter(|tool| is_deferred(tool.as_ref()))
            .cloned()
            .collect();
        match select {
            Some(list) => Self::select(list, &deferred, all).0,
            None => keywords::search(
                query,
                &Self::candidates(&deferred, all),
                &all.iter()
                    .map(|tool| tool.name().to_owned())
                    .collect::<Vec<_>>(),
                max_results,
            ),
        }
    }
}

fn select_query(query: &str) -> Option<&str> {
    static SELECT: OnceLock<Option<Regex>> = OnceLock::new();
    SELECT
        .get_or_init(|| Regex::new(r"(?is)^select:(.+)$").ok())
        .as_ref()?
        .captures(query)
        .and_then(|captures| captures.get(1))
        .map(|list| list.as_str())
}

#[async_trait]
impl Tool for ToolSearchTool {
    fn name(&self) -> &str {
        TOOL_SEARCH
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(100_000)
    }

    /// 没有专属的权限检查，默认放行。
    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let query = input
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let max_results = input
            .get("max_results")
            .and_then(Value::as_f64)
            .filter(|max| max.is_finite() && *max >= 0.0)
            .map_or(5, |max| max as usize);
        let all = self
            .catalog
            .tools()
            .map_err(|message| ToolError::Failed { message })?;
        let select = select_query(query);
        // 先照常搜；`select:` 时记下找不到的名字。
        let (mut matches, missing) = match select {
            Some(list) => {
                let deferred: Vec<Arc<dyn Tool>> = all
                    .iter()
                    .filter(|tool| is_deferred(tool.as_ref()))
                    .cloned()
                    .collect();
                Self::select(list, &deferred, &all)
            }
            None => (Self::search(query, None, max_results, &all), Vec::new()),
        };
        // 搜不到时看连接中的 MCP 服务器：等它们、再搜一次（tools-more.md §3.5.1，修复线 F36）。
        let mcp = self.catalog.mcp();
        let retry = if select.is_some() {
            !missing.is_empty()
        } else {
            matches.is_empty()
        };
        if retry && let Some(mcp) = &mcp {
            // `select:` 只重找找不到的那些名字；点名的文本也是它们。
            let missing_list = missing.join(",");
            let (text_for, list) = match select {
                Some(_) => (missing.join(" "), Some(missing_list.as_str())),
                None => (query.to_owned(), None),
            };
            let tools = || self.catalog.tools();
            let search = |tools: &[Arc<dyn Tool>]| Self::search(query, list, max_results, tools);
            let found =
                tool_search_mcp::retry(mcp.as_ref(), &all, &text_for, tools, search, &context)
                    .await?;
            for name in found {
                if !matches.contains(&name) {
                    matches.push(name);
                }
            }
        }
        if matches.is_empty() {
            let note = tool_search_mcp::pending_note(mcp.as_deref());
            return Ok(text(format!("{}{note}", texts::NO_DEFERRED_MATCH)));
        }
        Ok(ToolOutput {
            content: matches
                .into_iter()
                .map(|tool_name| ToolResultContent::ToolReference { tool_name })
                .collect(),
            is_error: false,
            display: Value::Null,
        })
    }
}

#[cfg(test)]
#[path = "tool_search_tests.rs"]
mod tests;
