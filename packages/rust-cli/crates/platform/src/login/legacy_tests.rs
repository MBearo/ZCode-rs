//! 旧 CLI 用户文件的导入判定（Node `importLegacyCliPersonalProviderConfig` 的筛选）。
use rust_cli_base::models;
use serde_json::json;

use super::pending;

#[test]
fn custom_providers_and_defaults_are_imported_by_node() {
    let custom =
        json!({"provider": {"my-proxy": {"kind": "anthropic", "options": {"apiKey": "k"}}}});
    assert!(pending(&custom));
    assert!(pending(&json!({"model": "my-proxy/m1"})));
    assert!(pending(
        &json!({"model": {"main": format!("zai/{}", models::GLM_5_3_ACCOUNT)}})
    ));
    let builtin_key =
        json!({"provider": {"builtin:zai": {"kind": "openai", "options": {"apiKey": " k "}}}});
    assert!(pending(&builtin_key));
}

#[test]
fn nothing_to_import_lets_rust_cli_write() {
    assert!(!pending(&json!({"ui": {"locale": "en-US"}})));
    assert!(!pending(&json!({"model": "builtin:zapi/glm"})));
    assert!(!pending(&json!({"model": "no-slash"})));
    let skipped = json!({"provider": {
        "builtin:zai": {"kind": "openai", "options": {"apiKey": " "}},
        "builtin:other": {"kind": "openai"},
        "account:x": {"kind": "openai"},
        "desktop": {"kind": "openai", "source": "builtin"},
        " ": {"kind": "openai"},
    }});
    assert!(!pending(&skipped));
    // Node 整份放弃导入：连默认模型也不导入。
    let unsupported = json!({"model": "p/m", "provider": {
        "p": {"kind": "openai", "options": {"apiKeyRequired": false}},
    }});
    assert!(!pending(&unsupported));
}
