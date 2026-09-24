use super::*;
use crate::domain::delivery;
use serde_json::json;
use std::time::{Duration, Instant};

const TOPIC: &str = "conversation/s";

fn frames(lines: &[String]) -> Vec<Value> {
    lines
        .iter()
        .map(|l| {
            let params = serde_json::from_str::<Value>(l).unwrap()["params"].clone();
            let mut frame = params["frame"].clone();
            frame["deliveryKind"] = params["deliveryKind"].clone();
            frame
        })
        .collect()
}
fn snapshot(seq: u64) -> Value {
    json!({"epoch":"e","seq":seq,"mode":"snapshot","snapshot":{"seq":seq}})
}
fn text(append: &str) -> (Value, usize) {
    let delta = json!({"op":"row.delta","rowId":1,"path":"text","append":append});
    let size = delivery::json_bytes(&delta);
    (delta, size)
}
fn input(append: &str) -> (Value, usize) {
    let delta = json!({"op":"row.delta","rowId":2,"path":"inputText","append":append});
    let size = delivery::json_bytes(&delta);
    (delta, size)
}
fn range(frame: &Value) -> (u64, u64) {
    (
        frame["fromSeq"].as_u64().unwrap(),
        frame["toSeq"].as_u64().unwrap(),
    )
}
fn subscribe(d: &mut Delivery, connection: &str, profile: Profile, state: &Value) -> Subscribed {
    d.subscribe((TOPIC, connection), profile, state).unwrap()
}

#[test]
fn deltas_are_buffered_until_the_window_ends_and_coalesced() {
    let mut d = Delivery::default();
    let t0 = Instant::now();
    let sub = subscribe(&mut d, "c", Profile::Continuous, &snapshot(5));
    assert_eq!(sub.mode, "snapshot");
    assert_eq!(frames(&sub.lines)[0]["deliveryKind"], "initial");
    // 快照之前产生的增量已包含在快照内。
    d.deltas(TOPIC, (3, 5), &[text("old")], t0);
    d.deltas(TOPIC, (5, 6), &[text("a")], t0);
    d.deltas(TOPIC, (6, 7), &[text("b")], t0 + Duration::from_millis(10));
    assert!(d.flush_due(t0 + Duration::from_millis(29)).lines.is_empty());
    assert_eq!(d.next_due(), Some(t0 + Duration::from_millis(30)));
    let sent = frames(&d.flush_due(t0 + Duration::from_millis(30)).lines);
    assert_eq!(range(&sent[0]), (5, 7));
    assert_eq!(sent[0]["deliveryKind"], "online");
    assert_eq!(sent[0]["payload"]["deltas"], json!([text("ab").0]));
    assert!(d.flush_due(t0 + Duration::from_secs(1)).lines.is_empty());
}

#[test]
fn replayable_subscribers_keep_seq_continuity_over_filtered_deltas() {
    let mut d = Delivery::default();
    let t0 = Instant::now();
    subscribe(&mut d, "m", Profile::Replayable, &snapshot(0));
    d.deltas(TOPIC, (0, 1), &[input("x")], t0);
    assert!(
        d.flush_due(t0 + Duration::from_millis(149))
            .lines
            .is_empty()
    );
    let sent = frames(&d.flush_due(t0 + Duration::from_millis(150)).lines);
    assert_eq!(range(&sent[0]), (0, 1));
    assert_eq!(sent[0]["payload"]["deltas"], json!([]));
}

#[test]
fn gaps_recover_by_snapshot_and_replies_cover_deltas_in_flight() {
    let mut d = Delivery::default();
    let t0 = Instant::now();
    let sub = subscribe(&mut d, "c", Profile::Continuous, &snapshot(5));
    d.deltas(TOPIC, (9, 10), &[text("x")], t0);
    let out = d.flush_due(t0 + Duration::from_millis(30));
    assert!(out.lines.is_empty());
    assert_eq!(out.recover, vec![(sub.id.clone(), TOPIC.to_owned())]);
    // 回复在途：期间的增量都已含在回复中。
    d.deltas(TOPIC, (10, 11), &[text("y")], t0);
    assert!(d.flush_due(t0 + Duration::from_secs(1)).recover.is_empty());
    let lines = d.recovered(&sub.id, "online", &snapshot(11)).unwrap();
    assert_eq!(range(&frames(&lines)[0]), (0, 11));
    d.deltas(TOPIC, (11, 12), &[text("z")], t0);
    let sent = frames(&d.flush_due(t0 + Duration::from_secs(2)).lines);
    assert_eq!(range(&sent[0]), (11, 12));
}

