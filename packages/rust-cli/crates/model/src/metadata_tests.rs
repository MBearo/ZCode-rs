//! `metadata.user_id` 与 Node 逐字节相同（golden 由 `scripts/rust-cli/metadata-golden.mjs` 用 Node 的
//! `createAnthropicRequestMetadataUserId` 生成，model.md §3.3）。

use serde_json::Value;

use super::user_id;

#[test]
fn the_user_id_text_matches_node() {
    let golden: Value =
        serde_json::from_str(include_str!("../tests/fixtures/metadata-golden.json")).unwrap();
    let cases = golden.as_array().unwrap();
    assert!(cases.len() >= 20);
    for case in cases {
        let device = case["device"].as_str().unwrap();
        let session = case["session"].as_str();
        assert_eq!(
            user_id(device, session),
            case["userId"].as_str().unwrap(),
            "{case}"
        );
    }
}
