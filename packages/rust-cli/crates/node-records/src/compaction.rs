//! 压缩边界与摘要的 Node 记录：触发方式的名字与读回（docs/specs/rust-cli/compact.md §3.5、§5）。写入在 node-store 的
//! `compaction`。

use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, Message, PreservedSegment, SystemMessage,
};
use serde_json::Value;

use crate::records::RUST_CLI;

/// `compaction` part 的 `metadata.rustCli` 里记压缩前已加载的工具的键（compact.md §3.5）。
pub const DISCOVERED_KEY: &str = "preCompactDiscoveredTools";

/// trigger → Node 的 `trigger`、`phase`、`compactReason`。
pub fn node_trigger(trigger: CompactTrigger) -> (&'static str, &'static str, &'static str) {
    match trigger {
        CompactTrigger::Manual => ("manual", "standalone_turn", "user_requested"),
        CompactTrigger::Auto => ("auto", "pre_request", "context_limit"),
        CompactTrigger::Reactive => ("reactive", "reactive", "provider_overflow"),
    }
}

fn kernel_trigger(trigger: &str) -> CompactTrigger {
    match trigger {
        "manual" => CompactTrigger::Manual,
        "reactive" => CompactTrigger::Reactive,
        _ => CompactTrigger::Auto,
    }
}

/// 冷读取：摘要消息的 `compactBoundary` → 内核边界（摘要之前）。
pub fn hydrate_boundary(parts: &[Value], summary: &str) -> Option<Message> {
    let payload = parts
        .iter()
        .find(|part| part["type"] == "compaction" && part["compactBoundary"].is_object())
        .map(|part| &part["compactBoundary"])?;
    let preserved = payload["preservedSegment"].as_object().and_then(|segment| {
        let id = |key: &str| segment.get(key)?.as_str().map(MessageId::new);
        Some(PreservedSegment {
            head: id("headMessageId")?,
            anchor: id("anchorMessageId")?,
            tail: id("tailMessageId")?,
        })
    });
    let compaction = parts
        .iter()
        .find(|part| part["type"] == "compaction" && part["compactBoundary"].is_object());
    let discovered_tools = compaction
        .and_then(|part| part["metadata"][RUST_CLI][DISCOVERED_KEY].as_array())
        .map(|names| {
            names
                .iter()
                .filter_map(|name| name.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    Some(Message::System(SystemMessage::CompactBoundary(
        CompactBoundary {
            id: MessageId::new(payload["boundaryId"].as_str()?),
            trigger: kernel_trigger(payload["trigger"].as_str().unwrap_or("")),
            pre_tokens: payload["preCompactTokenCount"].as_u64().unwrap_or(0),
            summary: MessageId::new(payload["summaryMessageIds"][0].as_str().unwrap_or(summary)),
            preserved,
            discovered_tools,
        },
    )))
}
