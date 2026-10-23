//! ToolSearch 遇到连接中的 MCP 服务器（docs/specs/rust-cli/tools-more.md §3.5.1，修复线 F36）：等待、重试与补充句。
//!
//! 搜不到、又有 Pending 的服务器时，查询点名的服务器为空或其中有一个在 Pending，就等它们离开 Pending（最多 5 s，
//! 连接状态的变化通知加截止时刻，不轮询），再取一次工具、重新搜。

use std::sync::Arc;

use regex::Regex;
// 补充句里最多列出的服务器数（30）：只定义在 kernel-api（F58：以前这里与 context 各写一份）。
use rust_cli_kernel_api::mcp_state::PENDING_LISTED as LISTED;
use rust_cli_kernel_api::mcp_state::{McpConnections, McpView, normalize_name};
use rust_cli_kernel_api::tool::{Tool, ToolError, ToolUseContext};

/// 最多等多久（5000 ms）。
const WAIT_MS: u64 = 5_000;

/// 查询点名的服务器：文本里 `mcp__<段>` 的服务器段（到 `__` 为止），加上名字按词边界（不分大小写）
/// 出现在文本里的服务器；去重，按出现的先后。
pub(crate) fn mentioned_servers(text: &str, servers: &[String]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        if !found.iter().any(|seen| seen == name) {
            found.push(name.to_owned());
        }
    };
    if let Ok(prefix) = Regex::new(r"mcp__([a-zA-Z0-9._-]+)") {
        for captures in prefix.captures_iter(text) {
            let segment = &captures[1];
            add(segment
                .split_once("__")
                .map_or(segment, |(server, _)| server));
        }
    }
    let lower = text.to_lowercase();
    for name in servers {
        // JS 的 `\b` 只认 ASCII 的单词字符。
        let pattern = format!(r"(?i)(?-u:\b){}(?-u:\b)", regex::escape(name));
        if Regex::new(&pattern).is_ok_and(|word| word.is_match(&lower)) {
            add(name);
        }
    }
    found
}

/// 点名的服务器里有一个在 Pending（原名或规范化后的名字）；没有点名时为真。
fn targets_pending(targets: &[String], pending: &[String]) -> bool {
    let normalized: Vec<String> = pending.iter().map(|name| normalize_name(name)).collect();
    targets.is_empty()
        || targets
            .iter()
            .any(|target| pending.contains(target) || normalized.contains(target))
}

/// 等到没有 Pending 的服务器，或点名的服务器都不在 Pending。
fn settled(view: &McpView, targets: &[String]) -> bool {
    let pending = view.pending();
    pending.is_empty() || (!targets.is_empty() && !targets_pending(targets, &pending))
}

/// 搜不到时的重试：工具有新增或有 Pending 的服务器才做；新增时先在新的工具里搜；仍没有结果、有 Pending、
/// 点名的服务器为空或在 Pending 时等，再取一次工具重新搜。`tools` 取当前的工具（按名字排序），`search` 在给出的
/// 工具里搜，`text` 是用来找点名的文本。返回新找到的工具名。
pub(crate) async fn retry(
    mcp: &dyn McpConnections,
    before: &[Arc<dyn Tool>],
    text: &str,
    tools: impl Fn() -> Result<Vec<Arc<dyn Tool>>, String>,
    search: impl Fn(&[Arc<dyn Tool>]) -> Vec<String>,
    context: &ToolUseContext,
) -> Result<Vec<String>, ToolError> {
    let failed = |message| ToolError::Failed { message };
    let fresh = tools().map_err(failed)?;
    let added = fresh
        .iter()
        .any(|tool| !before.iter().any(|old| old.name() == tool.name()));
    let view = mcp.view();
    let pending = view.pending();
    if !added && pending.is_empty() {
        return Ok(Vec::new());
    }
    let matches = if added { search(&fresh) } else { Vec::new() };
    let names: Vec<String> = view
        .servers
        .iter()
        .map(|server| server.name.clone())
        .collect();
    let targets = mentioned_servers(text, &names);
    if !matches.is_empty() || pending.is_empty() || !targets_pending(&targets, &pending) {
        return Ok(matches);
    }
    let done = |view: &McpView| settled(view, &targets);
    tokio::select! {
        _ = mcp.wait(WAIT_MS, &done) => {}
        () = context.cancel.cancelled() => return Err(ToolError::Cancelled),
    }
    Ok(search(&tools().map_err(failed)?))
}

/// 没有结果时的补充句：此刻还有 Pending 的服务器时才有。
pub(crate) fn pending_note(mcp: Option<&dyn McpConnections>) -> String {
    let pending = mcp.map(|mcp| mcp.view().pending()).unwrap_or_default();
    if pending.is_empty() {
        return String::new();
    }
    let names = if pending.len() > LISTED {
        format!(
            "{}, \u{2026}and {} more",
            pending[..LISTED].join(", "),
            pending.len() - LISTED
        )
    } else {
        pending.join(", ")
    };
    format!(
        ". Some MCP servers are still connecting: {names}. Their tools will become available shortly \u{2014} try searching again. If you're looking for a capability rather than a specific tool name, try keywords that might match the server's purpose (e.g., 'slack message', 'calendar event'). Once you find a matching tool, call it directly \u{2014} do not stop after searching."
    )
}

#[cfg(test)]
#[path = "tool_search_mcp_tests.rs"]
mod tests;
