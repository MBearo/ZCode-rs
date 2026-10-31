//! 工作线 F90（docs/specs/rust-cli/tools-more.md §5.1、model.md §4.1、query.md §1.9）：真实 `zcode -p` 连本地夹具，
//! 主回复里有端点自己产生的服务端块——`server_tool_use`、引用它的非标准 `tool_result`（GLM 的写法）、类型不认识
//! 而且收不到结束的块，序号乱序。回合不失败：回复去掉服务端块后落库，工具照常执行，第二个主请求里没有服务端块，
//! 会话能冷读回来。修 bug 前：跳过的块留下空槽，流结束报 `stream ended with an unfinished block`；半截回复带着
//! `server_tool_use` 提交，node-store 报 `not implemented: node-store assistant server_tool_use block`，退出码 1。
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、起本地 HTTP 夹具、读回会话"
)]

mod sse;

use std::process::Stdio;
use std::sync::Arc;

use rust_cli_base::models::GLM_5_3_FLASH;
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_node_store::{NodeIds, NodeStore, SystemClock};
use rust_cli_paths::{StoragePaths, StorageSettings};
use serde_json::{Value, json};

/// 夹具的第一个主回复：GLM 在主回复里做了一次服务端调用。
fn reply_with_server_blocks(notes: &str) -> String {
    let block = |index: usize, content_block: Value| {
        sse::raw_event(
            "content_block_start",
            json!({"index": index, "content_block": content_block}),
        )
    };
    let stop = |index: usize| sse::raw_event("content_block_stop", json!({"index": index}));
    let text_delta = |index: usize, text: &str| {
        sse::raw_event(
            "content_block_delta",
            json!({"index": index, "delta": {"type": "text_delta", "text": text}}),
        )
    };
    let read_input = json!({"file_path": notes}).to_string();
    sse::reply_of(
        &[
            block(0, json!({"type": "text", "text": ""})),
            text_delta(0, "Looking."),
            stop(0),
            block(
                1,
                json!({"type": "server_tool_use", "id": "srvtoolu_g", "name": "tool_search_tool_regex",
                       "input": {}}),
            ),
            sse::raw_event(
                "content_block_delta",
                json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"query\":\"read\"}"}}),
            ),
            stop(1),
            // 乱序：index 3 先于 index 2。
            block(3, json!({"type": "text", "text": ""})),
            text_delta(3, "Then read."),
            stop(3),
            block(
                2,
                json!({"type": "tool_result", "tool_use_id": "srvtoolu_g",
                       "content": [{"type": "tool_reference", "tool_name": "Read"}]}),
            ),
            stop(2),
            // 内核没有形状的块，而且收不到结束。
            block(5, json!({"type": "glm_internal"})),
            sse::tool_block(4, "toolu_read", "Read", &read_input),
        ],
        "tool_use",
    )
}

#[tokio::test]
async fn server_blocks_in_a_main_reply_do_not_fail_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let (home, work) = (base.join("home"), base.join("work"));
    std::fs::create_dir_all(work.join(".git")).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    let notes = work.join("notes.txt");
    std::fs::write(&notes, "hello notes\n").unwrap();
    let fixture = sse::serve(vec![
        reply_with_server_blocks(&notes.to_string_lossy()),
        sse::text_reply("done"),
    ]);
    let output = rust_cli_proc::std_command(env!("CARGO_BIN_EXE_zcode-rs"))
        .args(["-p", "go"])
        .current_dir(&work)
        .env_clear()
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("PATH", "/usr/bin:/bin")
        .env("ZCODE_DATA_BASE_DIR", &home)
        .env("ZCODE_MODEL_BASE_URL", &fixture.url)
        .env("ZCODE_API_KEY", "test-key")
        .env("ZCODE_MODEL", GLM_5_3_FLASH)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "done");

    // 第二个主请求：assistant 里没有服务端块，Read 的结果在。
    let requests = fixture.main_requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    let messages = requests[1]["messages"].as_array().unwrap();
    for message in messages
        .iter()
        .filter(|message| message["role"] == "assistant")
    {
        for block in message["content"].as_array().into_iter().flatten() {
            assert!(
                block["type"] != "server_tool_use" && block["type"] != "tool_result",
                "{message}"
            );
        }
    }
    let text = requests[1].to_string();
    assert!(!text.contains("srvtoolu_g"), "{text}");
    assert!(
        text.contains("Looking.") && text.contains("Then read."),
        "{text}"
    );
    assert!(
        text.contains("toolu_read") && text.contains("hello notes"),
        "{text}"
    );

    // 落库：会话冷读得回来，没有服务端块。
    let paths = StoragePaths::resolve(&StorageSettings::default(), &home, &work);
    let ids = NodeIds::new(Arc::new(SystemClock::utc()));
    let store = NodeStore::new(paths, Arc::new(ids));
    let listed = store
        .list(rust_cli_kernel_api::store::SessionFilter {
            workspace: rust_cli_kernel_api::env::WorkspaceRef {
                environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
                root: rust_cli_kernel_api::env::EnvPath::new(work.to_string_lossy()),
            },
            limit: 1,
        })
        .await
        .unwrap();
    let loaded = store.load(&listed[0].id).await.unwrap();
    let stored = serde_json::to_string(&loaded.messages).unwrap();
    assert!(
        stored.contains("toolu_read") && !stored.contains("srvtoolu_g"),
        "{stored}"
    );
}
