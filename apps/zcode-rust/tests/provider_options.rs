use serde_json::json;
use zcode_rust::domain::option_map::{evaluate, merge_patch, validate_patches};

#[test]
fn option_maps_preserve_types_short_circuit_and_patch_deletion() {
    let patch = evaluate("reasoningLevel == 'none' ? {'thinking': null} : {'thinking': {'type': 'enabled', 'budget_tokens': 4096}}", "reasoningLevel", &json!("none")).unwrap();
    let mut body = json!({"model":"fixture","thinking":{"type":"enabled"}});
    merge_patch(&mut body, &patch);
    assert_eq!(body, json!({"model":"fixture"}));
    assert_eq!(
        evaluate(
            "{'limit': maxOutputTokens / 2 + 1, 'safe': true || 1 / 0 > 1}",
            "maxOutputTokens",
            &json!(100)
        )
        .unwrap(),
        json!({"limit":51,"safe":true})
    );
    assert!(evaluate("{'x': reasoningLevel.foo}", "reasoningLevel", &json!("low")).is_err());
    assert!(evaluate("{'x': 1, 'x': 2}", "reasoningLevel", &json!("low")).is_err());
    assert!(evaluate("{'x': 1 / 0}", "reasoningLevel", &json!("low")).is_err());
    assert!(validate_patches(&[json!({"a":{"b":1}}), json!({"a":null})]).is_err());
}
