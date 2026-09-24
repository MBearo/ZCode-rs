//! The Node synthesis loop step: which stored message opens which kind of
//! turn (Node `synthesizeEventsFromMessages` body).
use super::facts::{self, js_string, text};
use super::policy::{self, text_of};
use super::synth::Turns;
use super::synth_models;
use super::synth_parts;
use crate::node_history::Record;
use serde_json::json;

/// One step of the Node synthesis loop: skips `record` or synthesizes the turn
/// it starts, returning the index after it.
pub fn next_turn(t: &mut Turns, record: &Record, index: usize) -> usize {
    let id = js_string(&record.info["id"]);
    if facts::is_provider_context_assistant(record)
        || facts::is_legacy_compact_input(record, t.messages.get(index + 1))
    {
        return index + 1;
    }
    if facts::is_fork_timeline(record) {
        t.fork_notice(record, None);
        return index + 1;
    }
    if let Some(model) = synth_models::model_change(record) {
        t.models.record(&mut t.s, model);
        return index + 1;
    }
    if let Some(launch) = facts::workflow_launch(record) {
        let opened = t.open(record);
        t.models
            .accept(&mut t.s, synth_models::user_selection(record));
        let payload = json!({"turnNumber": t.turn_number, "input": text_of(&record.parts),
            "messageId": id, "executionKind": "controlOnly", "inputSource": "workflow_launch",
            "workflowLaunch": launch});
        return t.run(opened, payload, index + 1, None);
    }
    if !policy::real_user_starter(record) {
        if let Some(wake) = policy::model_only_trigger(record) {
            let opened = t.open(record);
            t.models
                .accept(&mut t.s, synth_models::user_selection(record));
            let background = wake == "background_task";
            let input = if background {
                text_of(&record.parts)
            } else {
                String::new()
            };
            let mut payload = json!({"turnNumber": t.turn_number, "input": input,
                "inputVisibility": "model-only", "inputSource": wake});
            if background && let Some(meta) = facts::background_origin_meta(record) {
                payload["originMeta"] = meta;
            }
            payload["messageId"] = id.clone().into();
            return t.run(opened, payload, index + 1, Some(&id));
        }
        // 会话头部的非真实用户消息（rewind notice、compact summary）后面的 assistant
        // 输出以 preface model-only 轮呈现，否则刷新后整段回复消失。
        if record.info["role"] != "assistant" || !synth_parts::has_synthesizable_content(record) {
            return index + 1;
        }
        let opened = t.open(record);
        t.models
            .accept(&mut t.s, synth_models::assistant_selection(record));
        let payload =
            json!({"turnNumber": t.turn_number, "input": "", "inputVisibility": "model-only"});
        return t.run(opened, payload, index, None);
    }
    let opened = t.open(record);
    let mut payload = json!({"turnNumber": t.turn_number, "input": text_of(&record.parts)});
    if let Some(epilogue) = facts::epilogue_start(record) {
        payload["epilogueStart"] = epilogue;
    }
    payload["messageId"] = id.clone().into();
    if let Some(kind) = facts::execution_kind(record) {
        payload["executionKind"] = kind.into();
    }
    if let Some(input) = text(&record.info["anchor"]["sourceCommandId"]) {
        payload["inputId"] = input.into();
    }
    if let Some(intent) = facts::input_intent(record) {
        payload["intent"] = intent;
    }
    let attachments = facts::attachment_metas(&record.parts);
    if !attachments.is_empty() {
        payload["attachments"] = attachments.into();
    }
    t.models
        .accept(&mut t.s, synth_models::user_selection(record));
    t.run(opened, payload, index + 1, Some(&id))
}
