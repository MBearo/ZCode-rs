use super::*;

fn sample() -> FileTask {
    FileTask {
        id: "1a2b3c4d".to_owned(),
        cron: "*/5 * * * *".to_owned(),
        prompt: "check the deploy".to_owned(),
        created_at: 1_760_000_000_000,
        last_fired_at: None,
        recurring: true,
        permanent: false,
        created_by_session: Some("sess_a".to_owned()),
        created_by_pid: Some(4242),
        created_by_proc_start: Some("Sat Oct 18 01:02:03 2026".to_owned()),
        created_in_project: Some("/Users/dev/repo".to_owned()),
    }
}

/// 写出的形状：两格缩进、末尾换行、键序 id → createdInProject，可省略的字段不写。
#[test]
fn render_matches_the_file_layout() {
    let mut task = sample();
    task.last_fired_at = Some(1_760_000_300_000);
    let one_shot = FileTask {
        id: "5e6f7a8b".to_owned(),
        recurring: false,
        created_by_session: None,
        created_by_pid: None,
        created_by_proc_start: None,
        created_in_project: None,
        ..sample()
    };
    assert_eq!(
        render(&[task, one_shot]),
        r#"{
  "tasks": [
    {
      "id": "1a2b3c4d",
      "cron": "*/5 * * * *",
      "prompt": "check the deploy",
      "createdAt": 1760000000000,
      "lastFiredAt": 1760000300000,
      "recurring": true,
      "createdBySessionId": "sess_a",
      "createdByPid": 4242,
      "createdByProcStart": "Sat Oct 18 01:02:03 2026",
      "createdInProject": "/Users/dev/repo"
    },
    {
      "id": "5e6f7a8b",
      "cron": "*/5 * * * *",
      "prompt": "check the deploy",
      "createdAt": 1760000000000
    }
  ]
}
"#
    );
    assert_eq!(render(&[]), "{\n  \"tasks\": []\n}\n");
}

#[test]
fn parse_round_trips_and_skips_malformed_items() {
    let text = render(&[sample()]);
    assert_eq!(parse(&text), vec![sample()]);
    let mixed = r#"{"tasks": [
        {"id": 1, "cron": "* * * * *", "prompt": "p", "createdAt": 1},
        {"id": "a", "cron": "* * * * *", "prompt": "p"},
        {"id": "b", "cron": "* * * * *", "prompt": "p", "createdAt": 2.9, "recurring": 1, "permanent": "yes"},
        null
    ]}"#;
    let parsed = parse(mixed);
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].id, "b");
    assert_eq!(parsed[0].created_at, 2);
    assert!(parsed[0].recurring && parsed[0].permanent);
}

#[test]
fn missing_or_broken_files_have_no_tasks() {
    for raw in ["", "not json", "[]", "{\"tasks\": {}}", "{\"other\": []}"] {
        assert!(parse(raw).is_empty(), "{raw}");
    }
}
