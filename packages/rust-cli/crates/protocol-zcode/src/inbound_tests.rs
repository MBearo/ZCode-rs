use serde_json::json;

use super::*;

/// 随 crate 带的 schema 全部能编译（zod 用了不认识的关键字时这里失败，重新生成后要补校验器）。
#[test]
fn the_shipped_schema_compiles() {
    let inbound = Inbound::parse(INBOUND).unwrap();
    for method in [
        "v4/command",
        "v4/conversation/subscribe",
        "runtime/capabilities",
        "session/read",
    ] {
        assert!(inbound.methods.contains_key(method), "{method}");
    }
    for command in [
        "createSession",
        "sendText",
        "stop",
        "resolveInteraction",
        "setFollowupMode",
    ] {
        assert!(inbound.commands.contains_key(command), "{command}");
    }
    assert!(
        inbound
            .requires_base_revision
            .contains(&"setFollowupMode".to_owned())
    );
}

#[test]
fn method_params_follow_the_app_schema() {
    let inbound = Inbound::get();
    let workspace = json!({"workspacePath": "/w", "workspaceKey": "/w"});
    assert!(
        inbound
            .method(
                "workspace/updateOffPeakToolPolicy",
                &json!({"workspace": workspace, "enabled": true})
            )
            .is_ok()
    );
    let error = inbound
        .method(
            "workspace/updateOffPeakToolPolicy",
            &json!({"workspace": workspace, "enabled": true, "extra": 1}),
        )
        .unwrap_err();
    assert_eq!(error.code, -32602);
    // 省略 params 按 {} 校验。
    assert!(inbound.method("runtime/capabilities", &Value::Null).is_ok());
    assert!(inbound.method("not/registered", &json!(1)).is_ok());
}

#[test]
fn commands_check_the_payload_and_the_cross_field_rules() {
    let inbound = Inbound::get();
    let envelope = |kind: &str, payload: Value| {
        json!({"commandId": "c1", "clientId": "k", "sessionId": "s", "type": kind,
            "payload": payload, "issuedAt": 1})
    };
    assert!(
        inbound
            .command(&envelope("sendText", json!({"text": "hi"})))
            .is_ok()
    );
    assert!(
        inbound
            .command(&envelope("sendText", json!({"text": 1})))
            .is_err()
    );
    assert!(
        inbound
            .command(&envelope(
                "sendText",
                json!({"text": "hi", "modelExecution": {"kind": "standard"}})
            ))
            .is_err()
    );
    // CAS 命令要 baseRevision。
    assert!(
        inbound
            .command(&envelope("setFollowupMode", json!({"mode": "queue"})))
            .is_err()
    );
    let mut cas = envelope("setFollowupMode", json!({"mode": "queue"}));
    cas["baseRevision"] = json!(0);
    assert!(inbound.command(&cas).is_ok());
    assert!(
        inbound
            .command(&envelope("noSuchCommand", json!({})))
            .is_err()
    );
}

#[test]
fn reverse_request_replies_follow_the_app_schema() {
    let inbound = Inbound::get();
    let method = "interaction/requestProviderRuntimeHeaders";
    assert!(
        inbound
            .reply(
                method,
                &json!({"headersApplied": true, "requestAuth": {"apiKey": "k"}})
            )
            .is_ok()
    );
    assert!(
        inbound
            .reply(method, &json!({"headersApplied": true}))
            .is_err()
    );
    assert!(
        inbound
            .reply(
                method,
                &json!({"headersApplied": false, "errorMessage": "no"})
            )
            .is_ok()
    );
}
