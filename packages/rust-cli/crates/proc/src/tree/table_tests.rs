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

/// F98（remote-exec.md §25.3）：`/proc/<pid>/stat` 的一行：comm 里可以有空格与括号，从最后一个 `)` 之后数字段。
#[test]
fn a_proc_stat_line_gives_the_parent_the_group_and_the_start() {
    let line =
        "4242 (tail -f (x)) S 17 4242 17 0 -1 4194560 120 0 0 0 1 2 0 0 20 0 1 0 98765 1234 56 0";
    let (pid, process) = parse_stat(line).unwrap();
    assert_eq!(pid, 4242);
    assert_eq!(
        (process.parent, process.group, process.started.as_str()),
        (17, 4242, "98765")
    );
    assert!(!process.zombie);
    let (_, dead) = parse_stat("7 (sh) Z 1 7 7 0 -1 0 0 0 0 0 0 0 0 0 20 0 1 0 55 0 0").unwrap();
    assert!(dead.zombie);
    assert!(parse_stat("8 (cut").is_none());
    assert!(parse_stat("x (sh) S 1 1").is_none());
}

/// Linux 上进程表读 `/proc`（不依赖 `ps`）：快照里有本进程的子进程，父进程与进程组对。
#[cfg(target_os = "linux")]
#[tokio::test]
async fn the_linux_process_table_comes_from_proc() {
    let mut child = crate::command("/bin/sh")
        .args(["-c", "sleep 30"])
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let pid = i32::try_from(child.id().unwrap()).unwrap();
    let table = proc_table().await.unwrap();
    let me = i32::try_from(std::process::id()).unwrap();
    assert_eq!(table[&pid].parent, me);
    assert!(table.contains_key(&me));
    child.kill().await.unwrap();
}
