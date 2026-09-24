//! Node's input intent records from a Rust queue item (which has the V4
//! `QueueItem` shape): `TurnInputIntentMetadata`,
//! `buildPersistedConversationInputIntent` and the V4 admission ledger payload
//! (`admitInputCommand`, `TurnSteerQueued`). Keys in Node's literal order.
use serde_json::{Map, Value, json};

fn copy(out: &mut Map<String, Value>, from: &Value, key: &str, to: &str) {
    if let Some(value) = from.get(key).filter(|v| !v.is_null()) {
        out.insert(to.into(), value.clone());
    }
}

/// Node `TurnInputIntentMetadata` (`inputIntentMetadata`); a queued input
/// also carries its `queuePosition` (steering `{...intent, queuePosition}`).
pub fn turn_intent(item: &Value, queued: bool) -> Value {
    let mut out = Map::new();
    for key in [
        "sourceCommandId",
        "queueItemId",
        "clientId",
        "kind",
        "text",
        "modelSelection",
        "mode",
        "planEnabled",
    ] {
        copy(&mut out, item, key, key);
    }
    out.insert("admissionSeq".into(), item["order"]["admissionSeq"].clone());
    out.insert("admittedAt".into(), item["admittedAt"].clone());
    let delivery = &item["delivery"];
    out.insert("requestedDelivery".into(), delivery["requested"].clone());
    out.insert("admittedDelivery".into(), delivery["admitted"].clone());
    copy(
        &mut out,
        delivery,
        "fallbackReasonCode",
        "fallbackReasonCode",
    );
    copy(&mut out, item, "attachments", "attachmentRefs");
    copy(&mut out, item, "sharedContextRefs", "sharedContextRefs");
    copy(&mut out, item, "provenance", "provenance");
    if queued {
        copy(&mut out, &item["order"], "queuePosition", "queuePosition");
    }
    Value::Object(out)
}

fn steer(fallback: Option<&Value>, guided: bool) -> Value {
    match fallback {
        Some(reason) => json!({"state": "fellBack", "reasonCode": reason}),
        None if guided => json!({"state": "steering"}),
        None => json!({"state": "notRequested"}),
    }
}

/// Node `buildPersistedConversationInputIntent` (`dispatch`: `queued` or
/// `drained`).
pub fn conversation_intent(text: &str, intent: &Value, dispatch: &str) -> Value {
    let fallback = intent.get("fallbackReasonCode").filter(|v| !v.is_null());
    let mut steer = steer(fallback, intent["admittedDelivery"] == "guide");
    if fallback.is_none() && dispatch == "drained" && intent["admittedDelivery"] == "guide" {
        steer = json!({"state": "guided"});
    }
    let mut out = Map::new();
    for key in ["sourceCommandId", "queueItemId", "clientId", "kind"] {
        out.insert(key.into(), intent[key].clone());
    }
    out.insert(
        "text".into(),
        intent.get("text").cloned().unwrap_or_else(|| text.into()),
    );
    out.insert(
        "attachments".into(),
        intent.get("attachmentRefs").cloned().unwrap_or(json!([])),
    );
    for key in ["modelSelection", "mode", "planEnabled", "sharedContextRefs"] {
        copy(&mut out, intent, key, key);
    }
    let mut delivery =
        json!({"requested": intent["requestedDelivery"], "admitted": intent["admittedDelivery"]});
    if let Some(reason) = fallback {
        delivery["fallbackReasonCode"] = reason.clone();
    }
    out.insert("delivery".into(), delivery);
    let mut order = json!({"admissionSeq": intent["admissionSeq"]});
    if let Some(position) = intent.get("queuePosition") {
        order["queuePosition"] = position.clone();
    }
    out.insert("order".into(), order);
    out.insert("steer".into(), steer);
    out.insert("dispatch".into(), json!({"state": dispatch}));
    out.insert("admittedAt".into(), intent["admittedAt"].clone());
    copy(&mut out, intent, "provenance", "provenance");
    Value::Object(out)
}

