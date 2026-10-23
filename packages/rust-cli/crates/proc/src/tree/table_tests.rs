// 复制自 rust/crates/proc/src/tree/table_tests.rs（feat/rs 79b6691c55）。
use super::*;

#[test]
fn snapshots_keep_owned_groups_and_reject_reused_identities() {
    let first = parse(
        "10 1 10 S Mon Sep 21 00:00:00 2026\n11 10 11 S Mon Sep 21 00:00:00 2026\n12 11 11 S Mon Sep 21 00:00:00 2026\n99 1 99 S Mon Sep 21 00:00:00 2026",
    );
    let mut tree = Tree::new(10, &first);
    assert_eq!(tree.processes.len(), 3);
    assert!(!tree.groups.contains_key(&99));
    let next = parse(
        "11 1 11 S Mon Sep 21 00:00:00 2026\n13 1 10 S Mon Sep 21 00:00:01 2026\n14 11 11 S Mon Sep 21 00:00:01 2026\n12 1 12 S Mon Sep 21 00:01:00 2026",
    );
    tree.refresh(&next);
    assert!(tree.processes.contains_key(&13));
    assert!(tree.processes.contains_key(&14));
    assert_ne!(tree.processes[&12], next[&12].started);
    assert!(!tree.groups.contains_key(&12));
    assert!(!tree.owns_group(11, &parse("11 1 11 S Mon Sep 21 00:02:00 2026")));
    assert!(tree.owns_group(10, &next));
}
