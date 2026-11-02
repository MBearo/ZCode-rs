#![expect(
    clippy::disallowed_methods,
    reason = "测试准备模型轨迹的目录、检查写出的文件"
)]

use std::time::{Duration, UNIX_EPOCH};

use serde_json::{Value, json};

use super::*;

/// 一条记录：`messages` 是请求体的消息，`error` 为真时带错误。
fn pending(session: Option<&str>, started_ms: u64, messages: &[Value], error: bool) -> Pending {
    let mut record = json!({
        "type": "model_io",
        "sessionId": session,
        "request": {"route": "messages", "body": {"model": "m", "messages": messages}},
        "outcome": if error { "failed" } else { "completed" },
    });
    if error {
        record["error"] = json!({"name": "server", "message": "boom"});
    }
    let Value::Object(record) = record else {
        unreachable!()
    };
    Pending {
        session: session.map(str::to_owned),
        started_ms,
        build: Box::new(move || record),
    }
}

fn messages(texts: &[&str]) -> Vec<Value> {
    texts
        .iter()
        .map(|text| json!({"role": "user", "content": [{"type": "text", "text": text}]}))
        .collect()
}

fn parse(line: &str) -> Value {
    assert!(line.ends_with('\n'));
    serde_json::from_str(line).unwrap()
}

fn kind(record: &Value) -> (Value, Value, Value, usize) {
    let request = &record["request"];
    (
        request["bodyMessagesKind"].clone(),
        request["bodyMessageOffset"].clone(),
        request["bodyMessageCount"].clone(),
        request["body"]["messages"].as_array().unwrap().len(),
    )
}

#[test]
fn file_names_follow_the_node_segment_rule() {
    assert_eq!(
        file_name(Some("sess_5b1c2f7e-9d4a")),
        "model-io-sess_5b1c2f7e-9d4a.jsonl"
    );
    assert_eq!(file_name(Some("a/b  c")), "model-io-a-b-c.jsonl");
    assert_eq!(file_name(Some("--x::y--")), "model-io-x-y.jsonl");
    assert_eq!(file_name(Some("会话:1")), "model-io-1.jsonl");
    assert_eq!(file_name(Some("///")), "model-io-no-session.jsonl");
    assert_eq!(file_name(None), "model-io-no-session.jsonl");
    let long = "x".repeat(100);
    assert_eq!(
        file_name(Some(&long)),
        format!("model-io-{}.jsonl", "x".repeat(80))
    );
}

#[test]
fn messages_are_full_then_delta_then_baseline_like_node() {
    // 第一条：完整。
    let (first, state) = line(
        pending(Some("s"), 10, &messages(&["a", "b", "c"]), false),
        None,
        Some(64),
        None,
        0,
    );
    assert_eq!(kind(&parse(&first)), (json!("full"), json!(0), json!(3), 3));
    // 接着的一条只写新增的两条。
    let (second, state) = line(
        pending(Some("s"), 20, &messages(&["a", "b", "c", "d", "e"]), false),
        Some(&state),
        Some(64),
        None,
        0,
    );
    let second = parse(&second);
    assert_eq!(kind(&second), (json!("delta"), json!(3), json!(5), 2));
    assert_eq!(
        second["request"]["body"]["messages"][0]["content"][0]["text"],
        "d"
    );
    // 缓存断点后移：上一条的最后一条消息这一条里不同，从相同的前缀之后写（offset 小于上一条的条数）。
    let marked = |texts: &[&str]| {
        let mut list = messages(texts);
        if let Some(last) = list.last_mut() {
            last["content"][0]["cache_control"] = json!({"type": "ephemeral"});
        }
        list
    };
    let (before, cached) = line(
        pending(Some("s"), 21, &marked(&["a", "b", "c"]), false),
        None,
        Some(64),
        None,
        0,
    );
    assert_eq!(
        kind(&parse(&before)),
        (json!("full"), json!(0), json!(3), 3)
    );
    let (moved, _) = line(
        pending(Some("s"), 22, &marked(&["a", "b", "c", "d"]), false),
        Some(&cached),
        Some(64),
        None,
        0,
    );
    let moved = parse(&moved);
    assert_eq!(kind(&moved), (json!("delta"), json!(2), json!(4), 2));
    assert!(
        moved["request"]["body"]["messages"][0]["content"][0]
            .get("cache_control")
            .is_none()
    );
    // 前缀不同（例如标题请求）：写基线。
    let (third, title) = line(
        pending(Some("s"), 30, &messages(&["title"]), false),
        Some(&state),
        Some(64),
        None,
        0,
    );
    assert_eq!(kind(&parse(&third)), (json!("full"), json!(0), json!(1), 1));
    // 上一条开始得比这一条晚（两个请求重叠，完成顺序与开始顺序不同）：不写 delta。
    let (overlap, _) = line(
        pending(Some("s"), 25, &messages(&["title", "x"]), false),
        Some(&title),
        Some(64),
        None,
        0,
    );
    assert_eq!(
        kind(&parse(&overlap)),
        (json!("full"), json!(0), json!(2), 2)
    );
    // 失败的请求总是完整的。
    let (failed, _) = line(
        pending(
            Some("s"),
            40,
            &messages(&["a", "b", "c", "d", "e", "f"]),
            true,
        ),
        Some(&state),
        Some(64),
        None,
        0,
    );
    assert_eq!(
        kind(&parse(&failed)),
        (json!("full"), json!(0), json!(6), 6)
    );
}

