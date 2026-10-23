//! 本机跨会话通信，真实进程（F28，docs/specs/rust-cli/cross-session.md §6）：两个 `zcode -p` 共用一个临时 HOME，
//! 经登记处互相发现、经收件箱 socket 发送；接收方在下一个工具边界并入；模式不同时扣住并给发送方回执；对方退出后
//! 发送失败；重开会话后扣住的消息仍在。每个进程连自己的本地模型夹具。等待只用于测试判定。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读登记目录"
)]

mod sse;

use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sse::Reply;

/// 请求里全部文本（user / system 的文本块与 tool_result 的文本），按出现顺序连起来。
fn texts(request: &Value) -> String {
    let mut out = String::new();
    for message in request["messages"].as_array().into_iter().flatten() {
        match &message["content"] {
            Value::String(text) => out.push_str(text),
            Value::Array(blocks) => {
                for block in blocks {
                    if let Some(text) = block["text"].as_str() {
                        out.push_str(text);
                    }
                    match &block["content"] {
                        Value::String(text) => out.push_str(text),
                        Value::Array(parts) => {
                            for part in parts {
                                out.push_str(part["text"].as_str().unwrap_or_default());
                            }
                        }
                        _ => {}
                    }
                    out.push('\n');
                }
            }
            _ => {}
        }
        out.push('\n');
    }
    out
}

/// 最后一条 tool_result 的文本。
fn last_result(request: &Value) -> String {
    let mut found = String::new();
    for message in request["messages"].as_array().into_iter().flatten() {
        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"] != "tool_result" {
                continue;
            }
            found = match &block["content"] {
                Value::String(text) => text.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|part| part["text"].as_str())
                    .collect(),
                _ => String::new(),
            };
        }
    }
    found
}

fn call(id: &str, name: &str, input: &Value) -> String {
    sse::tools_reply(&[sse::tool_block(0, id, name, &input.to_string())])
}

/// ListAgents 结果里第一个同伴的名字（`  <名字> [<ref>]  ·  …`）。
fn first_peer(listing: &str) -> String {
    let row = listing
        .lines()
        .skip_while(|line| !line.starts_with("Peer sessions ("))
        .nth(1)
        .unwrap_or_else(|| panic!("no peer row in {listing:?}"));
    row.trim().split(' ').next().unwrap().to_owned()
}

struct Sandbox {
    _dir: tempfile::TempDir,
    home: PathBuf,
    /// 接收方 A 的工作目录。
    work: PathBuf,
    /// 发送方 B 的工作目录：与 A 分开，`-c` 才能找回 A 的会话（同一个会话库里按工作区找最近的会话）。
    work_b: PathBuf,
}

fn sandbox() -> Sandbox {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work, work_b) = (base.join("home"), base.join("work"), base.join("work-b"));
    for dir in [&home, &work, &work_b] {
        std::fs::create_dir_all(dir).unwrap();
    }
    Sandbox {
        _dir: dir,
        home,
        work,
        work_b,
    }
}

fn zcode(sandbox: &Sandbox, cwd: &Path, url: &str, args: &[&str], extra: &[(&str, &str)]) -> Child {
    let mut command = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"));
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("HOME", &sandbox.home)
        .env("USERPROFILE", &sandbox.home)
        .env("PATH", "/usr/bin:/bin")
        .env("SHELL", "/bin/bash")
        .env("ZCODE_DATA_BASE_DIR", &sandbox.home)
        .env("ZCODE_MODEL_BASE_URL", url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", "claude-opus-5-5")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra {
        command.env(key, value);
    }
    command.spawn().unwrap()
}

fn registry(home: &Path) -> PathBuf {
    home.join(".zcode").join("rust-cli").join("peers")
}

/// 等到登记目录里有 `count` 份登记（进程启动、绑定收件箱之后写）。
fn wait_registered(home: &Path, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let found = std::fs::read_dir(registry(home))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
                    .count()
            })
            .unwrap_or(0);
        if found >= count {
            return;
        }
        assert!(Instant::now() < deadline, "no registration under {home:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn finish(child: Child) -> String {
    let output = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "{stderr}");
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// 发送方的剧本：ListAgents，再按结果里的名字 SendMessage，之后 `rest`。
fn sender_script(rest: Vec<Reply>) -> Vec<Reply> {
    let mut script = vec![
        Reply::Fixed(call("toolu_list", "ListAgents", &json!({}))),
        Reply::From(Box::new(|body: &Value| {
            let name = first_peer(&last_result(body));
            call(
                "toolu_send",
                "SendMessage",
                &json!({"to": name, "summary": "say hello", "message": "hello from B"}),
            )
        })),
    ];
    script.extend(rest);
    script
}

