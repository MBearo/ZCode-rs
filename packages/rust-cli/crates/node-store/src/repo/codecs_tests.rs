// 复制自 apps/zcode-cli-rust/crates/state/src/node/codecs.rs 的测试（feat/rs 79b6691c55）。
use super::*;

#[test]
fn model_selections_are_strict_and_trimmed() {
    let ok = json!({"providerId":" p ","modelId":"m","options":{"reasoningLevel":" high "}});
    assert_eq!(
        parse_model_selection(&ok),
        Some(json!({"providerId":"p","modelId":"m","options":{"reasoningLevel":"high"}}))
    );
    assert_eq!(
        parse_model_selection(&json!({"providerId":"p","modelId":"m","x":1})),
        None
    );
    assert_eq!(
        parse_model_selection(&json!({"providerId":"p","modelId":" "})),
        None
    );
    assert_eq!(
        parse_model_selection(&json!({"providerId":"p","modelId":"m","options":{}})),
        Some(json!({"providerId":"p","modelId":"m","options":{}}))
    );
    assert_eq!(legacy_user_model(None), json!({}));
    assert_eq!(
        legacy_user_model(Some(
            &json!({"providerId":"p","modelId":"m","options":{"reasoningLevel":"max"}})
        )),
        json!({"providerID":"p","modelID":"m","variant":"max"})
    );
}

#[test]
fn decoding_drops_legacy_members_like_node() {
    let user = r#"{"role":"user","time":{"created":1},"modelSelection":{"providerId":"p","modelId":"m"},"agent":"a","model":{"providerID":"p"}}"#;
    let decoded = decode_message(user, "msg_1", "sess_1").unwrap();
    let keys: Vec<&String> = decoded.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["role", "time", "agent", "modelSelection", "id", "sessionID"]
    );
    let assistant = r#"{"role":"assistant","providerID":"p","modelId":"m","variant":"x"}"#;
    assert_eq!(
        decode_message(assistant, "a", "s").unwrap(),
        json!({"role":"assistant","modelId":"m","id":"a","sessionID":"s"})
    );
    let change = r#"{"timelineType":"model_change","type":"timeline","toModel":{"providerID":"p"},"fromModelSelection":{"providerId":"p","modelId":"m","label":"M"},"toModelSelection":{"providerId":"q","modelId":"n","bad":1}}"#;
    assert_eq!(
        decode_part(change, "p1", "s", "m1").unwrap(),
        json!({"timelineType":"model_change","type":"timeline","fromModel":{"providerId":"p","modelId":"m","label":"M"},"id":"p1","sessionID":"s","messageID":"m1"})
    );
    assert_eq!(
        decode_entry_data(
            MODEL_SELECTION_ENTRY,
            r#"{"modelSelection":null,"providerId":"old"}"#
        )
        .unwrap(),
        Value::Null
    );
    assert_eq!(
        part_created_at(&json!({"type":"tool","state":{"status":"pending"}}), 7),
        7
    );
    assert_eq!(
        part_created_at(
            &json!({"type":"tool","state":{"status":"running","time":{"start":3}}}),
            7
        ),
        3
    );
}