/// Node V4 `admitInputCommand`: the ledger payload written before the command
/// is acknowledged (`conversationInputIntent` in the zod schema's key order).
pub fn admission_payload(item: &Value, source_command_type: &str) -> Value {
    let delivery = &item["delivery"];
    let fallback = delivery.get("fallbackReasonCode").filter(|v| !v.is_null());
    let attachments = item.get("attachments").cloned().unwrap_or(json!([]));
    let refs = item.get("sharedContextRefs").filter(|v| !v.is_null());
    let mut conversation = Map::new();
    for key in ["sourceCommandId", "queueItemId", "clientId", "kind", "text"] {
        conversation.insert(key.into(), item[key].clone());
    }
    conversation.insert("attachments".into(), attachments.clone());
    if let Some(refs) = refs {
        conversation.insert("sharedContextRefs".into(), refs.clone());
    }
    let mut admitted =
        json!({"requested": delivery["requested"], "admitted": delivery["admitted"]});
    if let Some(reason) = fallback {
        admitted["fallbackReasonCode"] = reason.clone();
    }
    conversation.insert("delivery".into(), admitted);
    conversation.insert(
        "order".into(),
        json!({"admissionSeq": item["order"]["admissionSeq"]}),
    );
    let steer = match fallback {
        Some(reason) => json!({"state": "fellBack", "reasonCode": reason}),
        None if delivery["requested"] == "guide" => json!({"state": "submitting"}),
        None => json!({"state": "notRequested"}),
    };
    conversation.insert("steer".into(), steer);
    conversation.insert("dispatch".into(), json!({"state": "admitted"}));
    conversation.insert("admittedAt".into(), item["admittedAt"].clone());
    let mut intent = Map::new();
    for key in ["sourceCommandId", "queueItemId", "clientId", "kind"] {
        intent.insert(key.into(), item[key].clone());
    }
    intent.insert("admissionSeq".into(), item["order"]["admissionSeq"].clone());
    intent.insert("admittedAt".into(), item["admittedAt"].clone());
    intent.insert("requestedDelivery".into(), delivery["requested"].clone());
    intent.insert("admittedDelivery".into(), delivery["admitted"].clone());
    if let Some(reason) = fallback {
        intent.insert("fallbackReasonCode".into(), reason.clone());
    }
    intent.insert("attachmentRefs".into(), attachments.clone());
    let mut payload = json!({"text": item["text"], "intent": intent,
        "conversationInputIntent": conversation, "attachments": attachments});
    if let Some(refs) = refs {
        payload["intent"]["sharedContextRefs"] = refs.clone();
        payload["sharedContextRefs"] = refs.clone();
    }
    payload["sourceCommandType"] = source_command_type.into();
    payload
}

/// Node `TurnSteerQueued` ledger payload: the queued input's intent.
pub fn queued_payload(item: &Value) -> Value {
    let text = item["text"].as_str().unwrap_or("");
    let intent = turn_intent(item, true);
    let conversation = conversation_intent(text, &intent, "queued");
    json!({"text": text, "intent": intent, "conversationInputIntent": conversation})
}

/// Node `persistUserPrompt` metadata for an input with its intent.
pub fn prompt_metadata(text: &str, intent: &Value, presentation: Option<&str>) -> Value {
    let mut metadata = Map::new();
    if let Some(presentation) = presentation {
        metadata.insert("inputPresentation".into(), presentation.into());
    }
    metadata.insert("inputIntent".into(), intent.clone());
    metadata.insert(
        "conversationInputIntent".into(),
        conversation_intent(text, intent, "drained"),
    );
    if let Some(client) = intent.get("clientId").filter(|c| !c.is_null()) {
        metadata.insert("inputClientId".into(), client.clone());
    }
    Value::Object(metadata)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(value: &Value) -> Vec<&str> {
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    fn item() -> Value {
        json!({"sourceCommandId": "c1", "queueItemId": "queue_c1", "clientId": "cli",
            "kind": "sendText", "text": "hi", "attachments": [],
            "modelSelection": {"providerId": "p", "modelId": "m"}, "mode": "build",
            "planEnabled": false, "delivery": {"requested": "guide", "admitted": "guide"},
            "order": {"admissionSeq": 3, "queuePosition": 1}, "steer": {"state": "steering"},
            "dispatch": {"state": "queued"}, "admittedAt": 9})
    }

    #[test]
    fn records_follow_nodes_key_order() {
        let admission = admission_payload(&item(), "sendText");
        assert_eq!(
            keys(&admission),
            [
                "text",
                "intent",
                "conversationInputIntent",
                "attachments",
                "sourceCommandType"
            ]
        );
        assert_eq!(
            keys(&admission["conversationInputIntent"]),
            [
                "sourceCommandId",
                "queueItemId",
                "clientId",
                "kind",
                "text",
                "attachments",
                "delivery",
                "order",
                "steer",
                "dispatch",
                "admittedAt"
            ]
        );
        assert_eq!(
            admission["conversationInputIntent"]["steer"]["state"],
            "submitting"
        );
        assert_eq!(
            admission["conversationInputIntent"]["dispatch"]["state"],
            "admitted"
        );
        let queued = queued_payload(&item());
        assert_eq!(keys(&queued["intent"]).last(), Some(&"queuePosition"));
        let conversation = &queued["conversationInputIntent"];
        assert_eq!(
            conversation["order"],
            json!({"admissionSeq": 3, "queuePosition": 1})
        );
        assert_eq!(conversation["steer"]["state"], "steering");
        let intent = turn_intent(&item(), false);
        assert!(intent.get("queuePosition").is_none());
        let metadata = prompt_metadata("hi", &intent, Some("user_steer"));
        assert_eq!(
            keys(&metadata),
            [
                "inputPresentation",
                "inputIntent",
                "conversationInputIntent",
                "inputClientId"
            ]
        );
        // 引导输入被消费后是 guided（Node buildPersistedConversationInputIntent）。
        assert_eq!(
            metadata["conversationInputIntent"]["steer"]["state"],
            "guided"
        );
        assert_eq!(
            metadata["conversationInputIntent"]["dispatch"]["state"],
            "drained"
        );
    }
}
