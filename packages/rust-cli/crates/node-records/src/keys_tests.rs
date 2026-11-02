//! entry 的键与种类的写法（与以前各处拼的逐字相同：Node 与旧库里的记录照旧读得回）。

use super::*;

#[test]
fn keys_and_types_are_spelled_as_before() {
    assert_eq!(
        EntryKey::single("sess_1", PERMISSION_MODE).encode(),
        "sess_1:rust-cli/permission_mode"
    );
    assert_eq!(
        EntryKey::item("sess_1", COMPACT_BOUNDARY, "b1").encode(),
        "sess_1:rust-cli/compact_boundary:b1"
    );
    assert_eq!(named(COMMIT), "rust-cli/commit");
    assert_eq!(named(MODEL_CONNECTION), "rust-cli/model_connection");
}

#[test]
fn an_item_key_parses_back_to_its_id() {
    let key = EntryKey::item("sess_1", "peer_message", "m:2").encode();
    assert_eq!(
        EntryKey::parse(&key, "sess_1", "peer_message").and_then(|key| key.id),
        Some("m:2")
    );
    // 别的会话、别的种类、种类只是前缀相同、每个会话一条的键：都不是。
    assert_eq!(EntryKey::parse(&key, "sess_2", "peer_message"), None);
    assert_eq!(EntryKey::parse(&key, "sess_1", "peer_receipt"), None);
    assert_eq!(EntryKey::parse(&key, "sess_1", "peer"), None);
    let single = EntryKey::single("sess_1", "peer_message").encode();
    assert_eq!(EntryKey::parse(&single, "sess_1", "peer_message"), None);
}