#[test]
fn a_long_baseline_keeps_the_tail_and_full_retention_keeps_everything() {
    let texts: Vec<String> = (0..70).map(|index| format!("m{index}")).collect();
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    let (tail, _) = line(
        pending(None, 1, &messages(&texts), false),
        None,
        Some(64),
        None,
        0,
    );
    let tail = parse(&tail);
    assert_eq!(kind(&tail), (json!("tail"), json!(6), json!(70), 64));
    assert_eq!(
        tail["request"]["body"]["messages"][0]["content"][0]["text"],
        "m6"
    );
    // 全量保留：不压缩，也不写压缩的键。
    let (full, state) = line(
        pending(None, 1, &messages(&texts), false),
        None,
        None,
        None,
        3,
    );
    let full = parse(&full);
    assert_eq!(
        full["request"]["body"]["messages"]
            .as_array()
            .unwrap()
            .len(),
        70
    );
    assert!(full["request"].get("bodyMessagesKind").is_none());
    assert_eq!(full["droppedRecords"], 3);
    // 状态照样更新：关掉全量保留后的下一条可以只写新增的。
    let mut more = texts.clone();
    more.push("m70");
    let (next, _) = line(
        pending(None, 2, &messages(&more), false),
        Some(&state),
        Some(64),
        None,
        0,
    );
    assert_eq!(
        kind(&parse(&next)),
        (json!("delta"), json!(70), json!(71), 1)
    );
}

fn touch(dir: &Path, name: &str, age_secs: u64) {
    let path = dir.join(name);
    std::fs::write(&path, "{}\n").unwrap();
    let when = UNIX_EPOCH + Duration::from_secs(1_700_000_000 - age_secs);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn production_keeps_three_files_and_deletes_the_oldest_first() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("rollout");
    std::fs::create_dir_all(&root).unwrap();
    for (name, age) in [
        ("model-io-old.jsonl", 40),
        ("model-io-older.jsonl", 50),
        ("model-io-new.jsonl", 10),
        ("model-io-mid.jsonl", 20),
    ] {
        touch(&root, name, age);
    }
    std::fs::write(root.join("other.jsonl"), "x").unwrap();
    let mut files = Files::new(root.clone(), false);
    files
        .write(pending(Some("s1"), 1, &messages(&["a"]), false), false, 0)
        .await;
    assert_eq!(
        names(&root),
        [
            "model-io-mid.jsonl",
            "model-io-new.jsonl",
            "model-io-s1.jsonl",
            "other.jsonl"
        ]
    );
    // 同一个会话的文件已在：不轮转，接着写。
    files
        .write(
            pending(Some("s1"), 2, &messages(&["a", "b"]), false),
            false,
            0,
        )
        .await;
    assert_eq!(names(&root).len(), 4);
    let text = std::fs::read_to_string(root.join("model-io-s1.jsonl")).unwrap();
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 2);
    assert_eq!(records[1]["request"]["bodyMessagesKind"], "delta");
    // 全量保留不轮转。
    files
        .write(pending(Some("s2"), 3, &messages(&["a"]), false), true, 0)
        .await;
    assert_eq!(names(&root).len(), 5);
}

#[tokio::test]
async fn development_does_not_rotate() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("debug");
    std::fs::create_dir_all(&root).unwrap();
    for (index, name) in ["a", "b", "c", "d"].iter().enumerate() {
        touch(&root, &format!("model-io-{name}.jsonl"), index as u64);
    }
    let mut files = Files::new(root.clone(), true);
    files
        .write(pending(Some("e"), 1, &messages(&["a"]), false), false, 0)
        .await;
    assert_eq!(names(&root).len(), 5);
}

#[tokio::test]
async fn a_file_at_the_cap_is_overwritten_with_a_baseline() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("rollout");
    let mut files = Files::new(root.clone(), false);
    files
        .write(pending(Some("s"), 1, &messages(&["a"]), false), false, 0)
        .await;
    // 文件到了 64 MiB（稀疏文件）：下一条不用上一条的状态，写成基线并覆盖整个文件。
    let path = root.join("model-io-s.jsonl");
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(PRODUCTION_CAP)
        .unwrap();
    files
        .write(
            pending(Some("s"), 2, &messages(&["a", "b"]), false),
            false,
            0,
        )
        .await;
    let text = std::fs::read_to_string(&path).unwrap();
    let records: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1, "{text}");
    assert_eq!(
        records[0]["modelIOReset"],
        json!({"maxFileBytes": PRODUCTION_CAP, "previousFileBytes": PRODUCTION_CAP,
               "reason": "session_file_size_limit"})
    );
    assert_eq!(records[0]["request"]["bodyMessagesKind"], "full");
    // 覆盖用的临时文件不留在目录里。
    assert_eq!(names(&root), ["model-io-s.jsonl"]);
}