const SLEEP: fn(&str, u32) -> Reply = |id, seconds| {
    Reply::Fixed(call(
        id,
        "Bash",
        &json!({"command": format!("sleep {seconds}"), "description": "wait"}),
    ))
};

#[test]
fn two_sessions_discover_each_other_and_deliver_at_the_next_tool_boundary() {
    let sandbox = sandbox();
    let fixture_a = sse::serve_with(vec![
        SLEEP("toolu_wait", 6),
        Reply::Fixed(sse::text_reply("A done")),
    ]);
    let a = zcode(
        &sandbox,
        &sandbox.work,
        &fixture_a.url,
        &[
            "-p",
            "wait",
            "--permission-mode",
            "dontAsk",
            "--allowedTools",
            "Bash",
        ],
        &[],
    );
    wait_registered(&sandbox.home, 1);
    let fixture_b = sse::serve_with(sender_script(vec![Reply::Fixed(sse::text_reply("B done"))]));
    let b = zcode(
        &sandbox,
        &sandbox.work_b,
        &fixture_b.url,
        &["-p", "send", "--permission-mode", "dontAsk"],
        &[],
    );
    assert_eq!(finish(b), "B done");
    assert_eq!(finish(a), "A done");

    let sent = fixture_b.main_requests();
    let listing = last_result(&sent[1]);
    assert!(listing.starts_with("This session is work-"), "{listing}");
    assert!(
        listing.contains(" — the name other sessions use to message it (it is not listed below; a message to it would be a message to yourself).\n\nPeer sessions (1):\n  work-"),
        "{listing}"
    );
    assert!(
        listing.contains("  ·  interactive  ·  busy  ·  started "),
        "{listing}"
    );
    let name = first_peer(&listing);
    let result = last_result(&sent[2]);
    assert_eq!(
        result,
        format!(
            "“say hello” → {name} (another ZCode session on this machine; in that session's inbox, not yet read by its ZCode — a [Cross-session delivery notice] follows if that session holds it (usually a different permission mode) or refuses it)"
        )
    );

    // 接收方：Bash 的工具边界并入，信封带发送方的地址、名字与模式类别。
    let received = fixture_a.main_requests();
    let text = texts(received.last().unwrap());
    assert!(
        text.contains("Another ZCode session sent a message while you were working:\n<cross-session-message from=\"uds:"),
        "{text}"
    );
    assert!(
        text.contains("\" from-mode=\"prompting\">\nhello from B\n</cross-session-message>\n\nThis came from another ZCode session — not typed by your user"),
        "{text}"
    );
    assert!(
        text.contains("(reply via SendMessage to the `from=` address)."),
        "{text}"
    );
    // 两个进程退出后登记都已删除。
    let left = std::fs::read_dir(registry(&sandbox.home)).unwrap().count();
    assert_eq!(left, 0);
}

#[test]
fn a_mode_mismatch_holds_the_message_and_the_sender_hears_held_then_expired() {
    let sandbox = sandbox();
    let fixture_a = sse::serve_with(vec![
        SLEEP("toolu_wait", 6),
        Reply::Fixed(sse::text_reply("A done")),
    ]);
    let a = zcode(
        &sandbox,
        &sandbox.work,
        &fixture_a.url,
        &[
            "-p",
            "wait",
            "--permission-mode",
            "dontAsk",
            "--allowedTools",
            "Bash",
        ],
        &[("ZCODE_USER_DIALOG_TIMEOUT_MS", "1500")],
    );
    wait_registered(&sandbox.home, 1);
    let fixture_b = sse::serve_with(sender_script(vec![
        SLEEP("toolu_after", 4),
        Reply::Fixed(sse::text_reply("B done")),
    ]));
    let b = zcode(
        &sandbox,
        &sandbox.work_b,
        &fixture_b.url,
        &["-p", "send", "--permission-mode", "bypassPermissions"],
        &[],
    );
    assert_eq!(finish(b), "B done");
    assert_eq!(finish(a), "A done");

    // 接收方：扣住，模型没有看到。
    let received = fixture_a.main_requests();
    assert!(!texts(received.last().unwrap()).contains("hello from B"));
    // 发送方：sleep 的工具边界并入两条回执（held、expired），各带前缀。
    let sent = fixture_b.main_requests();
    let text = texts(sent.last().unwrap());
    assert!(
        text.contains("[MESSAGE FROM NON-USER SOURCE - NOT USER INPUT]\n[Cross-session delivery notice] Your message to another session was held by that session (recipient: uds:"),
        "{text}"
    );
    assert!(
        text.contains("[Cross-session delivery notice] Your message to another session was not approved before expiry (recipient: uds:"),
        "{text}"
    );
    assert!(text.contains("; msg_id: "), "{text}");
}