#[test]
fn saturated_connections_buffer_until_drained_or_overflow() {
    let mut d = Delivery::default();
    let t0 = Instant::now();
    let desktop = subscribe(&mut d, "desktop", Profile::Continuous, &snapshot(0));
    subscribe(&mut d, "mobile", Profile::Replayable, &snapshot(0));
    d.pause("mobile");
    d.deltas(TOPIC, (0, 1), &[text("a")], t0);
    let later = t0 + Duration::from_secs(1);
    let sent = frames(&d.flush_due(later).lines);
    assert_eq!(sent.len(), 1, "only the desktop flushes");
    assert_eq!(sent[0]["subscriptionId"], desktop.id.as_str());
    d.drain("mobile", later);
    let sent = frames(&d.flush_due(later).lines);
    assert_eq!(range(&sent[0]), (0, 1), "drained sends the buffered deltas");
    assert_eq!(sent[0]["payload"]["kind"], "deltas");

    d.pause("mobile");
    for i in 1..=delivery::MAX_OPS as u64 + 1 {
        let delta = json!({"op":"row.upserted","row":{"rowId":i}});
        let size = delivery::json_bytes(&delta);
        d.deltas(TOPIC, (i, i + 1), &[(delta, size)], later);
    }
    d.drain("mobile", later);
    let out = d.flush_due(later);
    assert_eq!(out.recover.len(), 1, "overflow recovers by snapshot");
    assert!(out.lines.is_empty(), "the desktop window has not ended");
}

#[test]
fn resume_replays_filtered_deltas_or_sends_nothing_when_aligned() {
    let mut d = Delivery::default();
    let aligned = json!({"epoch":"e","seq":4,"mode":"resume","from":4,"deltas":[]});
    let sub = subscribe(&mut d, "m", Profile::Replayable, &aligned);
    assert_eq!((sub.mode, sub.lines.len()), ("resume", 0));
    let behind = json!({"epoch":"e","seq":6,"mode":"resume","from":4,
        "deltas":[text("a").0, input("i").0, text("b").0]});
    let sub = subscribe(&mut d, "m", Profile::Replayable, &behind);
    assert!(sub.replaced);
    let sent = frames(&sub.lines);
    assert_eq!(range(&sent[0]), (4, 6));
    assert_eq!(sent[0]["payload"]["deltas"], json!([text("ab").0]));
    // resync 已对齐时也发 (N,N] 空帧收口客户端恢复。
    d.resync_started(&sub.id);
    let aligned = json!({"epoch":"e","seq":6,"mode":"resume","from":6,"deltas":[]});
    let sent = frames(&d.recovered(&sub.id, "recovery", &aligned).unwrap());
    assert_eq!(range(&sent[0]), (6, 6));
    assert_eq!(sent[0]["deliveryKind"], "recovery");
}

#[test]
fn index_topics_flush_without_a_window_and_connections_release_topics() {
    let mut d = Delivery::default();
    let t0 = Instant::now();
    d.subscribe(("sessions-index/w", "c"), Profile::Continuous, &snapshot(0))
        .unwrap();
    let delta = json!({"op":"session.removed","sessionId":"s"});
    let size = delivery::json_bytes(&delta);
    d.deltas("sessions-index/w", (0, 1), &[(delta, size)], t0);
    assert_eq!(frames(&d.flush_due(t0).lines).len(), 1);
    subscribe(&mut d, "c", Profile::Continuous, &snapshot(0));
    let mut topics = d.close_connection("c");
    topics.sort();
    assert_eq!(
        topics,
        vec![TOPIC.to_owned(), "sessions-index/w".to_owned()]
    );
}

#[test]
fn backlog_invalidation_recovers_after_relief() {
    let mut d = Delivery::default();
    let t0 = Instant::now();
    let sub = subscribe(&mut d, "c", Profile::Continuous, &snapshot(0));
    d.deltas(TOPIC, (0, 1), &[text("a")], t0);
    d.invalidate_all();
    assert_eq!(d.next_due(), None);
    d.wake(t0);
    assert_eq!(d.flush_due(t0).recover, vec![(sub.id, TOPIC.to_owned())]);
}
