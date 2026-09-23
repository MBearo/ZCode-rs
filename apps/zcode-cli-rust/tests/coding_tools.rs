use serde_json::json;
use tokio_util::sync::CancellationToken;
use zcode_cli_tools::tools::WorkspaceTools;

#[tokio::test]
async fn files_are_paged_fresh_and_session_scoped() {
    let root = tempfile::tempdir().unwrap();
    let tools = WorkspaceTools::new(root.path().into(), root.path().join("artifacts"));
    let c = CancellationToken::new();
    tools
        .call(
            "one",
            "Write",
            &json!({"file_path":"nested/a.txt","content":"a\r\nb\r\na\r\n"}),
            &c,
        )
        .await
        .unwrap();
    let out = tools
        .call(
            "two",
            "Read",
            &json!({"file_path":"nested/a.txt","offset":2,"limit":1}),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(out.data["startLine"], 2);
    assert!(out.content.ends_with("2\tb"));
    assert!(
        tools
            .call(
                "two",
                "Write",
                &json!({"file_path":"nested/a.txt","content":"oops"}),
                &c
            )
            .await
            .is_err()
    );
    tools.call("one", "Edit", &json!({"file_path":"nested/a.txt","old_string":"a","new_string":"z","replace_all":true}), &c).await.unwrap();
    assert_eq!(
        tokio::fs::read(root.path().join("nested/a.txt"))
            .await
            .unwrap(),
        b"z\r\nb\r\nz\r\n"
    );
    tokio::fs::write(root.path().join("nested/a.txt"), "external")
        .await
        .unwrap();
    assert!(
        tools
            .call(
                "one",
                "Edit",
                &json!({"file_path":"nested/a.txt","old_string":"external","new_string":"lost"}),
                &c
            )
            .await
            .is_err()
    );
    assert!(
        tools
            .call(
                "three",
                "Edit",
                &json!({"file_path":"nested/a.txt","old_string":"external","new_string":"lost"}),
                &c
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn search_pagination_and_regex_are_native() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::write(root.path().join("a.rs"), "hello\n世界\nhello again\n")
        .await
        .unwrap();
    tokio::fs::write(root.path().join("b.txt"), "hello")
        .await
        .unwrap();
    tokio::fs::write(root.path().join(".ignore"), "b.txt\n")
        .await
        .unwrap();
    let tools = WorkspaceTools::new(root.path().into(), root.path().join("artifacts"));
    let c = CancellationToken::new();
    let out = tools
        .call("s", "Glob", &json!({"pattern":"**/*.rs"}), &c)
        .await
        .unwrap();
    assert_eq!(out.data["filenames"], json!(["a.rs"]));
    let out = tools
        .call(
            "s",
            "Grep",
            &json!({"pattern":"hello","output_mode":"content","offset":1,"head_limit":1}),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(out.data["content"], "a.rs:3:hello again");
    let out = tools
        .call(
            "s",
            "Grep",
            &json!({"pattern":"hello\\n世界","multiline":true,"output_mode":"files_with_matches"}),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(out.data["filenames"], json!(["a.rs"]));
    assert!(
        tools
            .call("s", "Grep", &json!({"pattern":"["}), &c)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn background_registration_requires_commit_and_eof_reaps_processes() {
    if cfg!(windows) {
        return;
    }
    use tokio::sync::mpsc;
    use zcode_cli_core_api::{Event, EventSink, ToolPort};
    let root = tempfile::tempdir().unwrap();
    let tools = std::sync::Arc::new(WorkspaceTools::new(
        root.path().into(),
        root.path().join("artifacts"),
    ));
    let (tx, mut rx) = mpsc::channel(16);
    let sink = EventSink {
        session_id: "one".into(),
        run_id: "run".into(),
        tx,
    };
    let c = CancellationToken::new();
    let task = {
        let tools = tools.clone();
        let sink = sink.clone();
        let c = c.clone();
        tokio::spawn(async move {
            tools
                .execute_scoped(
                    "Bash",
                    &json!({"command":"echo unsafe > effect","run_in_background":true}),
                    &sink,
                    &c,
                )
                .await
        })
    };
    let event = rx.recv().await.unwrap();
    assert!(!root.path().join("effect").exists());
    match event.event {
        Event::Background { committed, .. } => drop(committed),
        _ => panic!("expected registration"),
    }
    assert!(task.await.unwrap().is_err());
    assert!(!root.path().join("effect").exists());
    match rx.recv().await.unwrap().event {
        Event::Background { task, committed } => {
            assert_eq!(task.status, "failed");
            assert!(committed.is_none());
        }
        _ => panic!("expected failed registration terminal state"),
    }
    let task = {
        let tools = tools.clone();
        let sink = sink.clone();
        tokio::spawn(async move {
            tools
                .execute_scoped(
                    "Bash",
                    &json!({"command":"echo $$ > pid; sleep 30","run_in_background":true}),
                    &sink,
                    &CancellationToken::new(),
                )
                .await
        })
    };
    match rx.recv().await.unwrap().event {
        Event::Background { committed, .. } => {
            committed.unwrap().send(()).unwrap();
        }
        _ => panic!(),
    }
    let started = task.await.unwrap().unwrap();
    assert_eq!(started.data["status"], "backgrounded");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        while tokio::fs::metadata(root.path().join("pid")).await.is_err() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let pid: i32 = tokio::fs::read_to_string(root.path().join("pid"))
        .await
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let shutdown = tokio::time::timeout(std::time::Duration::from_secs(3), tools.shutdown()).await;
    if shutdown.is_err() {
        #[cfg(unix)]
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        tools.shutdown().await.unwrap();
    }
    shutdown
        .expect("Shell shutdown waited for the descendant sleep instead of terminating it")
        .unwrap();
    #[cfg(unix)]
    assert_eq!(unsafe { libc::kill(-pid, 0) }, -1);
    #[cfg(unix)]
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    match rx.recv().await.unwrap().event {
        Event::Background { task, .. } => assert_eq!(task.status, "cancelled"),
        _ => panic!(),
    }
}

#[tokio::test]
async fn cancelled_background_registration_has_terminal_event_without_spawning() {
    use zcode_cli_core_api::{Event, EventSink, ToolPort};
    let root = tempfile::tempdir().unwrap();
    let tools = std::sync::Arc::new(WorkspaceTools::new(
        root.path().into(),
        root.path().join("artifacts"),
    ));
    let (tx, mut rx) = tokio::sync::mpsc::channel(16);
    let sink = EventSink {
        session_id: "s".into(),
        run_id: "r".into(),
        tx,
    };
    let cancel = CancellationToken::new();
    let running = {
        let tools = tools.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            tools
                .execute_scoped(
                    "Bash",
                    &json!({"command":"echo unsafe > effect", "run_in_background":true}),
                    &sink,
                    &cancel,
                )
                .await
        })
    };
    let (id, receipt) = match rx.recv().await.unwrap().event {
        Event::Background { task, committed } => (task.id, committed),
        _ => panic!("expected registration"),
    };
    cancel.cancel();
    assert!(running.await.unwrap().is_err());
    let event = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
        .await
        .unwrap()
        .unwrap();
    match event.event {
        Event::Background { task, committed } => {
            assert_eq!(task.id, id);
            assert_eq!(task.status, "cancelled");
            assert!(task.ended_at.is_some());
            assert!(committed.is_none());
        }
        _ => panic!("expected terminal state"),
    }
    drop(receipt);
    tools.close_session("s").await.unwrap();
    assert!(!root.path().join("effect").exists());
}

#[tokio::test]
async fn close_releases_only_its_session_file_observations() {
    use zcode_cli_core_api::ToolPort;
    let root = tempfile::tempdir().unwrap();
    let tools = WorkspaceTools::new(root.path().into(), root.path().join("artifacts"));
    tokio::fs::write(root.path().join("a.txt"), "one")
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    for id in ["closing", "other"] {
        tools
            .call(id, "Read", &json!({"file_path":"a.txt"}), &cancel)
            .await
            .unwrap();
    }
    tools.close_session("closing").await.unwrap();
    let edit = json!({"file_path":"a.txt", "old_string":"one", "new_string":"two"});
    assert!(tools.call("closing", "Edit", &edit, &cancel).await.is_err());
    assert!(tools.call("other", "Edit", &edit, &cancel).await.is_ok());
}

#[tokio::test]
async fn huge_unicode_read_and_shell_output_are_bounded() {
    if cfg!(windows) {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let tools = WorkspaceTools::new(root.path().into(), root.path().join("artifacts"));
    tokio::fs::write(root.path().join("large"), "汉".repeat(100_000))
        .await
        .unwrap();
    let c = CancellationToken::new();
    let read = tools
        .call("s", "Read", &json!({"file_path":"large"}), &c)
        .await
        .unwrap();
    assert!(read.content.len() < 70_000);
    assert_eq!(read.data["truncated"], true);
    let out = tools
        .call(
            "s",
            "Bash",
            &json!({"command":"head -c 100000 /dev/zero | tr '\\0' x"}),
            &c,
        )
        .await
        .unwrap();
    assert_eq!(out.data["stdoutTruncated"], true);
    assert_eq!(out.data["stdoutBytes"], 100000);
    assert!(out.content.len() < 55_000);
    assert_eq!(
        tokio::fs::metadata(out.data["persistedOutputPath"].as_str().unwrap())
            .await
            .unwrap()
            .len(),
        100000
    );
    assert!(
        tools
            .call(
                "s",
                "Bash",
                &json!({"command":"echo bad > invalid-timeout", "timeout":"not-a-number"}),
                &c
            )
            .await
            .is_err()
    );
    assert!(!root.path().join("invalid-timeout").exists());
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(
        tools
            .call(
                "s",
                "Write",
                &json!({"file_path":"never","content":"no"}),
                &cancelled
            )
            .await
            .is_err()
    );
    assert!(!root.path().join("never").exists());
}
