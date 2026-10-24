//! 清单类 attachment：延迟工具、agent 类型、skill（`deferred_tools_delta`、
//! `agent_listing_delta`、`skill_listing`）。
//!
//! 已宣布的集合从压缩边界之后的 attachment 的 `data` 累计（加上 added、去掉 removed；skill 看全部历史），
//! 与这一步的清单比较，只发增量。清单内容由 [`rust_cli_kernel_api::listing::ListingSource`] 提供。

use std::collections::BTreeSet;

use rust_cli_kernel_api::listing::ListingEntry;
// 连接中的服务器名最多列几个（30）：只定义在 kernel-api（F58：以前这里与 tools 的 ToolSearch 各写一份）。
use rust_cli_kernel_api::mcp_state::PENDING_LISTED;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::orchestration::ContextRequest;
use rust_cli_kernel_api::tool_search::{TOOL_SEARCH, compare_names};
use rust_cli_prompts::texts::{self, render};
use serde_json::{Value, json};

use crate::render::attachments;

pub(crate) const DEFERRED_TOOLS: &str = "deferred_tools_delta";
pub(crate) const AGENTS: &str = "agent_listing_delta";
pub(crate) const SKILLS: &str = "skill_listing";

fn names(data: &Value, key: &str) -> Vec<String> {
    data.get(key)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// 已经宣布过、还没撤回的名字；第二项表示历史里有没有这类 attachment。
fn announced(history: &[Message], source: &str, added: &str) -> (BTreeSet<String>, bool) {
    let mut set = BTreeSet::new();
    let mut seen = false;
    for attachment in attachments(history, source) {
        seen = true;
        set.extend(names(&attachment.data, added));
        for name in names(&attachment.data, "removedNames")
            .into_iter()
            .chain(names(&attachment.data, "removedTypes"))
        {
            set.remove(&name);
        }
    }
    (set, seen)
}

fn lines(entries: &[&ListingEntry]) -> String {
    entries
        .iter()
        .map(|entry| entry.line.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn removed(known: &BTreeSet<String>, current: &[ListingEntry]) -> Vec<String> {
    known
        .iter()
        .filter(|name| !current.iter().any(|entry| &entry.name == *name))
        .cloned()
        .collect()
}

/// 延迟工具清单里要宣布的连接中 MCP 服务器（context.md §4.2，修复线 F36）：这一步的工具快照有 MCP 状态、工具里有
/// ToolSearch（乐观判断，tools-more.md §3.2.1）时是 Pending 的服务器名（排序）；否则 None（不带这一项）。修改（工作线
/// F89）：不再看模型（F84 时禁用表里的模型不带）。
pub(crate) fn pending_servers(request: &ContextRequest) -> Option<Vec<String>> {
    let view = request.mcp.as_ref()?;
    let tool_search = request.tool_names.iter().any(|name| name == TOOL_SEARCH);
    tool_search.then(|| view.pending())
}

/// 最近一次宣布的连接中服务器（最后一条带 `pendingMcpServers` 数组的 `deferred_tools_delta`）。
fn announced_pending(history: &[Message]) -> Vec<String> {
    attachments(history, DEFERRED_TOOLS)
        .rev()
        .find(|attachment| {
            attachment
                .data
                .get(PENDING_KEY)
                .is_some_and(Value::is_array)
        })
        .map(|attachment| names(&attachment.data, PENDING_KEY))
        .unwrap_or_default()
}

const PENDING_KEY: &str = "pendingMcpServers";

/// 连接中服务器的那一段：多于 30 个时前 30 个用 `, ` 连接，接 `, …and <n> more`。
fn pending_section(pending: &[String]) -> String {
    let lines = if pending.len() > PENDING_LISTED {
        format!(
            "{}, \u{2026}and {} more",
            pending[..PENDING_LISTED].join(", "),
            pending.len() - PENDING_LISTED
        )
    } else {
        pending.join("\n")
    };
    render(texts::deferred_tools_pending(), &[("lines", &lines)])
}

/// 延迟工具里列过行的名字：每条的 `addedNames` 里不在本条 `readdedNames` 的；撤回不去掉（context.md §4.3）。
/// 旧记录没有 `readdedNames`，全部算列过。
fn listed(history: &[Message]) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for attachment in attachments(history, DEFERRED_TOOLS) {
        let readded = names(&attachment.data, READDED_KEY);
        set.extend(
            names(&attachment.data, "addedNames")
                .into_iter()
                .filter(|name| !readded.contains(name)),
        );
    }
    set
}

const READDED_KEY: &str = "readdedNames";
/// 多于这么多个撤回的 MCP 工具时按服务器归组。
const MCP_LISTED: usize = 30;

fn is_mcp(name: &str) -> bool {
    name.starts_with("mcp__")
}

/// 服务器摘要：`mcp__<server>__x` 归成 `mcp__<server>__*`，同组多于一个时写个数，按名字排序。
fn servers(names: &[&String]) -> String {
    let mut groups: Vec<(String, usize)> = Vec::new();
    for name in names {
        let key = if is_mcp(name) {
            let mut parts = name.splitn(3, "__");
            let (first, second) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            format!("{first}__{second}__*")
        } else {
            (*name).clone()
        };
        match groups.iter_mut().find(|(group, _)| *group == key) {
            Some((_, count)) => *count += 1,
            None => groups.push((key, 1)),
        }
    }
    groups.sort_by(|left, right| compare_names(&left.0, &right.0));
    groups
        .into_iter()
        .map(|(key, count)| {
            if count > 1 {
                format!("{key} ({count})")
            } else {
                key
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `<n> deferred tool is` / `<n> deferred tools are` 的后半（按个数选单复数）。
fn verb(count: usize) -> &'static str {
    if count == 1 { " is" } else { "s are" }
}

/// 重新可用的工具（`readdedNames` 的两段）：MCP 的按服务器归组，其余逐个列名。
fn readded_sections(readded: &[String]) -> Vec<String> {
    let (mcp, other): (Vec<&String>, Vec<&String>) = readded.iter().partition(|name| is_mcp(name));
    let mut sections = Vec::new();
    if !mcp.is_empty() {
        let count = mcp.len().to_string();
        sections.push(render(
            texts::deferred_tools_readded_mcp(),
            &[
                ("count", &count),
                ("verb", verb(mcp.len())),
                ("servers", &servers(&mcp)),
            ],
        ));
    }
    if !other.is_empty() {
        let count = other.len().to_string();
        let listed: Vec<&str> = other.iter().map(|name| name.as_str()).collect();
        sections.push(render(
            texts::deferred_tools_readded(),
            &[
                ("count", &count),
                ("verb", verb(other.len())),
                ("names", &listed.join(", ")),
            ],
        ));
    }
    sections
}

/// 撤回的工具：MCP 的用“服务器断开”的模板（多于 30 个时按服务器归组），其余用原来的模板。
fn removed_sections(removed: &[String]) -> Vec<String> {
    let (mcp, other): (Vec<&String>, Vec<&String>) = removed.iter().partition(|name| is_mcp(name));
    let mut sections = Vec::new();
    if mcp.len() > MCP_LISTED {
        let count = mcp.len().to_string();
        sections.push(render(
            texts::deferred_tools_removed_mcp_many(),
            &[("count", &count), ("servers", &servers(&mcp))],
        ));
    } else if !mcp.is_empty() {
        let lines: Vec<&str> = mcp.iter().map(|name| name.as_str()).collect();
        sections.push(render(
            texts::deferred_tools_removed_mcp(),
            &[("lines", &lines.join("\n"))],
        ));
    }
    if !other.is_empty() {
        let lines: Vec<&str> = other.iter().map(|name| name.as_str()).collect();
        sections.push(render(
            texts::deferred_tools_removed(),
            &[("lines", &lines.join("\n"))],
        ));
    }
    sections
}

/// 延迟工具（context.md §4.3）：没列过的按清单顺序列行（经 ToolSearch 加载）；列过、撤回后又回来的只说重新可用；
/// 不再可用的按 MCP 与其余分开说明，附环境说明。`pending` 是这一步连接中的 MCP 服务器（[`pending_servers`]）：
/// 与上次宣布的不同也算变化；不为空时排在最后加一段（修复线 F36）。
///
/// 修 bug（F82）：以前撤回只有一个模板（不说服务器断开），重连后的工具按新增再列一遍行。
pub(crate) fn deferred_tools(
    history: &[Message],
    current: &[ListingEntry],
    pending: Option<&[String]>,
) -> Option<(String, Value)> {
    let (known, _) = announced(history, DEFERRED_TOOLS, "addedNames");
    let listed = listed(history);
    let added: Vec<&ListingEntry> = current
        .iter()
        .filter(|entry| !known.contains(&entry.name))
        .collect();
    let lines: Vec<&ListingEntry> = current
        .iter()
        .filter(|entry| !listed.contains(&entry.name))
        .collect();
    let mut readded: Vec<String> = added
        .iter()
        .filter(|entry| listed.contains(&entry.name))
        .map(|entry| entry.name.clone())
        .collect();
    readded.sort();
    let removed = removed(&known, current);
    let pending_changed = pending.is_some_and(|now| now != announced_pending(history).as_slice());
    if added.is_empty() && lines.is_empty() && removed.is_empty() && !pending_changed {
        return None;
    }
    let mut sections = Vec::new();
    if !lines.is_empty() {
        sections.push(render(
            texts::deferred_tools(),
            &[("lines", &self::lines(&lines))],
        ));
    }
    sections.extend(readded_sections(&readded));
    sections.extend(removed_sections(&removed));
    if !removed.is_empty() {
        sections.push(texts::ambient().to_owned());
    }
    if let Some(now) = pending.filter(|now| !now.is_empty()) {
        sections.push(pending_section(now));
    }
    let mut added_names: Vec<&str> = added.iter().map(|entry| entry.name.as_str()).collect();
    for entry in &lines {
        if !added_names.contains(&entry.name.as_str()) {
            added_names.push(entry.name.as_str());
        }
    }
    let mut data = json!({
        "addedNames": added_names,
        "addedLines": lines.iter().map(|entry| entry.line.as_str()).collect::<Vec<_>>(),
        "removedNames": removed,
    });
    if !readded.is_empty() {
        data[READDED_KEY] = json!(readded);
    }
    // 有 MCP 客户端表时总带这一项（可以是空数组），下一步据此判断有没有变化。
    if let Some(now) = pending {
        data[PENDING_KEY] = json!(now);
    }
    // 只有连接中的服务器变成空时文字为空：照常落库，发给模型时跳过（渲染为空，query.md §3）。
    Some((sections.join("\n\n"), data))
}

/// agent 类型：按类型名排序（按不分大小写的字典序）。
pub(crate) fn agents(history: &[Message], current: &[ListingEntry]) -> Option<(String, Value)> {
    let (known, seen) = announced(history, AGENTS, "addedTypes");
    let mut added: Vec<&ListingEntry> = current
        .iter()
        .filter(|entry| !known.contains(&entry.name))
        .collect();
    added.sort_by_key(|entry| (entry.name.to_lowercase(), entry.name.clone()));
    let removed = removed(&known, current);
    let initial = !seen;
    let mut sections = Vec::new();
    if !added.is_empty() {
        let heading = if initial {
            texts::agents()
        } else {
            texts::agents_new()
        };
        sections.push(render(heading, &[("lines", &lines(&added))]));
    }
    if !removed.is_empty() {
        let listed: Vec<String> = removed.iter().map(|name| format!("- {name}")).collect();
        sections.push(render(
            texts::agents_removed(),
            &[("lines", &listed.join("\n"))],
        ));
        sections.push(texts::ambient().to_owned());
    }
    if !added.is_empty() && initial {
        sections.push(texts::agents_concurrency().to_owned());
    }
    if sections.is_empty() {
        return None;
    }
    let data = json!({
        "addedTypes": added.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
        "addedLines": added.iter().map(|entry| entry.line.as_str()).collect::<Vec<_>>(),
        "removedTypes": removed,
        "isInitial": initial,
    });
    Some((sections.join("\n\n"), data))
}

/// skill：只宣布新出现的（不宣布移除）。`history` 是会话的全部历史，不切到压缩边界之后。
///
/// 缺陷修复（L25）：原来从边界之后推导，压缩后整份 skill 清单重发一次；已宣布的集合
/// 是会话级状态，压缩不清。这份状态的唯一来源是历史里的 `skill_listing`，这里是唯一的推导处。
pub(crate) fn skills(history: &[Message], current: &[ListingEntry]) -> Option<(String, Value)> {
    let (known, seen) = announced(history, SKILLS, "names");
    let added: Vec<&ListingEntry> = current
        .iter()
        .filter(|entry| !known.contains(&entry.name))
        .collect();
    if added.is_empty() {
        return None;
    }
    let data = json!({
        "names": added.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
        "isInitial": !seen,
    });
    Some((render(texts::skills(), &[("lines", &lines(&added))]), data))
}

#[cfg(test)]
#[path = "listings_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "listings_reconnect_tests.rs"]
mod reconnect_tests;
