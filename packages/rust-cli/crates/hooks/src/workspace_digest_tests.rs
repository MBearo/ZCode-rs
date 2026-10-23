//! 信任 digest 的结构（与 Node 的逐字节对拍在 cli 的 `config::trust` 测试里，config.md §5）。
use serde_json::json;

use super::*;

fn hooks(value: serde_json::Value) -> NodeHooks {
    NodeHooks::from_json(&value).0
}

fn source<'a>(hooks: &'a NodeHooks, order: usize) -> DigestSource<'a> {
    DigestSource {
        relative_path: ".zcode/config.json".to_owned(),
        discovery_order: order,
        kind: ".zcode/config.json",
        explicit: false,
        editable: true,
        hooks,
    }
}

#[test]
fn entries_follow_the_node_event_order_and_gates() {
    let project = hooks(json!({"events": {
        "Stop": [{"hooks": [{"type": "command", "command": "stop"}]}],
        "PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": "a"},
            {"type": "process", "command": "b", "enabled": false}
        ]}],
        "Notification": [{"hooks": [{"type": "command", "command": "cc-only"}]}]
    }}));
    let user = hooks(json!({"enabled": true, "timeoutMs": 1500.4}));
    let root = runtime_root([&user, &project]);
    assert_eq!(
        root,
        RuntimeRoot {
            enabled: true,
            timeout_ms: 1500.0,
            max_output_bytes: 32768.0
        }
    );
    let sources = [source(&project, 2)];
    let bundle = bundle(&sources, &root).unwrap();
    let ids: Vec<_> = bundle
        .entries
        .iter()
        .map(|entry| (entry.review_item_id.as_str(), entry.configured_enabled))
        .collect();
    // Node 的 7 个事件按固定顺序；Node 不认的事件不进 digest。
    assert_eq!(
        ids,
        [
            ("workspace-hook-0-PreToolUse-0-0", true),
            ("workspace-hook-0-PreToolUse-0-1", false),
            ("workspace-hook-0-Stop-0-0", true),
        ]
    );
    assert!(bundle.entries.iter().all(|entry| entry.digest.len() == 64));
    assert_eq!(bundle.bundle_digest.len(), 64);
    // 运行根关闭时三个门之一为假：configuredEnabled 全为假，digest 也随之变化。
    let off = runtime_root([&project]);
    let closed = super::bundle(&sources, &off).unwrap();
    assert!(closed.entries.iter().all(|entry| !entry.configured_enabled));
    assert_ne!(closed.bundle_digest, bundle.bundle_digest);
}

#[test]
fn no_declarations_means_no_bundle() {
    let empty = hooks(json!({"enabled": true}));
    let sources = [source(&empty, 0)];
    assert!(bundle(&sources, &runtime_root([&empty])).is_none());
}