#[test]
fn sending_after_the_peer_exits_reports_it_unreachable() {
    let sandbox = sandbox();
    let fixture_a = sse::serve_with(vec![
        SLEEP("toolu_wait", 3),
        Reply::Fixed(sse::text_reply("A done")),
    ]);
    let a = zcode(
        &sandbox,
        &sandbox.work,
        &fixture_a.url,
        &[
            "-p",
            "wait",
            "--permission-mode",
            "dontAsk",
            "--allowedTools",
            "Bash",
        ],
        &[],
    );
    wait_registered(&sandbox.home, 1);
    let fixture_b = sse::serve_with(vec![
        Reply::Fixed(call("toolu_list", "ListAgents", &json!({}))),
        SLEEP("toolu_gap", 6),
        Reply::From(Box::new(|body: &Value| {
            let listing = texts(body);
            let name = first_peer(&listing[listing.find("Peer sessions").unwrap()..]);
            call(
                "toolu_send",
                "SendMessage",
                &json!({"to": name, "summary": "late", "message": "are you there"}),
            )
        })),
        Reply::Fixed(sse::text_reply("B done")),
    ]);
    let b = zcode(
        &sandbox,
        &sandbox.work_b,
        &fixture_b.url,
        &[
            "-p",
            "send",
            "--permission-mode",
            "dontAsk",
            "--allowedTools",
            "Bash",
        ],
        &[],
    );
    assert_eq!(finish(a), "A done");
    assert_eq!(finish(b), "B done");
    let sent = fixture_b.main_requests();
    let listing = last_result(&sent[1]);
    let name = first_peer(&listing);
    assert_eq!(
        last_result(sent.last().unwrap()),
        format!(
            "No agent named '{name}' is reachable.\nUse ListAgents to see everyone you can message."
        )
    );
}

/// A 的会话库：这个工作区最新的会话。
fn with_latest<T>(
    sandbox: &Sandbox,
    work: impl AsyncFnOnce(&rust_cli_node_store::NodeStore, rust_cli_kernel_api::ids::SessionId) -> T,
) -> T {
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::EnvironmentId;
    use rust_cli_kernel_api::store::{SessionFilter, SessionStore};
    use rust_cli_paths::{StoragePaths, StorageSettings};
    use std::sync::Arc;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let paths =
            StoragePaths::resolve(&StorageSettings::default(), &sandbox.home, &sandbox.home);
        let clock = Arc::new(rust_cli_node_store::SystemClock::utc());
        let ids = Arc::new(rust_cli_node_store::NodeIds::new(clock));
        let store = rust_cli_node_store::NodeStore::new(paths, ids);
        let workspace = WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new(sandbox.work.to_string_lossy()),
        };
        let latest = store
            .list(SessionFilter {
                workspace,
                limit: 1,
            })
            .await
            .unwrap()
            .remove(0);
        work(&store, latest.id).await
    })
}

/// A 的会话库里这个工作区最新的会话的某种 `rust-cli/<kind>` entry（只读）。
fn entries(sandbox: &Sandbox, kind: &str) -> Vec<(String, Value)> {
    use rust_cli_kernel_api::store::SessionStore;
    with_latest(sandbox, async |store, session| {
        store.entries(&session, kind).await.unwrap()
    })
}

/// 把扣住的消息的到期时刻改成收到的时刻：等价于“扣住的时长已经过去”。到期时刻是数据（cross-session.md §2.4），
/// 测试这样拨时间，不靠真实时间的余量。
fn expire_now(sandbox: &Sandbox, msg_id: &str, mut data: Value) {
    use rust_cli_kernel_api::ids::OwnerEpoch;
    use rust_cli_kernel_api::store::{Fact, FactBatch, SessionStore};
    data["held"]["expiresAt"] = data["receivedAt"].clone();
    let fact = Fact::Entry {
        kind: "peer_message".to_owned(),
        id: msg_id.to_owned(),
        data,
    };
    with_latest(sandbox, async |store, session| {
        let batch = FactBatch {
            owner_epoch: OwnerEpoch::LOCAL,
            command: None,
            facts: vec![fact],
            turn: None,
        };
        store.commit(&session, batch).await.unwrap();
    });
}

