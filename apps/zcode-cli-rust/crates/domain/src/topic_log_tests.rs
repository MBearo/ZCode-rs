use super::*;
use serde_json::json;

fn batch(n: usize) -> Deltas {
    sized(
        (0..n)
            .map(|i| json!({"op":"row.delta","rowId":i,"path":"text","append":"x"}))
            .collect(),
    )
}

#[test]
fn replay_starts_at_an_entry_boundary_of_the_current_epoch() {
    let mut log = TopicLog::conversation();
    log.record("e1", 0, 2, batch(2));
    log.record("e1", 2, 3, batch(1));
    assert_eq!(log.replay("e1", 0, 3).map(|d| d.len()), Some(3));
    assert_eq!(log.replay("e1", 2, 3).map(|d| d.len()), Some(1));
    assert_eq!(log.replay("e1", 3, 3), Some(vec![]), "aligned base");
    assert_eq!(log.replay("e1", 1, 3), None, "inside an entry");
    assert_eq!(log.replay("e1", 4, 3), None, "ahead of the topic");
    assert_eq!(log.replay("e1", 0, 5), None, "the log misses (3, 5]");
    assert_eq!(log.replay("e0", 0, 3), None, "another epoch");
}

#[test]
fn a_new_epoch_or_a_gap_restarts_the_log() {
    let mut log = TopicLog::conversation();
    log.record("e1", 0, 2, batch(2));
    log.record("e2", 0, 1, batch(1));
    assert_eq!(log.replay("e2", 0, 1).map(|d| d.len()), Some(1));
    log.record("e2", 5, 6, batch(1));
    assert_eq!(log.replay("e2", 0, 6), None, "(1, 5] was never recorded");
    assert_eq!(log.replay("e2", 5, 6).map(|d| d.len()), Some(1));
}

#[test]
fn eviction_moves_the_floor_by_entries_and_bytes() {
    let mut log = TopicLog::new(2, usize::MAX);
    for i in 0..3 {
        log.record("e", i, i + 1, batch(1));
    }
    assert_eq!(log.replay("e", 0, 3), None);
    assert_eq!(log.replay("e", 1, 3).map(|d| d.len()), Some(2));
    let one = batch(1).iter().map(|(_, s)| s).sum::<usize>();
    let mut small = TopicLog::new(100, one);
    small.record("e", 0, 1, batch(1));
    small.record("e", 1, 2, batch(1));
    assert_eq!(small.bytes(), one);
    assert_eq!(small.replay("e", 0, 2), None);
    assert_eq!(small.replay("e", 1, 2).map(|d| d.len()), Some(1));
    let mut huge = TopicLog::new(100, 1);
    huge.record("e", 0, 1, batch(1));
    assert_eq!(huge.bytes(), 0, "an entry over the cap is not kept");
    assert_eq!(huge.replay("e", 1, 1), Some(vec![]));
    assert_eq!(huge.replay("e", 0, 1), None);
}

#[test]
fn only_changed_top_level_keys_are_published() {
    let mut topic = ConversationTopic::default();
    let full = json!({"revision":1,"meta":{"title":"a"}});
    assert_eq!(topic.changed("e", full.clone()), Some(full.clone()));
    assert_eq!(topic.changed("e", full.clone()), None);
    assert_eq!(
        topic.changed("e", json!({"revision":2,"meta":{"title":"a"}})),
        Some(json!({"revision":2}))
    );
    assert_eq!(
        topic.changed("e2", json!({"revision":2,"meta":{"title":"a"}})),
        Some(json!({"revision":2,"meta":{"title":"a"}})),
        "a new epoch publishes the whole patch"
    );
    assert!(topic.bytes() > 0);
}
