//! `mcp_instructions_delta`（docs/specs/rust-cli/ext.md §3.6）：已连接的 MCP 服务器给出的 instructions，
//! 只宣布新增、内容变化与断开的服务器。来源是 [`ListingSource::mcp_instructions`]（ext 提供）。
//!
//! [`ListingSource::mcp_instructions`]: rust_cli_kernel_api::listing::ListingSource::mcp_instructions

use std::collections::BTreeMap;

use rust_cli_kernel_api::listing::ListingEntry;
use rust_cli_kernel_api::message::Message;
use rust_cli_prompts::texts::{self, render};
use serde_json::{Value, json};

use crate::render::attachments;

pub(crate) const SOURCE: &str = "mcp_instructions_delta";

fn strings(data: &Value, key: &str) -> Vec<String> {
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

/// 已宣布、还没撤回的服务器与段落（压缩边界之后的历史）。
fn announced(history: &[Message]) -> BTreeMap<String, String> {
    let mut known = BTreeMap::new();
    for attachment in attachments(history, SOURCE) {
        let names = strings(&attachment.data, "addedNames");
        let blocks = strings(&attachment.data, "addedBlocks");
        for (name, block) in names.into_iter().zip(blocks) {
            known.insert(name, block);
        }
        for name in strings(&attachment.data, "removedNames") {
            known.remove(&name);
        }
    }
    known
}

/// 这一步的增量；`current` 按服务器名排好（ext 给出）。
pub(crate) fn announce(history: &[Message], current: &[ListingEntry]) -> Option<(String, Value)> {
    let known = announced(history);
    let added: Vec<&ListingEntry> = current
        .iter()
        .filter(|entry| known.get(&entry.name) != Some(&entry.line))
        .collect();
    let removed: Vec<&String> = known
        .keys()
        .filter(|name| !current.iter().any(|entry| &entry.name == *name))
        .collect();
    let mut sections = Vec::new();
    if !added.is_empty() {
        let blocks: Vec<&str> = added.iter().map(|entry| entry.line.as_str()).collect();
        sections.push(render(
            texts::mcp_instructions(),
            &[("blocks", &blocks.join("\n\n"))],
        ));
    }
    if !removed.is_empty() {
        let names: Vec<&str> = removed.iter().map(|name| name.as_str()).collect();
        sections.push(render(
            texts::mcp_instructions_removed(),
            &[("names", &names.join("\n"))],
        ));
        sections.push(texts::ambient().to_owned());
    }
    if sections.is_empty() {
        return None;
    }
    let data = json!({
        "addedNames": added.iter().map(|entry| entry.name.as_str()).collect::<Vec<_>>(),
        "addedBlocks": added.iter().map(|entry| entry.line.as_str()).collect::<Vec<_>>(),
        "removedNames": removed,
    });
    Some((sections.join("\n\n"), data))
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