/// 发送方等过期回执的一步：请求里已有它就结束，否则再睡 1 秒、到下一个工具边界再看（等的是回执本身，不是时长；
/// 剧本用完时夹具回 `done`，断言 `B done` 会失败）。
fn until_expired(step: usize) -> Reply {
    Reply::From(Box::new(move |body: &Value| {
        if texts(body).contains("was not approved before expiry (recipient: uds:") {
            sse::text_reply("B done")
        } else {
            call(
                &format!("toolu_poll_{step}"),
                "Bash",
                &json!({"command": "sleep 1", "description": "wait"}),
            )
        }
    }))
}

/// 修复线 F35：原来 A 扣住 4 s、自己 sleep 2 s 后退出，负载下 A 收尾超过 4 s，消息在 A 退出前按到期结算（产品行为
/// 正确，测试靠真实时间的余量）；B 也只靠 sleep 10 s 等回执。现在三处都等事实：A 等 B 的 SendMessage 有了结果
/// （A 已提交并扣住）才收尾；扣住的时长是 1 小时（A 活着时不会到期），A 退出后测试把库里的到期时刻拨到过去再重开；
/// B 一直轮询到过期回执出现。
#[test]
fn a_held_message_survives_reopening_the_session() {
    const HOUR_MS: u64 = 3_600_000;
    let sandbox = sandbox();
    // B 的 SendMessage 有了结果时建这个文件：发送方的结果在接收方提交并 ack 之后才返回（cross-session.md §2.4）。
    let sent = sandbox.home.parent().unwrap().join("sent");
    let wait_sent = format!("until [ -e '{}' ]; do sleep 0.05; done", sent.display());
    let fixture_a = sse::serve_with(vec![
        Reply::Fixed(call(
            "toolu_wait",
            "Bash",
            &json!({"command": wait_sent, "description": "wait"}),
        )),
        Reply::Fixed(sse::text_reply("A done")),
        Reply::Fixed(sse::text_reply("A again")),
    ]);
    let timeout = [("ZCODE_USER_DIALOG_TIMEOUT_MS", "3600000")];
    let a = zcode(
        &sandbox,
        &sandbox.work,
        &fixture_a.url,
        &[
            "-p",
            "wait",
            "--permission-mode",
            "dontAsk",
            "--allowedTools",
            "Bash",
        ],
        &timeout,
    );
    wait_registered(&sandbox.home, 1);
    let mut rest = vec![Reply::From(Box::new(move |_: &Value| {
        std::fs::write(&sent, "").unwrap();
        call(
            "toolu_poll_0",
            "Bash",
            &json!({"command": "sleep 1", "description": "wait"}),
        )
    }))];
    rest.extend((1..45).map(until_expired));
    let fixture_b = sse::serve_with(sender_script(rest));
    let b = zcode(
        &sandbox,
        &sandbox.work_b,
        &fixture_b.url,
        &[
            "-p",
            "send",
            "--permission-mode",
            "bypassPermissions",
            "--allowedTools",
            "Bash",
        ],
        &[],
    );
    assert_eq!(finish(a), "A done");
    // A 退出时还扣着：库里有扣住的消息、没有解决（退出时不按过期结算，按规则 9 保留，§5 D3）。到期时刻是
    // 收到的时刻加扣住的时长。
    let held = entries(&sandbox, "peer_message");
    assert_eq!(held.len(), 1);
    let (msg_id, data) = held.into_iter().next().unwrap();
    assert_eq!(data["held"]["cause"], "mode-mismatch");
    assert_eq!(
        data["held"]["expiresAt"].as_u64(),
        data["receivedAt"].as_u64().map(|at| at + HOUR_MS)
    );
    assert!(entries(&sandbox, "peer_resolved").is_empty());

    // 过了到期时刻再重开同一会话：恢复后按时钟判断已过期，结算并把回执发给还在跑的 B。
    expire_now(&sandbox, &msg_id, data);
    let again = zcode(
        &sandbox,
        &sandbox.work,
        &fixture_a.url,
        &["-c", "-p", "again", "--permission-mode", "dontAsk"],
        &timeout,
    );
    assert_eq!(finish(again), "A again");
    let resolved = entries(&sandbox, "peer_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0].1["outcome"], "expired");
    assert_eq!(finish(b), "B done");
    let text = texts(fixture_b.main_requests().last().unwrap());
    assert!(
        text.contains("was held by that session (recipient: uds:"),
        "{text}"
    );
    assert!(
        text.contains("was not approved before expiry (recipient: uds:"),
        "{text}"
    );
}
